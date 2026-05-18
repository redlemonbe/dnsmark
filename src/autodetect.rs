#[allow(dead_code)]
pub struct AutoConfig {
    pub cpus: usize,
    pub mem_mb: u64,
    pub xdp_available: bool,
}

pub fn detect() -> AutoConfig {
    let cpus = num_cpus::get();
    let mem_mb = read_proc_meminfo_avail_mb();
    let xdp_available = probe_xdp_support();
    AutoConfig { cpus, mem_mb, xdp_available }
}

/// Returns the logical CPU IDs of physical cores only — one per physical
/// core_id, selected by reading /sys topology. HT siblings are excluded.
///
/// Falls back to `0..num_cpus::get_physical()` if /sys is unavailable.
pub fn physical_cores() -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut cores = Vec::new();

    for cpu_id in 0..num_cpus::get() * 2 {
        let path = format!(
            "/sys/devices/system/cpu/cpu{}/topology/core_id",
            cpu_id
        );
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(core_id) = s.trim().parse::<usize>() {
                if seen.insert(core_id) {
                    cores.push(cpu_id);
                }
            }
        }
    }

    if cores.is_empty() {
        cores = (0..num_cpus::get_physical()).collect();
    }
    cores
}

fn read_proc_meminfo_avail_mb() -> u64 {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            return kb / 1024;
        }
    }
    0
}

pub fn read_proc_meminfo_total_and_avail_kb() -> (u64, u64) {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    (total, avail)
}

fn probe_xdp_support() -> bool {
    // AF_XDP = 44 on Linux
    let fd = unsafe { libc::socket(44, libc::SOCK_RAW, 0) };
    if fd >= 0 {
        unsafe { libc::close(fd) };
        true
    } else {
        false
    }
}
