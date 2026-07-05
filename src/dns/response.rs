#[derive(Debug, Clone, Copy)]
pub struct ResponseInfo {
    pub id: u16,
    pub rcode: u8,
}

pub fn parse_response(buf: &[u8]) -> Option<ResponseInfo> {
    if buf.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    // QR bit must be set (response)
    if flags & 0x8000 == 0 {
        return None;
    }
    let rcode = (flags & 0x000F) as u8;
    Some(ResponseInfo { id, rcode })
}
