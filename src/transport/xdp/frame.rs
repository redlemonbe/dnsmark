// Ethernet+IPv4+UDP frame builder for the XDP TX hot path.
//
// Pre-builds a 42-byte header template (Eth+IP+UDP) stamped into every UMEM
// slot before the DNS payload is appended.  Per-frame work: copy template,
// patch IP total-length + checksum + UDP length, append DNS bytes.

#![allow(dead_code)]

use std::net::Ipv4Addr;

pub const ETH_HDR:   usize = 14;
pub const IPV4_HDR:  usize = 20;
pub const UDP_HDR:   usize = 8;
pub const OUTER_HDR: usize = ETH_HDR + IPV4_HDR + UDP_HDR; // 42 bytes

/// Pre-built Ethernet+IPv4+UDP header template.
#[derive(Clone)]
pub struct FrameHeader {
    tpl: [u8; OUTER_HDR],
}

impl FrameHeader {
    pub fn new(
        src_mac:  [u8; 6],
        dst_mac:  [u8; 6],
        src_ip:   Ipv4Addr,
        dst_ip:   Ipv4Addr,
        src_port: u16,
        dst_port: u16,
    ) -> Self {
        let mut tpl = [0u8; OUTER_HDR];
        // Ethernet
        tpl[0..6].copy_from_slice(&dst_mac);
        tpl[6..12].copy_from_slice(&src_mac);
        tpl[12..14].copy_from_slice(&[0x08, 0x00]);
        // IPv4 (ver=4, IHL=5, TTL=64, proto=17=UDP, DF flag)
        tpl[ETH_HDR]     = 0x45;
        tpl[ETH_HDR + 6] = 0x40; // flags: DF
        tpl[ETH_HDR + 8] = 64;
        tpl[ETH_HDR + 9] = 17;
        tpl[ETH_HDR + 12..ETH_HDR + 16].copy_from_slice(&src_ip.octets());
        tpl[ETH_HDR + 16..ETH_HDR + 20].copy_from_slice(&dst_ip.octets());
        // UDP
        tpl[ETH_HDR + IPV4_HDR]     = (src_port >> 8) as u8;
        tpl[ETH_HDR + IPV4_HDR + 1] = src_port as u8;
        tpl[ETH_HDR + IPV4_HDR + 2] = (dst_port >> 8) as u8;
        tpl[ETH_HDR + IPV4_HDR + 3] = dst_port as u8;
        // IP checksum = 0 and UDP checksum = 0 until write_frame patches them
        Self { tpl }
    }

    /// Stamp a complete Ethernet frame into `out` for DNS payload `dns`.
    /// Returns total frame length. `out` must be >= OUTER_HDR + dns.len().
    #[inline]
    pub fn write_frame(&self, out: &mut [u8], dns: &[u8]) -> usize {
        let total = OUTER_HDR + dns.len();
        debug_assert!(out.len() >= total);
        // DNS payload
        out[OUTER_HDR..total].copy_from_slice(dns);
        self.write_header(out, dns.len())
    }

    /// Patch the Eth+IP+UDP header for a payload of `dns_len` bytes that the
    /// caller has ALREADY written at `out[OUTER_HDR..OUTER_HDR + dns_len]`.
    /// Zero-copy hot path: the DNS query is written straight into the UMEM frame
    /// by the wire pool, then this stamps the headers — no intermediate buffer,
    /// no double copy (the Runbound model).
    #[inline]
    pub fn write_header(&self, out: &mut [u8], dns_len: usize) -> usize {
        let total   = OUTER_HDR + dns_len;
        let udp_len = (UDP_HDR + dns_len) as u16;
        let ip_tot  = (IPV4_HDR as u16) + udp_len;

        out[..OUTER_HDR].copy_from_slice(&self.tpl);
        // IP total length
        out[ETH_HDR + 2] = (ip_tot >> 8) as u8;
        out[ETH_HDR + 3] = ip_tot as u8;
        // IP checksum (header bytes [10..12] are 0 from the template copy)
        let cksum = ipv4_checksum(&out[ETH_HDR..ETH_HDR + IPV4_HDR]);
        out[ETH_HDR + 10] = (cksum >> 8) as u8;
        out[ETH_HDR + 11] = cksum as u8;
        // UDP length
        out[ETH_HDR + IPV4_HDR + 4] = (udp_len >> 8) as u8;
        out[ETH_HDR + IPV4_HDR + 5] = udp_len as u8;
        total
    }
}

