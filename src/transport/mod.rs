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

/// Auto-NUMA: confine the whole process to the NIC's NUMA node — CPUs and memory —
/// so the user never needs `numactl --cpunodebind=N --membind=N`. Without this, a
/// 20-worker auto run on a 2-node host spreads half its TX threads onto the remote
/// node (QPI-bound, slower egress); the manual numactl recovered it. This makes it
/// automatic. SINGLE-NIC ONLY: with multiple NICs on different nodes, confining to one
/// node would starve the others — there each stack must pin to its own NIC's node.
#[cfg(target_os = "linux")]
pub fn confine_to_nic_node(server: std::net::IpAddr) {
    let Some(iface) = crate::autodetect::iface_for_addr(server) else { return; };
    let Some(node)  = crate::autodetect::numa_node_for_iface(&iface) else { return; };

    // CPUs: restrict process affinity to all logical CPUs of the NIC node (= --cpunodebind).
    let cpus = crate::autodetect::logical_cpus_for_node(node);
    if !cpus.is_empty() {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            for &c in &cpus { if c < libc::CPU_SETSIZE as usize { libc::CPU_SET(c, &mut set); } }
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        }
    }
    // Memory: bind allocations to the NIC node (= --membind). MPOL_BIND, single-node mask.
    // Set on the main thread before any worker spawns → child threads inherit the policy.
    if node < 64 {
        let nodemask: u64 = 1u64 << node;
        const MPOL_BIND: libc::c_int = 2;
        unsafe {
            libc::syscall(libc::SYS_set_mempolicy, MPOL_BIND,
                &nodemask as *const u64, 64u64);
        }
    }
    tracing::info!(
        "[CPU] auto-NUMA: process confined to NIC node {} ({} logical CPUs) + membind \
         — no numactl needed", node, cpus.len()
    );
}

#[cfg(not(target_os = "linux"))]
pub fn confine_to_nic_node(_server: std::net::IpAddr) {}

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
