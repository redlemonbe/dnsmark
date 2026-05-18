pub mod dot;
pub mod tcp;
pub mod udp;

pub use dot::run_dot_worker;
pub use tcp::run_tcp_worker;
pub use udp::run_udp_worker;

/// Pin the calling OS thread to a physical core (HT siblings excluded).
/// The core list is computed once and cached. No-op on non-Linux targets.
pub fn pin_to_cpu(worker_id: usize) {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        static CORES: OnceLock<Vec<usize>> = OnceLock::new();
        let cores = CORES.get_or_init(crate::autodetect::physical_cores);
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
