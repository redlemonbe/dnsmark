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

/// Best-effort: clear any XDP program already attached to `iface`, via netlink
/// (RTM_NEWLINK + IFLA_XDP{IFLA_XDP_FD = -1}). A program left behind by a
/// previously hard-killed run (SIGKILL never runs `XdpHandle::drop`) otherwise
/// wedges the next attach and silently breaks TX. Doing this automatically keeps
/// dnsmark out-of-the-box: the user never has to run `ip link set <if> xdp off`.
fn force_detach_xdp(iface: &str) {
    let ifindex = {
        let c = match std::ffi::CString::new(iface) { Ok(c) => c, Err(_) => return };
        unsafe { libc::if_nametoindex(c.as_ptr()) }
    };
    if ifindex == 0 { return; }

    const RTM_NEWLINK: u16 = 16;
    const NLM_F_REQUEST: u16 = 0x01;
    const NLM_F_ACK: u16 = 0x04;
    const IFLA_XDP: u16 = 43;
    const IFLA_XDP_FD: u16 = 1;
    const NLA_F_NESTED: u16 = 0x8000;

    let mut buf: Vec<u8> = Vec::with_capacity(48);
    // nlmsghdr (nlmsg_len patched in at the end)
    buf.extend_from_slice(&0u32.to_ne_bytes());
    buf.extend_from_slice(&RTM_NEWLINK.to_ne_bytes());
    buf.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK).to_ne_bytes());
    buf.extend_from_slice(&1u32.to_ne_bytes());            // seq
    buf.extend_from_slice(&0u32.to_ne_bytes());            // pid
    // ifinfomsg
    buf.push(libc::AF_UNSPEC as u8);                       // ifi_family
    buf.push(0);                                           // pad
    buf.extend_from_slice(&0u16.to_ne_bytes());            // ifi_type
    buf.extend_from_slice(&(ifindex as i32).to_ne_bytes());// ifi_index
    buf.extend_from_slice(&0u32.to_ne_bytes());            // ifi_flags
    buf.extend_from_slice(&0u32.to_ne_bytes());            // ifi_change
    // IFLA_XDP { IFLA_XDP_FD = -1 }
    let inner_len: u16 = 4 + 4;        // rtattr hdr + i32 fd
    let nest_len: u16 = 4 + inner_len; // nested hdr + inner
    buf.extend_from_slice(&nest_len.to_ne_bytes());
    buf.extend_from_slice(&(IFLA_XDP | NLA_F_NESTED).to_ne_bytes());
    buf.extend_from_slice(&inner_len.to_ne_bytes());
    buf.extend_from_slice(&IFLA_XDP_FD.to_ne_bytes());
    buf.extend_from_slice(&(-1i32).to_ne_bytes());

    let total = buf.len() as u32;
    buf[0..4].copy_from_slice(&total.to_ne_bytes());

    unsafe {
        let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE);
        if fd < 0 { return; }
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as u16;
        let _ = libc::sendto(
            fd, buf.as_ptr() as *const libc::c_void, buf.len(), 0,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        );
        let mut rbuf = [0u8; 512];
        let _ = libc::recv(fd, rbuf.as_mut_ptr() as *mut libc::c_void, rbuf.len(), 0);
        libc::close(fd);
    }
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

        // Clear any stale XDP program from a previously killed run before we
        // attach ours (otherwise the attach can succeed while TX stays wedged).
        force_detach_xdp(iface);

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
