pub mod dot;
pub mod tcp;
pub mod udp;

pub use dot::run_dot_worker;
pub use tcp::run_tcp_worker;
pub use udp::run_udp_worker;

/// Pin the calling OS thread to CPU `worker_id % num_cpus`.
/// No-op on non-Linux targets.
pub fn pin_to_cpu(worker_id: usize) {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(worker_id % num_cpus::get(), &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = worker_id;
}
