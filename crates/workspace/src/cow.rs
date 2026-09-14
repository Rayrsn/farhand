use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use tracing::{info, warn};

/// Perform a Copy-on-Write (CoW) directory clone from `src` to `dst`.
///
/// On macOS (APFS), this leverages `clonefile(2)` or `cp -c -R` which duplicates
/// directories in milliseconds without consuming additional disk blocks.
/// On Linux, it attempts `cp --reflink=auto -a` for Btrfs/XFS/ZFS reflinks.
/// Falls back to standard recursive copy if CoW cloning is unsupported on the underlying filesystem.
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

    // Attempt native macOS APFS clonefile syscall if compiled for macOS
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
                info!(
                    "Successfully cloned workspace via APFS clonefile: {} -> {}",
                    src.display(),
                    dst.display()
                );
                return Ok(());
            }
        }
    }

    // Attempt platform-specific CoW copy commands
    #[cfg(target_os = "macos")]
    let status = Command::new("cp")
        .arg("-c")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .status();

    #[cfg(target_os = "linux")]
    let status = Command::new("cp")
        .arg("--reflink=auto")
        .arg("-a")
        .arg(src)
        .arg(dst)
        .status();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let status: io::Result<std::process::ExitStatus> = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "CoW not natively supported on this OS",
    ));

    if let Ok(st) = status {
        if st.success() {
            info!(
                "Successfully cloned workspace via system CoW copy: {} -> {}",
                src.display(),
                dst.display()
            );
            return Ok(());
        }
    }

    // Fallback: standard recursive copy
    warn!(
        "CoW cloning unavailable. Falling back to recursive file copy: {} -> {}",
        src.display(),
        dst.display()
    );
    recursive_copy(src, dst)
}

fn recursive_copy(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            recursive_copy(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&src_path)?;
                std::os::unix::fs::symlink(&target, &dst_path)?;
            }
            #[cfg(windows)]
            {
                let _ = fs::copy(&src_path, &dst_path);
            }
        }
    }
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
