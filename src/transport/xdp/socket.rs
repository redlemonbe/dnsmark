// AF_XDP socket creation and NIC binding for dnsmark.
// Receive-only path: dnsmark sends queries via regular UDP sockets and
// captures DNS responses (src_port=53) via AF_XDP.

#![allow(dead_code)]

use std::os::fd::RawFd;

use super::umem::{
    Umem, DescRing, AddrRing,
    SOL_XDP, XDP_RX_RING, XDP_TX_RING,
    XDP_PGOFF_RX_RING, XDP_PGOFF_TX_RING,
    RING_SIZE, SockaddrXdp,
    XDP_ZEROCOPY, XDP_COPY, XDP_USE_NEED_WAKEUP,
    get_rx_tx_offsets, mmap_desc_ring,
};

pub const AF_XDP: libc::c_int = 44;

pub struct XskSocket {
    pub fd:      RawFd,
    pub umem:    Umem,
    pub rx:      DescRing,
    pub tx:      DescRing,
    pub tx_pool: Vec<u64>,
}

impl XskSocket {
    /// Extract TX ring, completion ring, and frame pool before moving the
    /// socket into the RX receiver thread.  Leaves zeroed stubs in their
    /// place so the receiver thread only uses rx + umem.fill.
    pub fn extract_tx(&mut self) -> (DescRing, AddrRing, Vec<u64>) {
        let tx   = std::mem::replace(&mut self.tx,       DescRing::zeroed());
        let comp = std::mem::replace(&mut self.umem.comp, AddrRing::zeroed());
        let pool = std::mem::take(&mut self.tx_pool);
        (tx, comp, pool)
    }
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

    let (umem, tx_pool) = Umem::new(fd).inspect_err(|_| { libc::close(fd); })?;

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

    Ok(XskSocket { fd, umem, rx, tx, tx_pool })
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
/// Tries `getifaddrs()` subnet match first (accurate on multi-interface hosts
/// such as Proxmox where several interfaces share the same /24), then falls
/// back to the routing table.
pub fn iface_for_server(server: std::net::IpAddr) -> Option<String> {
    if server.is_loopback() {
        return Some("lo".to_string());
    }
    match server {
        std::net::IpAddr::V4(v4) => {
            getifaddrs_iface_for_subnet(v4)
                .or_else(|| {
                    let iface = iface_for_ipv4(v4)?;
                    tracing::debug!(
                        iface = %iface,
                        "XDP: interface selected via routing table"
                    );
                    Some(iface)
                })
        }
        std::net::IpAddr::V6(_) => {
            let iface = default_interface()?;
            tracing::debug!(iface = %iface, "XDP: interface selected via routing table");
            Some(iface)
        }
    }
}

/// Use `getifaddrs()` to find the first non-loopback interface whose IPv4
/// address lies in the same subnet as `server`.
fn getifaddrs_iface_for_subnet(server: std::net::Ipv4Addr) -> Option<String> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return None;
    }

    let server_u32 = u32::from(server);
    let mut result: Option<String> = None;
    let mut cur = ifap;

    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_netmask.is_null() { continue; }

        let family = unsafe { (*ifa.ifa_addr).sa_family } as libc::c_int;
        if family != libc::AF_INET { continue; }

        let (iface_u32, mask_u32) = unsafe {
            let addr = &*(ifa.ifa_addr    as *const libc::sockaddr_in);
            let mask = &*(ifa.ifa_netmask as *const libc::sockaddr_in);
            (u32::from_be(addr.sin_addr.s_addr), u32::from_be(mask.sin_addr.s_addr))
        };

        // Skip loopback and /0 masks (no meaningful subnet).
        if mask_u32 == 0 { continue; }
        let name = match unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_str() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name == "lo" { continue; }

        if (iface_u32 & mask_u32) == (server_u32 & mask_u32) {
            tracing::debug!(
                iface = %name, server = %server,
                "XDP: interface selected via getifaddrs() for subnet of server IP"
            );
            result = Some(name.to_owned());
            break;
        }
    }
    unsafe { libc::freeifaddrs(ifap); }
    result
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
            tracing::debug!(iface = %iface, "XDP: interface selected via routing table");
            return Some(iface);
        }
    }
    None
}

/// Returns `true` if `iface` is a virtual interface (bridge, bond, veth,
/// ipvlan, macvlan, tun/tap). Physical NICs expose
/// `/sys/class/net/<iface>/device`. VLAN sub-interfaces (`eth0.10`) have
/// `DEVTYPE=vlan` in their uevent and are treated as physical (XDP-capable).
pub fn is_virtual_interface(iface: &str) -> bool {
    if std::path::Path::new(&format!("/sys/class/net/{iface}/device")).exists() {
        return false;
    }
    let uevent = std::fs::read_to_string(format!("/sys/class/net/{iface}/uevent"))
        .unwrap_or_default();
    if uevent.lines().any(|l| l.trim() == "DEVTYPE=vlan") {
        return false;
    }
    true
}

/// Try to find a physical parent of a virtual interface.
/// Search order: `lower_*` sysfs entries (ipvlan/macvlan) →
/// `master` symlink (bond slave / bridge port) →
/// `brif/` directory (ports of a bridge).
pub fn parent_interface(iface: &str) -> Option<String> {
    let sysfs = format!("/sys/class/net/{iface}");
    // 1. lower_* entries (ipvlan, macvlan)
    if let Ok(entries) = std::fs::read_dir(&sysfs) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let name  = fname.to_string_lossy();
            if let Some(lower) = name.strip_prefix("lower_") {
                if !lower.is_empty() {
                    return Some(lower.to_string());
                }
            }
        }
    }
    // 2. master symlink (bond slave / bridge port)
    let master_path = format!("{sysfs}/master");
    if let Ok(target) = std::fs::read_link(&master_path) {
        if let Some(fname) = target.file_name() {
            let master = fname.to_string_lossy().into_owned();
            if !master.is_empty() {
                if !is_virtual_interface(&master) {
                    return Some(master);
                }
                if let Some(port) = first_physical_bridge_port(&master) {
                    return Some(port);
                }
            }
        }
    }
    // 3. brif/ directory (iface IS the bridge)
    first_physical_bridge_port(iface)
}

fn first_physical_bridge_port(bridge: &str) -> Option<String> {
    let brif = format!("/sys/class/net/{bridge}/brif");
    let entries = std::fs::read_dir(&brif).ok()?;
    for entry in entries.flatten() {
        let port = entry.file_name().to_string_lossy().into_owned();
        if !is_virtual_interface(&port) {
            return Some(port);
        }
    }
    None
}

/// Number of TX queues on `iface`.
pub fn get_tx_queue_count(iface: &str) -> u32 {
    let path = format!("/sys/class/net/{iface}/queues");
    std::fs::read_dir(&path)
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("tx-"))
                .count() as u32
        })
        .unwrap_or(1)
        .max(1)
}
