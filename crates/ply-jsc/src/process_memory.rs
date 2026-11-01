//! Current memory use of the whole process for the runtime's watchdog.

/// Physical footprint, as reported by Activity Monitor.
#[cfg(target_os = "macos")]
pub(crate) fn footprint() -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, (&mut info as *mut libc::rusage_info_v2).cast())
    };
    if ok == 0 { info.ri_phys_footprint } else { 0 }
}

/// Current resident memory; ru_maxrss would retain the historical peak.
#[cfg(target_os = "linux")]
pub(crate) fn footprint() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else { return 0 };
    let Some(pages) = statm.split_whitespace().nth(1).and_then(|n| n.parse::<u64>().ok()) else { return 0 };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size > 0 { pages.saturating_mul(page_size as u64) } else { 0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_current_process_memory() {
        assert!(super::footprint() > 0, "memory watchdog must observe the running process");
    }
}
