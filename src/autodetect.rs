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

/// Returns the NUMA node of a logical CPU ID by reading sysfs topology links.
/// `/sys/devices/system/cpu/cpuN/node*` — the matching symlink gives the node.
pub fn numa_node_for_cpu(cpu_id: usize) -> Option<usize> {
    let dir = format!("/sys/devices/system/cpu/cpu{}", cpu_id);
    for entry in std::fs::read_dir(&dir).ok()? {
        let name = entry.ok()?.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("node") {
            if let Ok(n) = s[4..].parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

/// Returns the NUMA node closest to a network interface's PCIe slot.
/// Reads `/sys/class/net/<iface>/device/numa_node`.
/// Returns `None` for loopback, virtual interfaces, or single-NUMA systems.
pub fn numa_node_for_iface(iface: &str) -> Option<usize> {
    let path = format!("/sys/class/net/{}/device/numa_node", iface);
    let s = std::fs::read_to_string(&path).ok()?;
    let n: i32 = s.trim().parse().ok()?;
    if n < 0 { None } else { Some(n as usize) }
}

/// Find the network interface that routes to `addr` via `/proc/net/route`.
/// Returns the default-route interface if no specific route matches.
pub fn iface_for_addr(addr: std::net::IpAddr) -> Option<String> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    // loopback — always lo, no useful NUMA info
    if addr.is_loopback() { return None; }
    let v4 = match addr {
        std::net::IpAddr::V4(a) => u32::from(a),
        _ => return None, // IPv6: skip NUMA lookup
    };
    let mut default_iface: Option<String> = None;
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 { continue; }
        let iface = cols[0];
        let dest = u32::from_be(u32::from_str_radix(cols[1], 16).unwrap_or(u32::MAX));
        let mask = u32::from_be(u32::from_str_radix(cols[7], 16).unwrap_or(0));
        if dest == 0 && mask == 0 {
            default_iface = Some(iface.to_string());
            continue;
        }
        if (v4 & mask) == (dest & mask) {
            return Some(iface.to_string());
        }
    }
    default_iface
}

/// Physical cores sorted by NUMA locality to `preferred_node`.
/// NUMA-local cores come first; remote cores follow.
/// Falls back to `physical_cores()` when NUMA info is unavailable.
pub fn physical_cores_numa_sorted(preferred_node: Option<usize>) -> Vec<usize> {
    let cores = physical_cores();
    let Some(preferred) = preferred_node else { return cores; };
    let mut local: Vec<usize> = Vec::new();
    let mut remote: Vec<usize> = Vec::new();
    for &cpu in &cores {
        match numa_node_for_cpu(cpu) {
            Some(n) if n == preferred => local.push(cpu),
            _ => remote.push(cpu),
        }
    }
    local.extend(remote);
    local
}
