use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[cfg(unix)]
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

    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let frsize = if stat.f_frsize > 0 {
        stat.f_frsize as u64
    } else {
        stat.f_bsize as u64
    };

    let available_bytes = stat.f_bavail as u64 * frsize;
    let total_bytes = stat.f_blocks as u64 * frsize;

    Ok(DiskSpace {
        available_bytes,
        total_bytes,
    })
}

#[cfg(windows)]
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
