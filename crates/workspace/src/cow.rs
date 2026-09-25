use std::fs;
use std::io;
use std::path::Path;

/// Perform a Copy-on-Write (CoW) directory clone from `src` to `dst`.
///
/// - macOS (APFS): the whole directory is duplicated via `clonefile(2)` in
///   milliseconds without consuming additional disk blocks.
/// - Linux (Btrfs/XFS/ZFS): each file is cloned via `ioctl(FICLONE)` reflinks.
/// - Everywhere else (and when reflinks are unsupported): a pure-Rust recursive
///   copy that preserves file modes, modification times, and symlinks.
///
/// No external processes are spawned — this is all std/libc, per the
/// zero-external-binaries tenet.
pub fn cow_clone_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Source directory does not exist: {}", src.display()),
        ));
    }

    if dst.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("Destination directory already exists: {}", dst.display()),
        ));
    }

    // Attempt native macOS APFS clonefile syscall (clones the whole tree:
    // contents, modes, mtimes, symlinks) if compiled for macOS.
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        if let (Ok(c_src), Ok(c_dst)) = (
            CString::new(src.as_os_str().as_bytes()),
            CString::new(dst.as_os_str().as_bytes()),
        ) {
            let res = unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), 0) };
            if res == 0 {
                tracing::info!(
                    "Successfully cloned workspace via APFS clonefile: {} -> {}",
                    src.display(),
                    dst.display()
                );
                return Ok(());
            }
        }
    }

    // Pure-Rust recursive clone with per-file CoW where the filesystem
    // supports it. Replaces the former `cp --reflink=auto -a` / `cp -c -R`
    // subprocess invocations.
    recursive_clone(src, dst)
}

/// Recursively clone `src` into `dst` in pure Rust.
///
/// Per entry: `cow_clone_file` (clonefile → FICLONE → copy), plus explicit
/// preservation of permission modes and modification times. Symlinks are
/// recreated as symlinks. One unsupported file never fails the whole clone.
fn recursive_clone(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&src_path)?;
                let _ = fs::remove_file(&dst_path);
                if std::os::unix::fs::symlink(&target, &dst_path).is_err() {
                    // Best-effort: recreate the target's contents instead.
                    clone_entry(&src_path, &dst_path)?;
                }
            }
            #[cfg(windows)]
            {
                let _ = fs::copy(&src_path, &dst_path);
            }
        } else {
            clone_entry(&src_path, &dst_path)?;
        }
    }

    // Directory mtimes must be set after their children are written.
    let _ = clone_mtime(src, dst);
    Ok(())
}

/// Clone one filesystem entry (regular file or directory) with mode/mtime
/// preservation. Best-effort on errors that only affect metadata.
fn clone_entry(src: &Path, dst: &Path) -> io::Result<()> {
    let src_meta = fs::symlink_metadata(src)?;
    if src_meta.is_dir() {
        recursive_clone(src, dst)?;
    } else if src_meta.is_file() {
        cow_clone_file(src, dst)?;
        // FICLONE and fs::copy do not preserve modes or mtimes; do it
        // explicitly so branch workspaces look identical to their seed.
        let _ = clone_mode(src, dst);
        let _ = clone_mtime(src, dst);
    }
    Ok(())
}

/// Copy permission bits from `src` to `dst`.
#[cfg(unix)]
fn clone_mode(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(src)?.permissions().mode() & 0o7777;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
fn clone_mode(src: &Path, dst: &Path) -> io::Result<()> {
    let perms = fs::metadata(src)?.permissions();
    fs::set_permissions(dst, perms)
}

/// Copy the modification time from `src` to `dst` (best effort by callers).
fn clone_mtime(src: &Path, dst: &Path) -> io::Result<()> {
    let mtime = fs::metadata(src)?.modified()?;
    let f = fs::File::options().write(true).open(dst)?;
    f.set_modified(mtime)
}

/// Clone a single file using Copy-on-Write (CoW) or copying.
///
/// - macOS (APFS): `clonefile(2)` for instant 0-block duplication (preserves
///   modes and mtimes).
/// - Linux: `ioctl(FICLONE)` reflink on Btrfs/XFS/ZFS, with permission bits
///   copied explicitly (reflinks do not carry metadata).
/// - Windows/unsupported filesystems: plain copy.
///
/// The former hardlink fallback was removed deliberately: a hardlinked file
/// that is modified in place (formatter, `git checkout`, editor save) would
/// silently corrupt the shared CAS object or seed workspace behind it.
/// Copying is slower on non-reflink filesystems but keeps every copy
/// independent.
pub fn cow_clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Source file does not exist: {}", src.display()),
        ));
    }

    if dst.exists() {
        let _ = fs::remove_file(dst);
    } else if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }

    // 1. macOS: APFS clonefile(2)
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        if let (Ok(c_src), Ok(c_dst)) = (
            CString::new(src.as_os_str().as_bytes()),
            CString::new(dst.as_os_str().as_bytes()),
        ) {
            let res = unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), 0) };
            if res == 0 {
                return Ok(());
            }
        }
    }

    // 2. Linux: ioctl FICLONE
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        if let (Ok(src_file), Ok(dst_file)) = (
            fs::File::open(src),
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(dst),
        ) {
            // FICLONE is 0x40049409 on Linux
            let res =
                unsafe { libc::ioctl(dst_file.as_raw_fd(), 0x40049409, src_file.as_raw_fd()) };
            if res == 0 {
                // Reflinks clone contents only; carry permission bits over.
                let _ = clone_mode(src, dst);
                return Ok(());
            }
        }
        let _ = fs::remove_file(dst);
    }

    // 3. Fallback: standard file copy (preserves permission bits, not mtimes)
    fs::copy(src, dst)?;
    Ok(())
}

