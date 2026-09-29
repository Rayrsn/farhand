//! Per-file change tokens for the digest cache gate.
//!
//! The gate that decides whether a cached digest may be reused compares
//! `(size, mtime, change_token)`. Size and mtime alone are blind to a
//! same-size overwrite whose mtime was preserved — which is exactly what
//! `rsync -a`, `tar -p`, `cp -p`, and `robocopy` do. A change token is a value
//! the kernel advances on every modification and that userspace cannot set
//! back, so it turns that blind spot into a cache miss.
//!
//! This lives in `workspace` rather than `fileset` because obtaining the token
//! needs FFI on Windows, and `fileset` is `#![forbid(unsafe_code)]`. This
//! crate is `#![deny(unsafe_code)]` with narrow, justified exceptions, matching
//! the CoW and `statvfs` sites in `cow.rs` and `disk.rs`.

use std::path::Path;

/// Unix: the sub-second component of the inode change time.
///
/// The *nanosecond* part is the useful half, not an oversight. mtime already
/// pins the whole second on most filesystems, so the sub-second component is
/// effectively a per-write nonce: two writes landing in the same second almost
/// always differ here, which is precisely the case that matters. And ctime
/// cannot be set back by userspace, so it cannot be forged the way mtime can.
#[cfg(unix)]
pub fn change_token(_path: &Path, meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.ctime_nsec()
}

/// Windows: the NTFS change time from `GetFileInformationByHandleEx`.
///
/// `std::os::windows::fs::MetadataExt` exposes no change time on stable — only
/// creation and last-write times, and creation time does not move on an
/// in-place write — so the digest gate degrades to `(size, mtime)` on Windows
/// unless this is read directly.
///
/// `ChangeTime` is maintained by the filesystem and, unlike `LastWriteTime`,
/// cannot be set through `SetFileTime`, which is what makes it a usable
/// change token. It is a `LARGE_INTEGER` of 100-nanosecond intervals since
/// 1601, so it is returned verbatim: the value only has to be stable and
/// change on every write, never meaningful on its own.
///
/// The extra `CreateFileW` per file is deliberate and cheap. It requests only
/// `FILE_READ_ATTRIBUTES` with full sharing, so it neither blocks nor is
/// blocked by a concurrent reader or writer, and it costs a couple of
/// microseconds against the milliseconds a content re-hash costs.
#[cfg(windows)]
#[allow(unsafe_code)] // FFI: GetFileInformationByHandleEx to read the NTFS change time.
pub fn change_token(path: &Path, _meta: &std::fs::Metadata) -> i64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileBasicInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_NORMAL,
        FILE_BASIC_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // so the PCWSTR the callee reads is valid for the duration. The share mode
    // requests no write access, so this cannot block a concurrent writer nor
    // deny one. A null security-attributes pointer means default DACL, and
    // OPEN_EXISTING never creates or truncates. Every early return after this
    // closes the handle.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return 0;
    }

    // SAFETY: `FILE_BASIC_INFO` is a `#[repr(C)]` struct of plain integers,
    // for which all-zero is a valid bit pattern, and every field is
    // overwritten by the call below before anything reads it.
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };

    // SAFETY: `info` is a correctly sized, zero-initialised FILE_BASIC_INFO
    // and the length passed matches it, so the callee writes only within
    // `info`. `handle` was checked for null and INVALID_HANDLE_VALUE above and
    // is a live handle to a real file.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };

    // SAFETY: `handle` is a live, owned handle that is not used after this
    // point, so closing it exactly once is correct on every path.
    unsafe {
        CloseHandle(handle);
    }

    if ok == 0 {
        0
    } else {
        info.ChangeTime
    }
}

/// No change token available: the gate degrades to `(size, mtime)`, which is
/// the guarantee git makes. Correct, because a spurious miss only costs a
/// re-hash.
#[cfg(not(any(unix, windows)))]
pub fn change_token(_path: &Path, _meta: &std::fs::Metadata) -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// A same-size overwrite with the mtime rolled back is the case the token
    /// exists to catch, and `rsync -a` / `tar -p` produce exactly it.
    #[cfg(any(unix, windows))]
    #[test]
    fn token_moves_when_a_same_size_overwrite_preserves_the_mtime() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.bin");
        fs::write(&path, b"aaaa").unwrap();
        let original_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let first = change_token(&path, &fs::metadata(&path).unwrap());

        // Same length, and the mtime restored to its original value.
        fs::write(&path, b"bbbb").unwrap();
        restore_mtime(&path, original_mtime);

        let meta = fs::metadata(&path).unwrap();
        let second = change_token(&path, &meta);

        assert_eq!(
            meta.len(),
            4,
            "the fixture must keep the same size for this to mean anything"
        );
        assert_ne!(
            first, second,
            "the change token did not move for a modified file"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn token_is_stable_for_an_untouched_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.bin");
        fs::write(&path, b"stable").unwrap();

        let first = change_token(&path, &fs::metadata(&path).unwrap());
        let second = change_token(&path, &fs::metadata(&path).unwrap());

        assert_eq!(first, second, "an untouched file must not invalidate");
    }

    #[cfg(unix)]
    fn restore_mtime(path: &Path, mtime: std::time::SystemTime) {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    #[cfg(windows)]
    fn restore_mtime(path: &Path, mtime: std::time::SystemTime) {
        // SetFileTime can move LastWriteTime on Windows; that is the whole
        // point of the fixture, and it is the one thing a user *can* forge —
        // which is why the change time is read separately.
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(mtime).unwrap();
    }
}
