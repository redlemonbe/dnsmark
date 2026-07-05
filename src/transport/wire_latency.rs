// Wire-anchored latency probe (`--wire-latency`).
//
// The problem (whitepaper §7): a generator's reported RTT = server + network + **generator
// software overhead**. The userspace send→recv path on the emitter (syscalls, socket queue,
// scheduling) inflates the number — dnsperf and the normal dnsmark paths all include it, so
// their absolute latency is NOT the server's.
//
// This mode removes the generator overhead by reading **kernel SO_TIMESTAMPING** stamps:
//   - TX timestamp: taken when the kernel hands the query to the NIC driver (read back from
//     the socket error queue, MSG_ERRQUEUE).
//   - RX timestamp: taken in the kernel RX softirq when the reply arrives from the driver
//     (SCM_TIMESTAMPING control message on recvmsg), BEFORE the socket queue / userspace.
// RTT = rx_ts − tx_ts ≈ network round-trip + server processing, with the emitter's userspace
// and socket-queue delay excluded. It prefers RAW HARDWARE stamps when the NIC provides them
// (e.g. ixgbe) and falls back to SOFTWARE (driver-level, available on every NIC incl. virtio).
//
// It is a serial ping-pong (one query in flight) at a paced rate: this measures the *unloaded*
// wire latency — the server's own response time anchored at the wire, free of the open-loop
// queuing that inflates the throughput-mode latency. Use it for the reference latency figure;
// use --ramp / closed-loop for the latency-vs-load curve.

use std::mem;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use crate::dns::build_query;
use crate::query::QuerySource;
use std::sync::Arc;

// SOF_TIMESTAMPING_* (linux/net_tstamp.h) — not all exposed by the libc crate, define them.
const SOF_TX_HARDWARE: u32 = 1 << 0;
const SOF_TX_SOFTWARE: u32 = 1 << 1;
const SOF_RX_HARDWARE: u32 = 1 << 2;
const SOF_RX_SOFTWARE: u32 = 1 << 3;
const SOF_SOFTWARE:    u32 = 1 << 4;
const SOF_RAW_HARDWARE:u32 = 1 << 6;
const SOF_OPT_ID:      u32 = 1 << 7;
const SOF_OPT_TSONLY:  u32 = 1 << 11;

/// scm_timestamping: three timespecs — [0] software, [1] (deprecated), [2] raw hardware.
#[repr(C)]
#[derive(Clone, Copy)]
struct ScmTimestamping {
    ts: [libc::timespec; 3],
}

#[inline]
fn ts_to_ns(t: &libc::timespec) -> u64 {
    (t.tv_sec as u64).wrapping_mul(1_000_000_000).wrapping_add(t.tv_nsec as u64)
}

/// Pull a TX timestamp (ns) off the socket error queue. Returns None if none ready.
unsafe fn read_tx_timestamp(fd: i32) -> Option<u64> {
    let mut ctrl = [0u8; 256];
    let mut iov_buf = [0u8; 256];
    let mut iov = libc::iovec { iov_base: iov_buf.as_mut_ptr() as *mut _, iov_len: iov_buf.len() };
    let mut msg: libc::msghdr = mem::zeroed();
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = ctrl.as_mut_ptr() as *mut _;
    msg.msg_controllen = ctrl.len() as _;
    let n = libc::recvmsg(fd, &mut msg, libc::MSG_ERRQUEUE | libc::MSG_DONTWAIT);
    if n < 0 { return None; }
    extract_timestamp(&msg)
}