/// Extract base project name from a project key.
/// E.g. "my-repo:feature-1" -> Some("my-repo")
///      "my-repo/feature-1" -> Some("my-repo")
///      "my-repo__feature-1" -> Some("my-repo")
///      "my-repo" -> None
pub fn parse_base_project_name(project_name: &str) -> Option<&str> {
    if let Some((base, _)) = project_name.split_once(':') {
        if !base.is_empty() {
            return Some(base);
        }
    }
    if let Some((base, _)) = project_name.split_once('/') {
        if !base.is_empty() {
            return Some(base);
        }
    }
    if let Some((base, _)) = project_name.split_once("__") {
        if !base.is_empty() {
            return Some(base);
        }
    }
    None
}

/// Locate an existing seed/base workspace for a given project key if one exists.
pub fn find_seed_workspace(base_dir: &Path, project_name: &str) -> Option<std::path::PathBuf> {
    let base_name = parse_base_project_name(project_name)?;

    // Candidates in priority order
    let candidates = [
        crate::resolve_workspace_dir(base_dir, base_name),
        crate::resolve_workspace_dir(base_dir, &format!("{}:main", base_name)),
        crate::resolve_workspace_dir(base_dir, &format!("{}:master", base_name)),
        crate::resolve_workspace_dir(base_dir, &format!("{}__main", base_name)),
        crate::resolve_workspace_dir(base_dir, &format!("{}__master", base_name)),
        crate::resolve_workspace_dir(base_dir, &format!("{}/main", base_name)),
        crate::resolve_workspace_dir(base_dir, &format!("{}/master", base_name)),
    ];

    candidates.into_iter().find(|c| c.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_base_project_name() {
        assert_eq!(parse_base_project_name("my-repo:feat-1"), Some("my-repo"));
        assert_eq!(parse_base_project_name("my-repo/feat-1"), Some("my-repo"));
        assert_eq!(parse_base_project_name("my-repo__feat-1"), Some("my-repo"));
        assert_eq!(parse_base_project_name("my-repo"), None);
    }

    #[test]
    fn test_cow_clone_dir_roundtrip() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");

        fs::create_dir_all(src.join("subdir")).unwrap();
        fs::write(src.join("file1.txt"), "hello world").unwrap();
        fs::write(src.join("subdir/file2.txt"), "nested file").unwrap();

        cow_clone_dir(&src, &dst).unwrap();

        assert!(dst.join("file1.txt").is_file());
        assert!(dst.join("subdir/file2.txt").is_file());
        assert_eq!(
            fs::read_to_string(dst.join("file1.txt")).unwrap(),
            "hello world"
        );
        assert_eq!(
            fs::read_to_string(dst.join("subdir/file2.txt")).unwrap(),
            "nested file"
        );
    }

    #[test]
    fn test_cow_clone_preserves_mode_mtime_and_symlinks() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        // Executable file with a distinctive mtime.
        let script = src.join("run.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let old_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        {
            let f = fs::File::options().write(true).open(&script).unwrap();
            f.set_modified(old_time).unwrap();
        }

        // A symlink pointing at the script.
        #[cfg(unix)]
        std::os::unix::fs::symlink("run.sh", src.join("latest")).unwrap();

        cow_clone_dir(&src, &dst).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dst.join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755, "executable bit must survive the clone");
            assert!(fs::symlink_metadata(dst.join("latest"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                fs::read_link(dst.join("latest")).unwrap().to_string_lossy(),
                "run.sh"
            );
        }
        let cloned_mtime = fs::metadata(dst.join("run.sh"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(cloned_mtime, old_time);
    }

    #[test]
    fn test_cow_clone_files_are_independent_copies() {
        // The removed hardlink fallback used to alias the seed workspace and
        // the branch: modifying the clone corrupted the source. Copies must be
        // independent.
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("data.txt"), "original").unwrap();

        cow_clone_dir(&src, &dst).unwrap();

        // Simulate an in-place modification (formatter / git checkout).
        fs::write(dst.join("data.txt"), "modified in place").unwrap();

        assert_eq!(
            fs::read_to_string(src.join("data.txt")).unwrap(),
            "original",
            "seed workspace must not be corrupted by clone modifications"
        );
    }

    #[test]
    fn test_find_seed_workspace() {
        let temp = tempdir().unwrap();
        let base_dir = temp.path();

        // No seed exists yet
        assert_eq!(find_seed_workspace(base_dir, "my-repo:feat-1"), None);

        // Create seed for my-repo
        let seed = crate::resolve_workspace_dir(base_dir, "my-repo");
        fs::create_dir_all(&seed).unwrap();

        let found = find_seed_workspace(base_dir, "my-repo:feat-1");
        assert_eq!(found, Some(seed));
    }
}
