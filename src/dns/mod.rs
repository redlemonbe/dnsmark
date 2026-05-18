pub mod response;

pub use response::parse_response;

pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: RD=1
    buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    buf.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT = 0
    buf.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT = 0
    buf.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT = 0
    // QNAME
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
    buf
}

pub fn parse_record_type(s: &str) -> u16 {
    match s.to_ascii_uppercase().as_str() {
        "A" => 1,
        "NS" => 2,
        "CNAME" => 5,
        "SOA" => 6,
        "PTR" => 12,
        "MX" => 15,
        "TXT" => 16,
        "AAAA" => 28,
        "SRV" => 33,
        "NAPTR" => 35,
        "DS" => 43,
        "RRSIG" => 46,
        "NSEC" => 47,
        "DNSKEY" => 48,
        "NSEC3" => 50,
        "TLSA" => 52,
        "CAA" => 257,
        "ANY" => 255,
        other => other.parse().unwrap_or(1),
    }
}
