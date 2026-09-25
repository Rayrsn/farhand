use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[cfg(unix)]
#[allow(unsafe_code)] // FFI: statvfs(3) — SAFETY contract inside the body.
pub fn get_disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Ensure directory exists or check its parent
    let target = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
        }
    } else {
        std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
    };

    let c_path = CString::new(target.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    // `statvfs` is a plain-old-data C struct of integers, so all-zero is a
    // valid bit pattern; we still go through MaybeUninit so the struct is
    // only ever assumed initialized after a successful syscall.
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c_path` is a live NUL-terminated CString and `stat` is a
    // writable, properly aligned slot for exactly the struct statvfs(3)
    // fills in. On success statvfs fully initializes the struct, which is
    // the only case where we call assume_init below.
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so the struct is fully initialized.
    let stat = unsafe { stat.assume_init() };

    // The statvfs block counters are u32 on some targets (macOS) and u64 on
    // others (Linux). `as_u64` is a real widening conversion on the former
    // and the identity on the latter, and (unlike a bare `as u64` or
    // `u64::from`) it is correct on every platform without tripping
    // clippy's same-type conversion lints on Linux.
    fn as_u64(v: impl Into<u64>) -> u64 {
        v.into()
    }

    let frsize = if stat.f_frsize > 0 {
        as_u64(stat.f_frsize)
    } else {
        as_u64(stat.f_bsize)
    };

    let available_bytes = as_u64(stat.f_bavail) * frsize;
    let total_bytes = as_u64(stat.f_blocks) * frsize;

    Ok(DiskSpace {
        available_bytes,
        total_bytes,
    })
}

#[cfg(windows)]
#[allow(unsafe_code)] // FFI: GetDiskFreeSpaceExW — SAFETY contract inside the body.
pub fn get_disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;

    let target = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
        }
    } else {
        std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
    };

    let mut wide_path: Vec<u16> = target.as_os_str().encode_wide().collect();
    wide_path.push(0);

    let mut free_bytes_available: u64 = 0;
    let mut total_number_of_bytes: u64 = 0;
    let mut total_number_of_free_bytes: u64 = 0;

    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    // SAFETY: `wide_path` is a NUL-terminated UTF-16 buffer (the trailing 0
    // is pushed above) and outlives the call, so `lpDirectoryName` is a valid
    // null-terminated wide string. The three output pointers address live,
    // writable `u64` locals, and GetDiskFreeSpaceExW is documented to fill
    // each of them (or fail without writing) — it never retains the pointers.
    let ret = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut free_bytes_available,
            &mut total_number_of_bytes,
            &mut total_number_of_free_bytes,
        )
    };

    if ret == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(DiskSpace {
        available_bytes: free_bytes_available,
        total_bytes: total_number_of_bytes,
    })
}

#[cfg(not(any(unix, windows)))]
pub fn get_disk_space(_path: &Path) -> std::io::Result<DiskSpace> {
    Ok(DiskSpace {
        available_bytes: u64::MAX,
        total_bytes: u64::MAX,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_space_query() {
        let cwd = std::env::current_dir().unwrap();
        let space = get_disk_space(&cwd).expect("get_disk_space should succeed");
        assert!(space.total_bytes > 0, "total disk space should be > 0");
        assert!(space.available_bytes <= space.total_bytes);
    }
}
