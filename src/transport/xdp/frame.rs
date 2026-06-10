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
pub const VLAN_HDR:  usize = 4;
pub const OUTER_HDR: usize = ETH_HDR + IPV4_HDR + UDP_HDR;            // 42 (untagged)
/// Template capacity: Eth + one optional 802.1Q tag + IPv4 + UDP.
pub const OUTER_HDR_MAX: usize = OUTER_HDR + VLAN_HDR;                // 46 (tagged)

/// VLAN id from env `DNSMARK_VLAN` (0 / unset = untagged), parsed once per call.
fn vlan_from_env() -> Option<u16> {
    std::env::var("DNSMARK_VLAN")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .filter(|&v| v != 0)
}

/// Pre-built Ethernet(+802.1Q)+IPv4+UDP header template.
///
/// VLAN (#188 / dnsmark#7): when env `DNSMARK_VLAN=<vid>` is set, a single
/// 802.1Q tag is baked into the template (no per-frame shift — the bench hot
/// path stays a copy+patch). All offsets derive from `l2` (14 or 18) so the
/// untagged path is byte-for-byte unchanged.
#[derive(Clone)]
pub struct FrameHeader {
    tpl: [u8; OUTER_HDR_MAX],
    /// Total L2+L3+L4 header length actually used: 42 (untagged) or 46 (tagged).
    outer: usize,
    /// IPv4 header offset: 14 (untagged) or 18 (one VLAN tag).
    l2: usize,
    /// One's-complement sum of the constant IPv4 header words (total-length and
    /// checksum fields = 0). Per packet we add only the total-length and fold —
    /// no 10-word recompute on the hot path.
    ip_base_sum: u32,
}

impl FrameHeader {
    /// Build a header template. `DNSMARK_VLAN=<vid>` injects one 802.1Q tag.
    pub fn new(
        src_mac:  [u8; 6],
        dst_mac:  [u8; 6],
        src_ip:   Ipv4Addr,
        dst_ip:   Ipv4Addr,
        src_port: u16,
        dst_port: u16,
    ) -> Self {
        Self::new_with_vlan(src_mac, dst_mac, src_ip, dst_ip, src_port, dst_port, vlan_from_env())
    }

