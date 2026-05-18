// AF_XDP socket creation and NIC binding for dnsmark.
// Receive-only path: dnsmark sends queries via regular UDP sockets and
// captures DNS responses (src_port=53) via AF_XDP.

#![allow(dead_code)]

use std::os::fd::RawFd;

use super::umem::{
    Umem, DescRing,
    SOL_XDP, XDP_RX_RING, XDP_TX_RING,
    XDP_PGOFF_RX_RING, XDP_PGOFF_TX_RING,
    RING_SIZE, SockaddrXdp,
    XDP_ZEROCOPY, XDP_COPY, XDP_USE_NEED_WAKEUP,
    get_rx_tx_offsets, mmap_desc_ring,
};

pub const AF_XDP: libc::c_int = 44;

pub struct XskSocket {
    pub fd:   RawFd,
    pub umem: Umem,
    pub rx:   DescRing,
    _tx:      DescRing, // required by kernel, unused (send via UDP sockets)
}

impl Drop for XskSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd); }
    }
}

/// Create one AF_XDP socket bound to `ifindex` queue `queue_id`.
/// Tries zero-copy (native driver) first, falls back to copy mode.
pub unsafe fn create_xsk_socket(
    ifindex:      u32,
    queue_id:     u32,
    use_zerocopy: bool,
) -> Result<XskSocket, String> {
    let fd = libc::socket(AF_XDP, libc::SOCK_RAW, 0);
    if fd < 0 {
        return Err(format!("socket(AF_XDP): {}", std::io::Error::last_os_error()));
    }

    let umem = Umem::new(fd).inspect_err(|_| { libc::close(fd); })?;

    for (opt, sz) in [(XDP_RX_RING, RING_SIZE), (XDP_TX_RING, RING_SIZE)] {
        let rc = libc::setsockopt(
            fd, SOL_XDP, opt,
            &sz as *const _ as *const libc::c_void,
            std::mem::size_of::<u32>() as libc::socklen_t,
        );
        if rc != 0 {
            libc::close(fd);
            return Err(format!("setsockopt ring ({opt}): {}", std::io::Error::last_os_error()));
        }
    }

    let (rx_off, tx_off) = get_rx_tx_offsets(fd)?;
    let rx = mmap_desc_ring(fd, XDP_PGOFF_RX_RING, &rx_off, RING_SIZE)
        .inspect_err(|_| { libc::close(fd); })?;
    let tx = mmap_desc_ring(fd, XDP_PGOFF_TX_RING, &tx_off, RING_SIZE)
        .inspect_err(|_| { libc::close(fd); })?;

    let bind_flags = XDP_USE_NEED_WAKEUP
        | if use_zerocopy { XDP_ZEROCOPY } else { XDP_COPY };

    let sa = SockaddrXdp {
        sxdp_family:         AF_XDP as u16,
        sxdp_flags:          bind_flags,
        sxdp_ifindex:        ifindex,
        sxdp_queue_id:       queue_id,
        sxdp_shared_umem_fd: 0,
    };
    let rc = libc::bind(
        fd,
        &sa as *const SockaddrXdp as *const libc::sockaddr,
        std::mem::size_of::<SockaddrXdp>() as libc::socklen_t,
    );
    if rc != 0 {
        libc::close(fd);
        return Err(format!(
            "bind AF_XDP (ifindex={ifindex}, q={queue_id}, zerocopy={use_zerocopy}): {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(XskSocket { fd, umem, rx, _tx: tx })
}

/// Number of RX queues on `iface`.
pub fn get_rx_queue_count(iface: &str) -> u32 {
    let path = format!("/sys/class/net/{iface}/queues");
    std::fs::read_dir(&path)
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("rx-"))
                .count() as u32
        })
        .unwrap_or(1)
        .max(1)
}

/// Interface name → kernel ifindex.
pub fn iface_index(name: &str) -> Option<u32> {
    let cname = std::ffi::CString::new(name).ok()?;
    let idx = unsafe { libc::if_nametoindex(cname.as_ptr()) };
    if idx == 0 { None } else { Some(idx) }
}

/// Find the network interface that routes traffic to `server`.
pub fn iface_for_server(server: std::net::IpAddr) -> Option<String> {
    // Loopback addresses bypass the main routing table — kernel uses lo directly.
    if server.is_loopback() {
        return Some("lo".to_string());
    }
    match server {
        std::net::IpAddr::V4(v4) => iface_for_ipv4(v4),
        std::net::IpAddr::V6(_)  => default_interface(),
    }
}

fn iface_for_ipv4(target: std::net::Ipv4Addr) -> Option<String> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    let target_u32 = u32::from(target).swap_bytes(); // fib_trie uses host-byte-order LE
    let mut best: Option<(u32, String)> = None;

    for line in content.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?.to_string();
        let dest = u32::from_str_radix(cols.next()?, 16).ok()?;
        let _ = cols.next(); // gateway
        let _ = cols.next(); // flags
        let _ = cols.next(); // refcnt
        let _ = cols.next(); // use
        let _ = cols.next(); // metric
        let mask = u32::from_str_radix(cols.next()?, 16).ok()?;

        if (target_u32 & mask) == (dest & mask) {
            let prefix_len = mask.count_ones();
            if best.is_none() || prefix_len > best.as_ref().unwrap().0 {
                best = Some((prefix_len, iface));
            }
        }
    }
    best.map(|(_, iface)| iface)
}

/// Default route interface from /proc/net/route.
pub fn default_interface() -> Option<String> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in content.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?.to_string();
        let dest  = cols.next()?;
        if dest == "00000000" {
            return Some(iface);
        }
    }
    None
}
