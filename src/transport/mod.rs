pub mod dot;
pub mod tcp;
pub mod udp;
#[cfg(feature = "xdp")]
pub mod xdp;

pub use dot::run_dot_worker;
pub use tcp::run_tcp_worker;
pub use udp::run_udp_worker;

#[cfg(target_os = "linux")]
static PINNED_CORES: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();

/// Initialize CPU pinning with NUMA awareness.
/// Call once at startup, after tracing is initialized, before spawning workers.
/// `server` is used to detect the egress NIC and its NUMA node.
pub fn init_cpu_pinning(server: std::net::IpAddr) {
    #[cfg(target_os = "linux")]
    {
        PINNED_CORES.get_or_init(|| {
            let iface = crate::autodetect::iface_for_addr(server);
            let numa_node = iface.as_deref().and_then(crate::autodetect::numa_node_for_iface);
            let cores = crate::autodetect::physical_cores_numa_sorted(numa_node);
            let total = cores.len();
            match (iface.as_deref(), numa_node) {
                (Some(iface), Some(node)) => {
                    let local = cores.iter()
                        .filter(|&&c| crate::autodetect::numa_node_for_cpu(c) == Some(node))
                        .count();
                    tracing::info!(
                        "[CPU] NUMA pinning: NIC {} on node {}, {}/{} cores NUMA-local",
                        iface, node, local, total
                    );
                }
                (Some(iface), None) => {
                    tracing::info!(
                        "[CPU] CPU pinning: {} — single-NUMA, {} physical cores",
                        iface, total
                    );
                }
                _ => {
                    tracing::info!(
                        "[CPU] CPU pinning: {} physical cores (no NUMA iface detected)",
                        total
                    );
                }
            }
            cores
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = server;
}

/// Pin the calling OS thread to a physical core (HT siblings excluded).
/// Uses the NUMA-sorted core list from `init_cpu_pinning()` if called first,
/// otherwise falls back to plain physical core order.
/// No-op on non-Linux targets.
pub fn pin_to_cpu(worker_id: usize) {
    #[cfg(target_os = "linux")]
    {
        let cores = PINNED_CORES.get_or_init(crate::autodetect::physical_cores);
        let cpu_id = cores[worker_id % cores.len()];
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(cpu_id, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = worker_id;
}
