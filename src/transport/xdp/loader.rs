// Load the compiled XDP eBPF program, attach it to a NIC, and manage the
// XSKMAP that maps queue_id → AF_XDP socket fd.

use std::os::fd::RawFd;

use aya::{Ebpf, maps::XskMap, programs::{Xdp, XdpFlags}};

/// Compiled XDP program bytes, embedded at build time.
static XDP_PROG: &[u8] = include_bytes!(env!("XDP_BPF_OBJ"));

/// RAII handle — dropping this detaches the XDP program from the NIC.
pub struct XdpHandle {
    _bpf: Ebpf,
}

impl XdpHandle {
    /// Load, attach, setrlimit MEMLOCK, and return the handle.
    pub fn load(iface: &str) -> Result<Self, String> {
        // RLIMIT_MEMLOCK must be infinite for UMEM allocation.
        // For a CLI tool (not systemd), setrlimit works correctly.
        unsafe {
            let rl = libc::rlimit {
                rlim_cur: libc::RLIM_INFINITY,
                rlim_max: libc::RLIM_INFINITY,
            };
            if libc::setrlimit(libc::RLIMIT_MEMLOCK, &rl) != 0 {
                return Err(format!(
                    "setrlimit(RLIMIT_MEMLOCK): {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        // aya's ELF parser requires 8-byte alignment.
        let words = XDP_PROG.len().div_ceil(8);
        let mut storage: Vec<u64> = vec![0u64; words];
        unsafe {
            std::ptr::copy_nonoverlapping(
                XDP_PROG.as_ptr(),
                storage.as_mut_ptr() as *mut u8,
                XDP_PROG.len(),
            );
        }
        let aligned = unsafe {
            std::slice::from_raw_parts(storage.as_ptr() as *const u8, XDP_PROG.len())
        };

        let mut bpf = Ebpf::load(aligned)
            .map_err(|e| format!("BPF_PROG_LOAD: {e}"))?;

        let program: &mut Xdp = bpf
            .program_mut("dns_xdp_client")
            .ok_or_else(|| "dns_xdp_client section not found in ELF".to_string())?
            .try_into()
            .map_err(|e| format!("program type mismatch: {e}"))?;

        program.load().map_err(|e| format!("XDP prog load: {e}"))?;

        program
            .attach(iface, XdpFlags::DRV_MODE)
            .or_else(|_| program.attach(iface, XdpFlags::SKB_MODE))
            .map_err(|e| format!("XDP attach to {iface}: {e}"))?;

        Ok(XdpHandle { _bpf: bpf })
    }

    /// Register an AF_XDP socket in the XSKMAP at `queue_id`.
    pub fn register_socket(&mut self, queue_id: u32, sock_fd: RawFd) -> Result<(), String> {
        let map = self._bpf
            .map_mut("XSKS")
            .ok_or_else(|| "XSKS map not found in BPF object".to_string())?;

        let mut xsk_map = XskMap::try_from(map)
            .map_err(|e| format!("XSKS is not XskMap: {e}"))?;

        xsk_map
            .set(queue_id, sock_fd, 0)
            .map_err(|e| format!("XskMap::set q={queue_id}: {e}"))
    }
}
