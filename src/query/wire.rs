// Pre-built wire-format DNS query pool — zero-allocation hot path.
//
// Build once at startup; write_next_with_id() in the critical loop copies a
// pre-built template (~30–60 bytes) and patches 2 bytes (the transaction ID).
// No heap allocation, no String parsing, no label-splitting in the hot path.

/// Maximum DNS query wire length we support (standard 512-byte UDP limit).
pub const MAX_QUERY: usize = 512;

/// Pool of pre-built DNS wire queries (transaction ID = 0x00 0x00). The round-robin
/// cursor is **caller-owned** (a per-worker local counter), so worker threads share
/// only the read-only `templates` — no shared atomic, no cross-core cache-line bounce.
pub struct WireQueryPool {
    templates: Vec<Box<[u8]>>,
}

impl WireQueryPool {
    /// Build the pool from (name, qtype) pairs. Panics if `entries` is empty.
    pub fn from_pairs(entries: &[(String, u16)]) -> Self {
        assert!(!entries.is_empty(), "WireQueryPool: empty entry list");
        let templates = entries
            .iter()
            .map(|(name, qtype)| build_wire_template(name, *qtype))
            .collect();
        Self { templates }
    }

    /// Return the raw wire template at `local_idx % len` (read-only).
    /// Used by benchmarks and tests as scalar baseline for A/B comparison.
    #[inline]
    pub fn get_template(&self, local_idx: usize) -> &[u8] {
        &self.templates[local_idx % self.templates.len()]
    }

    /// Write the template at caller-owned cursor `local_idx` (mod len) into `buf`,
    /// patch the transaction ID, return the bytes written. `buf` must be >= the
    /// template length. SIMD-accelerated copy (AVX2 → SSE2 → scalar, runtime dispatch).
    #[inline]
    pub fn write_with_index(&self, local_idx: usize, id: u16, buf: &mut [u8]) -> usize {
        let idx = local_idx % self.templates.len();
        let tmpl = &self.templates[idx];
        let len = tmpl.len();
        crate::simd::memcpy_dispatch(&mut buf[..len], tmpl);
        buf[0] = (id >> 8) as u8;
        buf[1] = id as u8;
        len
    }
}

/// Build a wire-format DNS query with ID = 0x00 0x00.
fn build_wire_template(name: &str, qtype: u16) -> Box<[u8]> {
    let mut buf = Vec::with_capacity(MAX_QUERY);
    buf.extend_from_slice(&[0x00, 0x00]); // ID placeholder
    buf.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: RD=1
    buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    buf.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // AN/NS/AR = 0
    let name = name.trim_end_matches('.');
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        let b = label.as_bytes();
        buf.push(b.len() as u8);
        buf.extend_from_slice(b);
    }
    buf.push(0x00); // root label
    buf.extend_from_slice(&qtype.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    debug_assert!(buf.len() <= MAX_QUERY, "query exceeds {MAX_QUERY} bytes");
    buf.into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_id_patch() {
        let pool = WireQueryPool::from_pairs(&[("example.com".to_string(), 1)]);
        let mut buf = [0u8; MAX_QUERY];
        let len = pool.write_with_index(0, 0xBEEF, &mut buf);
        assert_eq!(buf[0], 0xBE);
        assert_eq!(buf[1], 0xEF);
        assert!(len > 12); // at least header + qname + qtype + qclass
    }

    #[test]
    fn round_robin() {
        let pool = WireQueryPool::from_pairs(&[
            ("a.com".to_string(), 1),
            ("b.com".to_string(), 1),
        ]);
        let mut buf = [0u8; MAX_QUERY];
        let l1 = pool.write_with_index(0, 1, &mut buf);
        let mut b1 = [0u8; MAX_QUERY];
        b1[..l1].copy_from_slice(&buf[..l1]);
        let _l2 = pool.write_with_index(1, 2, &mut buf);
        let l3 = pool.write_with_index(2, 3, &mut buf);
        // Third call should match first (same length)
        assert_eq!(l3, l1);
    }

    #[test]
    fn wire_format_structure() {
        let pool = WireQueryPool::from_pairs(&[("www.example.com".to_string(), 1)]);
        let mut buf = [0u8; MAX_QUERY];
        let len = pool.write_with_index(0, 0x0042, &mut buf);
        // Header: ID + flags + counts = 12 bytes
        assert!(len >= 12);
        // QR=0 (query), RD=1
        assert_eq!(buf[2], 0x01);
        assert_eq!(buf[3], 0x00);
        // QDCOUNT = 1
        assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 1);
    }
}