/// Parse SCM_TIMESTAMPING out of a msghdr's control buffer. Prefers raw-HW ([2]), else SW ([0]).
unsafe fn extract_timestamp(msg: &libc::msghdr) -> Option<u64> {
    let mut cmsg = libc::CMSG_FIRSTHDR(msg);
    while !cmsg.is_null() {
        let c = &*cmsg;
        if c.cmsg_level == libc::SOL_SOCKET && c.cmsg_type == libc::SCM_TIMESTAMPING {
            let data = libc::CMSG_DATA(cmsg) as *const ScmTimestamping;
            let stamps = &*data;
            let hw = ts_to_ns(&stamps.ts[2]);
            let sw = ts_to_ns(&stamps.ts[0]);
            if hw != 0 { return Some(hw); }
            if sw != 0 { return Some(sw); }
        }
        cmsg = libc::CMSG_NXTHDR(msg, cmsg);
    }
    None
}

/// Receive one reply and return (rx_timestamp_ns, payload_len). None on timeout/error.
unsafe fn recv_with_timestamp(fd: i32, buf: &mut [u8]) -> Option<(u64, usize)> {
    let mut ctrl = [0u8; 256];
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let mut msg: libc::msghdr = mem::zeroed();
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = ctrl.as_mut_ptr() as *mut _;
    msg.msg_controllen = ctrl.len() as _;
    let n = libc::recvmsg(fd, &mut msg, 0);
    if n <= 0 { return None; }
    extract_timestamp(&msg).map(|ts| (ts, n as usize))
}

pub struct WireLatencyResult {
    pub samples: usize,
    pub hw: bool,
    pub min_us: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

/// Run the wire-latency probe: `count` paced ping-pongs, return the RTT distribution (µs).
pub fn probe(
    server_addr: SocketAddr,
    query_source: Arc<dyn QuerySource>,
    count: usize,
    rate: u64,
    timeout_ms: u64,
) -> Result<WireLatencyResult, String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind: {e}"))?;
    socket.connect(server_addr).map_err(|e| format!("connect: {e}"))?;
    let fd = socket.as_raw_fd();