    /// Like `new`, but with an explicit VLAN id: `Some(vid)` bakes one 802.1Q tag
    /// into the template, `None` is untagged. `new` reads it from `DNSMARK_VLAN`;
    /// tests call this directly (no env races).
    pub fn new_with_vlan(
        src_mac:  [u8; 6],
        dst_mac:  [u8; 6],
        src_ip:   Ipv4Addr,
        dst_ip:   Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        vlan:     Option<u16>,
    ) -> Self {
        let l2 = if vlan.is_some() { ETH_HDR + VLAN_HDR } else { ETH_HDR };
        let outer = l2 + IPV4_HDR + UDP_HDR;

        let mut tpl = [0u8; OUTER_HDR_MAX];
        // Ethernet MACs
        tpl[0..6].copy_from_slice(&dst_mac);
        tpl[6..12].copy_from_slice(&src_mac);
        if let Some(vid) = vlan {
            // 802.1Q: TPID 0x8100, TCI = PCP(0)|DEI(0)|VID(12 bits), inner=IPv4.
            tpl[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
            tpl[14..16].copy_from_slice(&(vid & 0x0FFF).to_be_bytes());
            tpl[16..18].copy_from_slice(&[0x08, 0x00]);
        } else {
            tpl[12..14].copy_from_slice(&[0x08, 0x00]);
        }
        // IPv4 (ver=4, IHL=5, TTL=64, proto=17=UDP, DF flag)
        tpl[l2]     = 0x45;
        tpl[l2 + 6] = 0x40; // flags: DF
        tpl[l2 + 8] = 64;
        tpl[l2 + 9] = 17;
        tpl[l2 + 12..l2 + 16].copy_from_slice(&src_ip.octets());
        tpl[l2 + 16..l2 + 20].copy_from_slice(&dst_ip.octets());
        // UDP
        tpl[l2 + IPV4_HDR]     = (src_port >> 8) as u8;
        tpl[l2 + IPV4_HDR + 1] = src_port as u8;
        tpl[l2 + IPV4_HDR + 2] = (dst_port >> 8) as u8;
        tpl[l2 + IPV4_HDR + 3] = dst_port as u8;
        // IP checksum = 0 and UDP checksum = 0 until write_frame patches them.
        // Precompute the constant part of the IPv4 header checksum once.
        let mut ip_base_sum: u32 = 0;
        for i in 0..(IPV4_HDR / 2) {
            ip_base_sum += u16::from_be_bytes([tpl[l2 + 2*i], tpl[l2 + 2*i + 1]]) as u32;
        }
        Self { tpl, outer, l2, ip_base_sum }
    }

    /// Total header length stamped before the DNS payload: 42 (untagged) or
    /// 46 (one 802.1Q tag). The caller writes the DNS query at `out[outer()..]`.
    #[inline(always)]
    pub fn outer(&self) -> usize {
        self.outer
    }

    /// Stamp a complete Ethernet frame into `out` for DNS payload `dns`.
    /// Returns total frame length. `out` must be >= OUTER_HDR + dns.len().
    #[inline]
    pub fn write_frame(&self, out: &mut [u8], dns: &[u8]) -> usize {
        let total = self.outer + dns.len();
        debug_assert!(out.len() >= total);
        // DNS payload
        out[self.outer..total].copy_from_slice(dns);
        self.write_header(out, dns.len())
    }

    /// Patch the Eth+IP+UDP header for a payload of `dns_len` bytes that the
    /// caller has ALREADY written at `out[OUTER_HDR..OUTER_HDR + dns_len]`.
    /// Zero-copy hot path: the DNS query is written straight into the UMEM frame
    /// by the wire pool, then this stamps the headers — no intermediate buffer,
    /// no double copy (the Runbound model).
    #[inline]
    pub fn write_header(&self, out: &mut [u8], dns_len: usize) -> usize {
        let l2      = self.l2;
        let total   = self.outer + dns_len;
        let udp_len = (UDP_HDR + dns_len) as u16;
        let ip_tot  = (IPV4_HDR as u16) + udp_len;

        out[..self.outer].copy_from_slice(&self.tpl[..self.outer]);
        // IP total length
        out[l2 + 2] = (ip_tot >> 8) as u8;
        out[l2 + 3] = ip_tot as u8;
        // IP checksum: constant base + this packet's total-length, folded once
        // (RFC 1071) — no per-packet 10-word sum.
        let mut sum = self.ip_base_sum + ip_tot as u32;
        sum = (sum & 0xFFFF) + (sum >> 16);
        sum = (sum & 0xFFFF) + (sum >> 16);
        let cksum = !(sum as u16);
        out[l2 + 10] = (cksum >> 8) as u8;
        out[l2 + 11] = cksum as u8;
        // UDP length
        out[l2 + IPV4_HDR + 4] = (udp_len >> 8) as u8;
        out[l2 + IPV4_HDR + 5] = udp_len as u8;
        total
    }

    /// Patch the UDP source port in an already-stamped frame (RSS spread). VLAN
    /// aware via `self.l2`; the UDP checksum is 0 so no recomputation is needed.
    #[inline]
    pub fn set_src_port(&self, out: &mut [u8], port: u16) {
        out[self.l2 + IPV4_HDR]     = (port >> 8) as u8;
        out[self.l2 + IPV4_HDR + 1] = port as u8;
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
    // Retry for ~2s, re-triggering ARP each round (a fresh link may have no entry).
    for i in 0..40 {
        if i % 8 == 0 { trigger_arp(server); }
        std::thread::sleep(std::time::Duration::from_millis(50));
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
    // connect() alone does not emit a packet; send a byte so the kernel actually
    // resolves the route and emits an ARP request for the neighbour.
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect(std::net::SocketAddr::from((server, 9))).is_ok() {
            let _ = s.send(&[0u8]);
        }
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

    // #188: a tagged frame must match the 802.1Q wire layout EXACTLY (checked
    // against the spec, not against the code's own constants), or the receiver
    // NIC / resolver will drop it. This is the offset-bug guard.
    #[test]
    fn vlan_frame_layout_matches_8021q_spec() {
        let dst = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let src = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let hdr = FrameHeader::new_with_vlan(
            src, dst,
            "10.8.0.2".parse().unwrap(), "10.8.0.1".parse().unwrap(),
            12345, 53, Some(2126),
        );
        assert_eq!(hdr.outer(), 46, "tagged outer header = 14+4+20+8");
        let dns = b"q";
        let mut buf = vec![0u8; hdr.outer() + dns.len()];
        let n = hdr.write_frame(&mut buf, dns);
        assert_eq!(n, buf.len());
        // L2: dst MAC, src MAC, then the 802.1Q tag — NOT the EtherType.
        assert_eq!(&buf[0..6],  &dst, "dst MAC");
        assert_eq!(&buf[6..12], &src, "src MAC");
        assert_eq!(&buf[12..14], &[0x81, 0x00], "TPID 0x8100");
        // TCI: PCP=0, DEI=0, VID=2126=0x84E
        assert_eq!(u16::from_be_bytes([buf[14], buf[15]]), 2126 & 0x0FFF, "VID");
        assert_eq!(&buf[16..18], &[0x08, 0x00], "inner EtherType IPv4");
        // L3 starts at 18 (14 + 4-byte tag), not 14.
        assert_eq!(buf[18] >> 4, 4, "IPv4 version");
        assert_eq!(buf[18] & 0xF, 5, "IHL=5");
        assert_eq!(buf[18 + 9], 17, "proto UDP");
        // IPv4 header checksum valid at the shifted offset.
        assert_eq!(ipv4_checksum(&buf[18..18 + IPV4_HDR]), 0, "IPv4 checksum");
        // DNS payload sits after the full 46-byte tagged header.
        assert_eq!(&buf[46..], dns.as_slice(), "DNS payload at offset 46");
    }

    // Tagging must shift L3+ by exactly 4 and change nothing else (idempotent IP).
    #[test]
    fn vlan_only_shifts_by_four() {
        let dst = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let src = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let a = "10.8.0.2".parse().unwrap();
        let b = "10.8.0.1".parse().unwrap();
        let untag = FrameHeader::new_with_vlan(src, dst, a, b, 12345, 53, None);
        let tag   = FrameHeader::new_with_vlan(src, dst, a, b, 12345, 53, Some(100));
        let dns = b"payload!";
        let mut bu = vec![0u8; 64];
        let mut bt = vec![0u8; 64];
        let nu = untag.write_frame(&mut bu, dns);
        let nt = tag.write_frame(&mut bt, dns);
        assert_eq!(untag.outer(), 42);
        assert_eq!(nt, nu + 4, "tagged frame is exactly 4 bytes longer");
        // The IPv4 header is identical, just at offset 14 vs 18.
        assert_eq!(&bu[14..14 + IPV4_HDR], &bt[18..18 + IPV4_HDR], "IPv4 header unchanged by tagging");
    }
}
