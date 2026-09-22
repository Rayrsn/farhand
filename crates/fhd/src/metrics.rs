use std::path::Path;

pub fn get_cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(unix)]
pub fn get_load_averages() -> Option<[f64; 3]> {
    let mut loads = [0.0f64; 3];
    let ret = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    if ret == 3 {
        Some(loads)
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn get_load_averages() -> Option<[f64; 3]> {
    None
}

pub fn get_memory_info() -> (Option<u64>, Option<u64>) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            let mut total_kb = None;
            let mut avail_kb = None;
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    total_kb = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok());
                } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    avail_kb = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok());
                }
            }
            if let (Some(t), Some(a)) = (total_kb, avail_kb) {
                let total = t * 1024;
                let used = total.saturating_sub(a * 1024);
                return (Some(used), Some(total));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        unsafe {
            let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
            let mut size: u64 = 0;
            let mut len = std::mem::size_of::<u64>();
            if libc::sysctl(
                mib.as_mut_ptr(),
                2,
                &mut size as *mut u64 as *mut _,
                &mut len,
                std::ptr::null_mut(),
                0,
            ) == 0
            {
                return (None, Some(size));
            }
        }
    }

    #[cfg(windows)]
    {
        #[repr(C)]
        #[allow(non_snake_case)]
        struct MEMORYSTATUSEX {
            dwLength: u32,
            dwMemoryLoad: u32,
            ullTotalPhys: u64,
            ullAvailPhys: u64,
            ullTotalPageFile: u64,
            ullAvailPageFile: u64,
            ullTotalVirtual: u64,
            ullAvailVirtual: u64,
            ullAvailExtendedVirtual: u64,
        }

        extern "system" {
            fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> i32;
        }

        let mut status = std::mem::MaybeUninit::<MEMORYSTATUSEX>::uninit();
        unsafe {
            let ptr = status.as_mut_ptr();
            (*ptr).dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(ptr) != 0 {
                let s = status.assume_init();
                let total = s.ullTotalPhys;
                let avail = s.ullAvailPhys;
                let used = total.saturating_sub(avail);
                return (Some(used), Some(total));
            }
        }
    }

    (None, None)
}

pub fn get_workspaces_count(workdir: &Path) -> Option<usize> {
    if let Ok(entries) = std::fs::read_dir(workdir) {
        let count = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                if let Ok(ft) = e.file_type() {
                    if ft.is_dir() {
                        let name = e.file_name();
                        let s = name.to_string_lossy();
                        return s != "cas" && !s.starts_with('.');
                    }
                }
                false
            })
            .count();
        Some(count)
    } else {
        None
    }
}