#[inline]
fn ipv4_checksum(hdr: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for i in 0..(hdr.len() / 2) {
        sum += u16::from_be_bytes([hdr[2 * i], hdr[2 * i + 1]]) as u32;
    }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

/// Read local MAC from /sys/class/net/<iface>/address.
pub fn local_mac(iface: &str) -> Option<[u8; 6]> {
    let s = std::fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok()?;
    parse_mac(s.trim())
}

/// Read local IPv4 address of `iface` via getifaddrs.
pub fn local_ipv4(iface: &str) -> Option<Ipv4Addr> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 { return None; }
    let mut result = None;
    let mut cur = ifap;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() { continue; }
        if unsafe { (*ifa.ifa_addr).sa_family } as libc::c_int != libc::AF_INET { continue; }
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_str().unwrap_or("");
        if name != iface { continue; }
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        result = Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
        break;
    }
    unsafe { libc::freeifaddrs(ifap); }
    result
}

/// Resolve server MAC via /proc/net/arp; triggers ARP ping if not cached.
pub fn resolve_server_mac(server: Ipv4Addr) -> Option<[u8; 6]> {
    if let Some(m) = lookup_arp(server) { return Some(m); }
    trigger_arp(server);
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(60));
        if let Some(m) = lookup_arp(server) { return Some(m); }
    }
    None
}

fn lookup_arp(server: Ipv4Addr) -> Option<[u8; 6]> {
    let target = server.to_string();
    let content = std::fs::read_to_string("/proc/net/arp").ok()?;
    for line in content.lines().skip(1) {
        let mut c = line.split_whitespace();
        let ip    = c.next()?;
        if ip != target { continue; }
        let _hw   = c.next()?;
        let flags = c.next()?;
        if flags == "0x0" { return None; } // incomplete
        return parse_mac(c.next()?);
    }
    None
}

fn trigger_arp(server: Ipv4Addr) {
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = s.connect(std::net::SocketAddr::from((server, 53)));
    }
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let p: Vec<&str> = s.split(':').collect();
    if p.len() != 6 { return None; }
    let mut m = [0u8; 6];
    for (i, x) in p.iter().enumerate() {
        m[i] = u8::from_str_radix(x, 16).ok()?;
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_checksum_valid() {
        let hdr = FrameHeader::new(
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            "192.168.1.1".parse().unwrap(),
            "192.168.1.2".parse().unwrap(),
            12345, 53,
        );
        let dns = b"hello dns payload";
        let mut buf = vec![0u8; OUTER_HDR + dns.len()];
        let n = hdr.write_frame(&mut buf, dns);
        assert_eq!(n, buf.len());
        // Ethernet type
        assert_eq!(&buf[12..14], &[0x08, 0x00]);
        // IPv4 ver+IHL
        assert_eq!(buf[ETH_HDR] >> 4, 4);
        assert_eq!(buf[ETH_HDR] & 0xF, 5);
        // Protocol = UDP
        assert_eq!(buf[ETH_HDR + 9], 17);
        // Payload
        assert_eq!(&buf[OUTER_HDR..], dns.as_slice());
        // Verifying checksum: summing all 16-bit words incl. checksum == 0
        assert_eq!(ipv4_checksum(&buf[ETH_HDR..ETH_HDR + IPV4_HDR]), 0);
    }

    #[test]
    fn parse_mac_ok() {
        assert_eq!(parse_mac("aa:bb:cc:dd:ee:ff").unwrap(),
                   [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }
}