    // Try HW first (RAW_HARDWARE), else SW. We don't reconfigure the NIC (SIOCSHWTSTAMP needs
    // CAP_NET_ADMIN and i40e only HW-stamps PTP) — RAW_HARDWARE simply yields a stamp when the
    // driver provides one, else ts[2]=0 and we use the software stamp ts[0].
    let flags_hw: u32 = SOF_TX_HARDWARE | SOF_RX_HARDWARE | SOF_RAW_HARDWARE
        | SOF_TX_SOFTWARE | SOF_RX_SOFTWARE | SOF_SOFTWARE | SOF_OPT_ID | SOF_OPT_TSONLY;
    let set_ts = |val: u32| -> bool {
        unsafe {
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPING,
                &val as *const u32 as *const libc::c_void,
                mem::size_of::<u32>() as libc::socklen_t) == 0
        }
    };
    if !set_ts(flags_hw) {
        return Err("SO_TIMESTAMPING setsockopt failed (kernel too old?)".into());
    }
    // HW-stamp detection is not wired yet; report SW conservatively (both TX and RX use the
    // software driver-level stamp, which is what excludes the generator's userspace overhead).
    let mut rtts_ns: Vec<u64> = Vec::with_capacity(count);
    let hw_seen = false;
    let interval = if rate > 0 { Duration::from_secs_f64(1.0 / rate as f64) } else { Duration::ZERO };

    // This mode is a *serial* ping-pong: one query in flight, the next only fires after the
    // current reply. A single slow reply therefore stalls the whole pace — and a cache-miss that
    // forwards upstream over the internet is 30–500 ms, vs ~40 µs for a cache-hit. Left unbounded
    // (the old code waited `timeout_ms`, default 3000 ms, per sample and busy-spun a core waiting
    // for the TX stamp) a run of a few thousand samples ran for minutes and printed nothing until
    // the very end, so any outer `timeout` killed it before a single percentile appeared (#18).
    //
    // Three bounds keep it honest and always-terminating:
    //   - TX stamp: appears in microseconds; wait a few ms via POLLERR (no CPU busy-spin), then
    //     skip the sample rather than stall if a path never emits a SW TX stamp.
    //   - reply: cap the per-sample wait so one slow forward can't freeze the pace.
    //   - whole probe: a wall-clock deadline; on hit we stop and report what we have.
    let tx_budget = Duration::from_millis(5);
    let reply_ms = timeout_ms.clamp(1, 250) as i32;
    let paced_secs = count as f64 / rate.max(1) as f64;
    let deadline = Instant::now() + Duration::from_secs_f64(paced_secs * 4.0 + 10.0);

    let mut tx_missing = 0usize;
    let mut no_reply = 0usize;
    let mut sent_total = 0usize;
    let mut next = Instant::now();
    let mut last_report = Instant::now();
    let mut id: u16 = rand::random();
    let mut rxbuf = [0u8; 1500];
    let mut truncated = false;

    for i in 0..count {
        if Instant::now() >= deadline {
            truncated = true;
            eprintln!("\r  … stopped at the {:.0}s time budget: {}/{} samples collected",
                paced_secs * 4.0 + 10.0, rtts_ns.len(), count);
            break;
        }
        if interval > Duration::ZERO {
            let now = Instant::now();
            if now < next { std::thread::sleep(next - now); }
            next += interval;
        }
        let entry = query_source.next();
        let q = build_query(id, &entry.name, entry.qtype);
        id = id.wrapping_add(1);
        let sent = unsafe { libc::send(fd, q.as_ptr() as *const _, q.len(), 0) };
        if sent < 0 { continue; }
        sent_total += 1;

        // TX timestamp off the socket error queue. It becomes ready within microseconds; wait for
        // POLLERR in short slices instead of busy-spinning, and give up after tx_budget.
        let mut tx_ts = None;
        let t_send = Instant::now();
        while tx_ts.is_none() && t_send.elapsed() < tx_budget {
            let mut pe = libc::pollfd { fd, events: libc::POLLERR, revents: 0 };
            unsafe { libc::poll(&mut pe, 1, 1) };
            tx_ts = unsafe { read_tx_timestamp(fd) };
        }
        let tx_ts = match tx_ts { Some(t) => t, None => { tx_missing += 1; continue } };

        // Wait for the reply (with its RX timestamp), bounded so one slow forward can't stall us.
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd, 1, reply_ms) };
        if pr <= 0 { no_reply += 1; continue; }
        if let Some((rx_ts, _len)) = unsafe { recv_with_timestamp(fd, &mut rxbuf) } {
            if rx_ts > tx_ts { rtts_ns.push(rx_ts - tx_ts); }
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            eprint!("\r  … {}/{} samples", i + 1, count);
            let _ = std::io::Write::flush(&mut std::io::stderr());
            last_report = Instant::now();
        }
    }
    if !truncated { eprintln!("\r  … {} samples done            ", sent_total); }

    if rtts_ns.is_empty() {
        return Err(format!(
            "no timestamped round-trips captured out of {sent_total} sends \
             ({tx_missing} missing a TX stamp, {no_reply} with no reply): check the target is \
             answering on this path and that the egress NIC supports SO_TIMESTAMPING"
        ));
    }
    if no_reply * 5 > sent_total.max(1) {
        eprintln!("  note: {no_reply}/{sent_total} sends got no reply within {reply_ms}ms \
                   (likely cache-misses forwarding upstream — warm the cache for a clean wire figure)");
    }
    rtts_ns.sort_unstable();
    let n = rtts_ns.len();
    let pct = |p: f64| rtts_ns[((n as f64 * p) as usize).min(n - 1)] as f64 / 1000.0;
    Ok(WireLatencyResult {
        samples: n,
        hw: hw_seen,
        min_us: rtts_ns[0] as f64 / 1000.0,
        p50_us: pct(0.50),
        p95_us: pct(0.95),
        p99_us: pct(0.99),
        max_us: rtts_ns[n - 1] as f64 / 1000.0,
    })
}
