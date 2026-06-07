// XDP receive path for dnsmark.
//
// Architecture:
//   - N sender OS threads: each owns a regular UDP socket, sends DNS queries,
//     records timestamps in a SHARED in_flight map keyed by a global DNS ID.
//   - 1 XDP receiver OS thread: reads raw Ethernet frames from AF_XDP rings,
//     parses Eth/IP/UDP/DNS headers, matches DNS IDs in the shared in_flight
//     map, and records RTTs in the shared StatsCollector.
//
// The XDP eBPF program intercepts UDP packets with src_port=53 (DNS responses)
// at the NIC driver level and redirects them into the AF_XDP ring buffer,
// bypassing the kernel network stack entirely.

use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
    OnceLock, Arc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::dns::{build_query, parse_response};
use crate::query::{QuerySource, WireQueryPool, MAX_QUERY};
use crate::stats::StatsCollector;

use super::frame::{self, FrameHeader};
use super::loader::XdpHandle;
use super::socket::{
    XskSocket, create_xsk_socket, get_rx_queue_count, iface_index,
    iface_for_server, default_interface,
    is_virtual_interface, parent_interface,
};
use super::umem::{XdpDesc, FRAME_SIZE, SockaddrXdp, mbind_to_node};
use super::socket::AF_XDP;

// Ethernet/IP/UDP header sizes (IPv4 with IHL=5 only; packets with options
// are XDP_PASS'd by the eBPF program and never reach this path).
const ETH_HDR:  usize = 14;
const IPV4_HDR: usize = 20;
const IPV6_HDR: usize = 40;
const UDP_HDR:  usize = 8;

const ETH_P_IP:   u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86DD;
const PROTO_UDP:   u8 = 17;

// ── Lock-free in-flight table ──────────────────────────────────────────────
//
// Indexed directly by the 16-bit DNS transaction id (65536 slots). Replaces the
// shared `Mutex<HashMap>` whose lock was hammered by every worker on every
// packet (the contention that collapsed throughput). Each slot is an AtomicU64
// holding the send time in ns since `base` (0 = free). Senders store, the
// receiver swaps-to-0 and computes the RTT — fully lock-free, no per-packet lock.
pub struct InFlight {
    base:  Instant,
    slots: Box<[AtomicU64]>, // len = 65536
}

impl InFlight {
    pub fn new() -> Self {
        let slots = (0..65536).map(|_| AtomicU64::new(0)).collect::<Vec<_>>().into_boxed_slice();
        InFlight { base: Instant::now(), slots }
    }
    #[inline]
    pub fn insert(&self, id: u16) {
        let t = (self.base.elapsed().as_nanos() as u64).max(1); // never 0 when occupied
        self.slots[id as usize].store(t, Ordering::Relaxed);
    }
    /// Mark `id` received; returns RTT in microseconds if it was outstanding.
    #[inline]
    pub fn take(&self, id: u16) -> Option<u64> {
        let prev = self.slots[id as usize].swap(0, Ordering::Relaxed);
        if prev == 0 { return None; }
        let now = self.base.elapsed().as_nanos() as u64;
        Some(now.saturating_sub(prev) / 1000)
    }
    /// Expire slots older than `timeout`; returns the count expired.
    pub fn sweep(&self, timeout: Duration) -> usize {
        let now = self.base.elapsed().as_nanos() as u64;
        let to  = timeout.as_nanos() as u64;
        let mut n = 0;
        for s in self.slots.iter() {
            let v = s.load(Ordering::Relaxed);
            if v != 0 && now.saturating_sub(v) > to
                && s.compare_exchange(v, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok()
            {
                n += 1;
            }
        }
        n
    }
    /// Expire slots older than `timeout`; returns ages in µs for each expired slot.
    /// Used to record timed-out queries into the latency histogram (honest tail).
    pub fn sweep_with_ages(&self, timeout: Duration) -> Vec<u64> {
        let now = self.base.elapsed().as_nanos() as u64;
        let to  = timeout.as_nanos() as u64;
        let mut ages = Vec::new();
        for s in self.slots.iter() {
            let v = s.load(Ordering::Relaxed);
            if v != 0 && now.saturating_sub(v) > to
                && s.compare_exchange(v, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok()
            {
                ages.push(now.saturating_sub(v) / 1000); // µs
            }
        }
        ages
    }
    /// Drain ALL non-zero slots (end-of-run). Returns ages in µs.
    pub fn drain_all(&self) -> Vec<u64> {
        let now = self.base.elapsed().as_nanos() as u64;
        let mut ages = Vec::new();
        for s in self.slots.iter() {
            let v = s.swap(0, Ordering::Relaxed);
            if v != 0 {
                ages.push(now.saturating_sub(v) / 1000);
            }
        }
        ages
    }
}

// ── XDP TX state ─────────────────────────────────────────────────────────

/// Per-NIC-queue TX state shared by sender workers assigned to that queue.
/// Sender workers write Ethernet frames into UMEM and submit to the TX ring.
pub struct XdpTxState {
    pub fd:   i32,
    pub tx:   Mutex<super::umem::DescRing>,
    pub comp: Mutex<super::umem::AddrRing>,
    pub pool: Mutex<Vec<u64>>,
    pub hdr:  FrameHeader,
    pub sa:   SockaddrXdp,
    pub area: *mut u8,
}
unsafe impl Send for XdpTxState {}
unsafe impl Sync for XdpTxState {}

/// Set once during `start_xdp_receive_path` for all queues.
/// Sender workers index into this by `worker_id % len`.
static XDP_TX_STATES: OnceLock<Vec<Arc<XdpTxState>>> = OnceLock::new();

/// True when the unified (RX+TX in one thread per queue) workers are running.
/// In that mode the engine's sender tasks become no-ops — the unified worker
/// owns its queue's whole socket, so there is no thread split and no Mutex on
/// the rings (the HT-contending 2-thread-per-queue model is what capped scaling
/// to the 8 NIC-local physical cores). This is the Runbound worker model.
static XDP_UNIFIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// Global physical-core cursor across ALL NICs: NIC1 takes its node, NIC2 spills
// to the next node, so dual fibre uses distinct cores (no oversubscription).
static GLOBAL_CORE_IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Config the engine hands to the unified workers (set before start_xdp_receive_path).
pub struct UnifiedCfg {
    pub wire_pool:       Arc<WireQueryPool>,
    pub qps_per_worker:  Arc<AtomicU64>,
    pub max_outstanding: usize,
    pub total_qps:       u64,   // used to recalibrate per-worker rate after spawn
}
static UNIFIED_CFG: OnceLock<UnifiedCfg> = OnceLock::new();

/// Called by the engine before `start_xdp_receive_path` to enable the unified
/// (RX+TX, one thread per queue/core) datapath. Without it, the legacy split
/// sender/receiver path is used.
pub fn set_unified_cfg(cfg: UnifiedCfg) {
    let _ = UNIFIED_CFG.set(cfg);
}

const TX_BATCH: usize = 64;

/// One worker per NIC queue, pinned to one NIC-local physical core, owning the
/// entire AF_XDP socket: it generates+TXes queries, reclaims TX completions, drains
/// RX responses and records stats — all in a single loop, no shared per-packet state,
/// no Mutex on the rings, no second thread stealing the physical core via HT.
#[allow(clippy::too_many_arguments)]
fn xdp_unified_worker(
    mut sock:         XskSocket,
    hdr:              FrameHeader,
    sa:               SockaddrXdp,
    wire_pool:        Arc<WireQueryPool>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    qps_per_worker:   Arc<AtomicU64>,
    worker_id:        usize,
    num_workers:      usize,
    max_outstanding:  usize,
    timeout_dur:      Duration,
) {
    let area = sock.umem.ptr_at(0);
    let fd   = sock.fd;
    let nw      = num_workers.max(1);
    let id_span = (65536usize / nw).max(1);
    let id_base = ((worker_id % nw) * id_span) as u16;
    // Per-worker local in-flight table — zero shared state, zero aliasing inter-worker.
    let in_flight = InFlight::new();
    let mut local_in_flight: usize = 0;
    let mut id_ctr:   usize = 0;
    let mut tmpl_idx: usize = worker_id;
    const FLUSH_N: usize = 1024;
    let mut local_egress:      usize = 0;
    let mut _local_submitted: usize = 0;
    // stall detection (egress vs submitted)
    let mut stall_sub:    usize = 0;
    let mut stall_egr:    usize = 0;
    let mut stall_warned: bool  = false;
    let mut stall_window  = std::time::Instant::now();

    // Fixed src port per worker: RSS on the receiver hashes (src_ip, dst_ip, sport, dport=53)
    // → each worker's responses land on one RX queue of the receiver, which maps back
    // to this worker's XSK via symmetric RSS. Zero cross-worker aliasing.
    let sport: u16 = 2048u16.wrapping_add(worker_id as u16);

    let mut descs:    Vec<XdpDesc> = Vec::with_capacity(TX_BATCH);
    let mut ids_to_register: Vec<u16> = Vec::with_capacity(TX_BATCH);
    let mut rx_addrs: Vec<u64>     = Vec::with_capacity(2048);
    let mut last_timeout = Instant::now();

    // qps pacing as a token bucket (0 = unlimited flood). Tokens accrue at the
    // target rate independently of how fast this loop spins, so the send rate is
    // exact even though the worker also drains RX every iteration (a fixed
    // "1 packet when due" cadence collapsed to the loop's iteration rate).
    let mut tokens: f64 = 0.0;
    let mut last_refill = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // 1) Reclaim TX completions → recycle frames to the local pool.
        // 1) Reclaim TX completions → recycle frames; count EGRESS (DMA done).
        let done = sock.umem.comp.dequeue_all();
        if !done.is_empty() {
            let n = done.len();
            sock.tx_pool.extend_from_slice(&done);
            local_egress += n;
            stall_egr    += n;
            if local_egress >= FLUSH_N {
                stats.inc_sent_n(local_egress);
                local_egress = 0;
            }
        }
        // stall detection: every 2 s, warn if egress << submitted
        if stall_window.elapsed().as_secs() >= 2 && !stall_warned {
            if stall_sub >= 1024 && stall_egr < stall_sub / 5 {
                let pct = stall_egr * 100 / stall_sub.max(1);
                eprintln!(
                    "\x1b[33m[dnsmark] WARN: XDP TX stalling — {} submitted, \
                    {} egressed ({}% not transmitted). \
                    Reset NIC: modprobe -r ixgbe && modprobe ixgbe\x1b[0m",
                    stall_sub, stall_egr, pct
                );
                stall_warned = true;
            }
            stall_sub = 0; stall_egr = 0;
            stall_window = std::time::Instant::now();
        }

        // 2) Decide how many to TX this iteration (rate / backpressure gate).
        let qps = qps_per_worker.load(Ordering::Relaxed);
        let mut headroom = if qps > 0 {
            let now = Instant::now();
            tokens = (tokens + now.duration_since(last_refill).as_secs_f64() * qps as f64)
                .min(TX_BATCH as f64);
            last_refill = now;
            let n = tokens.floor();
            tokens -= n;
            n as usize
        } else {
            TX_BATCH
        };
        if max_outstanding > 0 {
            headroom = headroom.min(max_outstanding.saturating_sub(local_in_flight));
        }

        // 3) TX a batch straight into UMEM (one SIMD copy, one produce_tx, one kick).
        if headroom > 0 {
            descs.clear();
            ids_to_register.clear();
            let take = headroom.min(sock.tx_pool.len());
            for _ in 0..take {
                if let Some(addr) = sock.tx_pool.pop() {
                    let id = id_base.wrapping_add((id_ctr % id_span) as u16); id_ctr += 1;
                    // SAFETY: addr is a frame offset from this worker's own pool; the
                    //         UMEM slice is mapped and owned solely by this thread.
                    let buf = unsafe {
                        std::slice::from_raw_parts_mut(area.add(addr as usize), FRAME_SIZE as usize)
                    };
                    let dns_len = wire_pool.write_with_index(tmpl_idx, id, &mut buf[frame::OUTER_HDR..]);
                    tmpl_idx += 1;
                    let total = hdr.write_header(buf, dns_len);
                    frame::set_src_port(buf, sport);
                    descs.push(XdpDesc { addr, len: total as u32, options: 0 });
                    ids_to_register.push(id);
                }
            }
            if !descs.is_empty() {
                let enq = sock.tx.produce_tx(&descs);
                if enq < descs.len() {
                    for d in &descs[enq..] { sock.tx_pool.push(d.addr); }
                }
                if enq > 0 {
                    // Always kick: no RX-driven NAPI on a generator (see xdp_tx_batch_inline).
                    unsafe {
                        libc::sendto(
                            fd, std::ptr::null(), 0, libc::MSG_DONTWAIT,
                            &sa as *const SockaddrXdp as *const libc::sockaddr,
                            std::mem::size_of::<SockaddrXdp>() as libc::socklen_t,
                        );
                    }
                    // global_in_flight gates max_outstanding: it MUST be accurate, so
                    // increment per batch (not sharded). It is skipped entirely in flood
                    // mode (max_outstanding==0). stats.sent stays sharded (no gate role).
                    // Timestamp AFTER the kick: comparable to dnsperf which timestamps
                    // at sendmsg(), not at buffer preparation.
                    for &id in ids_to_register.iter().take(enq) {
                        in_flight.insert(id);
                    }
                    if max_outstanding > 0 {
                        local_in_flight += enq;
                        stats.record_inflight(local_in_flight);
                    }
                    _local_submitted += enq;
                    stall_sub       += enq;
                    // backpressure gate only — NOT stats.sent
                }
            }
        }

        // 4) Drain RX responses → match in_flight → stats → refill the fill ring.
        let rxds = sock.rx.consume_rx();
        if !rxds.is_empty() {
            rx_addrs.clear();
            let mut completed = 0usize;
            for desc in &rxds {
                let frame = unsafe { sock.umem.frame(desc.addr, desc.len as usize) };
                if let Some((id, rcode)) = parse_dns_from_frame(frame) {
                    if let Some(rtt_us) = in_flight.take(id) { stats.record_response(rcode, rtt_us); completed += 1; }
                }
                rx_addrs.push(desc.addr);
            }
            sock.umem.fill.enqueue_batch(&rx_addrs);
            if completed > 0 && max_outstanding > 0 {
                local_in_flight = local_in_flight.saturating_sub(completed);
            }
        } else if sock.umem.fill.needs_wakeup() {
            unsafe {
                libc::recvfrom(fd, std::ptr::null_mut(), 0, libc::MSG_DONTWAIT,
                    std::ptr::null_mut(), std::ptr::null_mut());
            }
        }

        // 5) Expire timed-out queries every 10 ms.
        // A timed-out query is a loss (no response arrived within the timeout): count
        // it, but do not record it as a completion or a latency sample. The histogram
        // holds real response latencies only — slow responses still count (default
        // timeout is 3 s), genuine timeouts do not pollute the latency distribution.
        let now = Instant::now();
        if now.duration_since(last_timeout) >= Duration::from_millis(10) {
            let ages = in_flight.sweep_with_ages(timeout_dur);
            if !ages.is_empty() {
                for _ in &ages {
                    stats.inc_timeout();
                }
                if max_outstanding > 0 {
                    local_in_flight = local_in_flight.saturating_sub(ages.len());
                }
            }
            last_timeout = now;
        }
    }
    // End-of-run: queries still in flight never got a response → losses.
    {
        let remaining = in_flight.drain_all();
        for _ in &remaining {
            stats.inc_timeout();
        }
        // DIAGNOSTIC: print raw egress total so caller can compare to
        // ethtool -S <nic> tx_packets at the same instant.
        // This is the ONLY honest number — compare to ASIC to diagnose +9% residual.
        if local_egress > 0 { stats.inc_sent_n(local_egress); }
    }
}

// ── Frame parser ──────────────────────────────────────────────────────────

/// Parse a raw Ethernet frame and return the DNS response ID + rcode,
/// or None if the frame is not a valid DNS response.
fn parse_dns_from_frame(frame: &[u8]) -> Option<(u16, u8)> {
    if frame.len() < ETH_HDR + 2 { return None; }

    let eth_type = u16::from_be_bytes([frame[12], frame[13]]);
    let ip_hdr_len = match eth_type {
        ETH_P_IP => {
            if frame.len() < ETH_HDR + IPV4_HDR { return None; }
            if frame[ETH_HDR + 9] != PROTO_UDP { return None; }
            let ihl = ((frame[ETH_HDR] & 0xF) as usize) * 4;
            if ihl < IPV4_HDR { return None; }
            ihl
        }
        ETH_P_IPV6 => {
            if frame.len() < ETH_HDR + IPV6_HDR { return None; }
            if frame[ETH_HDR + 6] != PROTO_UDP { return None; }
            IPV6_HDR
        }
        _ => return None,
    };

    let dns_off = ETH_HDR + ip_hdr_len + UDP_HDR;
    if frame.len() < dns_off + 12 { return None; }

    parse_response(&frame[dns_off..]).map(|r| (r.id, r.rcode))
}

// ── XDP receiver OS thread ─────────────────────────────────────────────────

fn xdp_receiver_thread(
    sock:            XskSocket,
    in_flight:       Arc<InFlight>,
    global_in_flight: Arc<AtomicUsize>,
    stats:           Arc<StatsCollector>,
    shutdown:        Arc<AtomicBool>,
    timeout_dur:     Duration,
) {
    let mut last_timeout_check = Instant::now();
    let mut idle_spins: u32 = 0;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // Drain RX ring (busy-poll — no 100 ms poll() that would stall the hot path).
        let descs = sock.rx.consume_rx();

        if descs.is_empty() {
            // No RX descriptors: kick the driver to consume the fill ring and
            // produce RX (required under XDP_USE_NEED_WAKEUP), then back off briefly.
            if sock.umem.fill.needs_wakeup() {
                unsafe {
                    libc::recvfrom(
                        sock.fd, std::ptr::null_mut(), 0, libc::MSG_DONTWAIT,
                        std::ptr::null_mut(), std::ptr::null_mut(),
                    );
                }
            }
            idle_spins += 1;
            if idle_spins >= 1024 {
                // Long idle: sleep on poll so we don't burn a core for nothing,
                // but stay responsive (1 ms) for shutdown + timeout sweeps.
                let mut pfd = libc::pollfd { fd: sock.fd, events: libc::POLLIN, revents: 0 };
                unsafe { libc::poll(&mut pfd, 1, 1); }
                idle_spins = 0;
            } else {
                std::hint::spin_loop();
            }
            // Timeout sweep while idle.
            let now = Instant::now();
            if now.duration_since(last_timeout_check) >= Duration::from_millis(10) {
                let ages = in_flight.sweep_with_ages(timeout_dur);
                if !ages.is_empty() {
                    for _ in &ages {
                        stats.inc_timeout(); // timeout = loss, not a completion or latency
                    }
                    global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| Some(x.saturating_sub(ages.len()))).ok();
                }
                last_timeout_check = now;
            }
            continue;
        }
        idle_spins = 0;

        {
            let mut completed = 0usize;

            {
                let mut recycle: Vec<u64> = Vec::with_capacity(descs.len());

                for desc in &descs {
                    let frame = unsafe { sock.umem.frame(desc.addr, desc.len as usize) };
                    if let Some((id, rcode)) = parse_dns_from_frame(frame) {
                        if let Some(rtt_us) = in_flight.take(id) {
                            stats.record_response(rcode, rtt_us);
                            completed += 1;
                        }
                    }
                    recycle.push(desc.addr);
                }

                // Return frames to fill ring for re-use.
                sock.umem.fill.enqueue_batch(&recycle);
            }

            if completed > 0 {
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                    Some(x.saturating_sub(completed))
                }).ok();
            }
        }

        // Completion ring is drained by XDP TX sender threads.

        // Expire timed-out queries every 10 ms.
        let now = Instant::now();
        if now.duration_since(last_timeout_check) >= Duration::from_millis(10) {
            let expired = in_flight.sweep_with_ages(timeout_dur).len();
            if expired > 0 {
                for _ in 0..expired { stats.inc_timeout(); }
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                    Some(x.saturating_sub(expired))
                }).ok();
            }
            last_timeout_check = now;
        }
    }
}

// ── XDP sender OS thread ───────────────────────────────────────────────────
//
// Identical to udp.rs sender_thread except:
//   - Uses a SHARED in_flight map and GLOBAL DNS ID counter (AtomicU16).
//   - Does not start a receiver — the shared XDP receiver handles all responses.

#[allow(clippy::too_many_arguments)]
fn xdp_sender_thread(
    fd:               i32,
    in_flight:        Arc<InFlight>,
    global_in_flight: Arc<AtomicUsize>,
    global_id:        Arc<AtomicU16>,
    query_source:     Arc<dyn QuerySource>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    max_outstanding:  usize,
) {
    let mut next_send  = Instant::now();
    let mut last_qps:   u64 = 0;
    let mut send_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        let qps = qps_per_worker.load(Ordering::Relaxed);

        if qps > 0 {
            if qps != last_qps {
                send_interval = Duration::from_secs_f64(1.0 / qps as f64);
                next_send = Instant::now();
                last_qps = qps;
            }

            if max_outstanding > 0
                && global_in_flight.load(Ordering::Relaxed) >= max_outstanding
            {
                next_send = Instant::now() + send_interval;
                continue;
            }

            let now = Instant::now();
            if now < next_send {
                std::thread::sleep(next_send - now);
                if shutdown.load(Ordering::Relaxed) { break; }
            }
            next_send += send_interval;
            if next_send < Instant::now() { next_send = Instant::now(); }

            let id = global_id.fetch_add(1, Ordering::Relaxed);
            let entry = query_source.next();
            let qbytes = build_query(id, &entry.name, entry.qtype);
            let ret = unsafe {
                libc::send(fd, qbytes.as_ptr() as *const libc::c_void, qbytes.len(), 0)
            };
            if ret >= 0 {
                in_flight.insert(id);
                global_in_flight.fetch_add(1, Ordering::Relaxed);
                stats.inc_sent();
                if verbose {
                    tracing::debug!(id, name = %entry.name, "XDP sent query");
                }
            } else {
                tracing::debug!("XDP UDP send error: {}", std::io::Error::last_os_error());
                stats.inc_error();
            }
        } else {
            // Unlimited mode: burst-send up to headroom in global cap.
            let headroom = if max_outstanding > 0 {
                let cur = global_in_flight.load(Ordering::Relaxed);
                if cur >= max_outstanding {
                    std::thread::yield_now();
                    last_qps = 0;
                    continue;
                }
                (max_outstanding - cur).min(64)
            } else {
                64
            };

            let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(headroom);
            let mut ids:  Vec<u16>     = Vec::with_capacity(headroom);
            for _ in 0..headroom {
                let id = global_id.fetch_add(1, Ordering::Relaxed);
                let entry = query_source.next();
                bufs.push(build_query(id, &entry.name, entry.qtype));
                ids.push(id);
            }

            let mut iovecs: Vec<libc::iovec> = bufs.iter()
                .map(|b| libc::iovec { iov_base: b.as_ptr() as *mut _, iov_len: b.len() })
                .collect();
            let mut msgs: Vec<libc::mmsghdr> = iovecs.iter_mut()
                .map(|iov| {
                    let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
                    m.msg_hdr.msg_iov = iov as *mut _;
                    m.msg_hdr.msg_iovlen = 1;
                    m
                })
                .collect();

            let sent = unsafe {
                libc::sendmmsg(
                    fd, msgs.as_mut_ptr(), msgs.len() as libc::c_uint,
                    libc::MSG_DONTWAIT as _,
                )
            };
            let sent = if sent < 0 { 0usize } else { sent as usize };

            if sent > 0 {
                for &id in ids.iter().take(sent) {
                    in_flight.insert(id);
                }
                global_in_flight.fetch_add(sent, Ordering::Relaxed);
                stats.inc_sent_n(sent);
            }

            last_qps = 0;
        }
    }
}

// ── XDP TX sender ─────────────────────────────────────────────────────────

/// Write one DNS query as a complete Ethernet frame to the XDP TX ring.
/// Returns false if the TX pool is empty (caller should yield and retry).
fn xdp_tx_one(state: &XdpTxState, dns: &[u8]) -> bool {
    let frame_addr = match state.pool.lock().pop() {
        Some(a) => a,
        None    => return false,
    };

    let frame_len = unsafe {
        let buf = std::slice::from_raw_parts_mut(
            state.area.add(frame_addr as usize),
            FRAME_SIZE as usize,
        );
        state.hdr.write_frame(buf, dns)
    };

    let desc = XdpDesc { addr: frame_addr, len: frame_len as u32, options: 0 };
    let (enqueued, kick) = {
        let tx = state.tx.lock();
        let n = tx.produce_tx(&[desc]);
        (n, tx.needs_wakeup())
    };
    if enqueued == 0 {
        state.pool.lock().push(frame_addr);
        return false;
    }
    if kick {
        unsafe {
            libc::sendto(
                state.fd,
                std::ptr::null(),
                0,
                libc::MSG_DONTWAIT,
                &state.sa as *const SockaddrXdp as *const libc::sockaddr,
                std::mem::size_of::<SockaddrXdp>() as libc::socklen_t,
            );
        }
    }
    true
}

/// Zero-alloc batched TX (the line-rate hot path). Pops up to `count` frames from
/// the per-queue pool, writes each DNS query DIRECTLY into its UMEM frame via the
/// wire pool (one SIMD copy, no intermediate Vec, no double copy), stamps the
/// Eth/IP/UDP header, submits ALL descriptors in one produce_tx and kicks once.
/// `descs`, `out_ids`, `addrs` are caller-owned reused scratch (no per-batch alloc).
/// Generates ids from the worker's disjoint range. Returns the count placed on the ring.
#[allow(clippy::too_many_arguments)]
fn xdp_tx_batch_inline(
    state:     &XdpTxState,
    wire_pool: &WireQueryPool,
    id_base:   u16,
    id_span:   usize,
    id_ctr:    &mut usize,
    tmpl_idx:  &mut usize,
    count:     usize,
    descs:     &mut Vec<XdpDesc>,
    out_ids:   &mut Vec<u16>,
    addrs:     &mut Vec<u64>,
) -> usize {
    descs.clear();
    out_ids.clear();
    addrs.clear();
    {
        let mut pool = state.pool.lock();
        let take = count.min(pool.len());
        for _ in 0..take {
            if let Some(a) = pool.pop() { addrs.push(a); }
        }
    }
    if addrs.is_empty() { return 0; }

    for &addr in addrs.iter() {
        let id = id_base.wrapping_add((*id_ctr % id_span) as u16); *id_ctr += 1;
        // SAFETY: addr is a valid frame offset popped from this queue's own pool;
        //         the UMEM region [addr, addr+FRAME_SIZE) is mapped and owned solely
        //         by this worker until it is recycled via the completion ring.
        let buf = unsafe {
            std::slice::from_raw_parts_mut(state.area.add(addr as usize), FRAME_SIZE as usize)
        };
        // Wire pool writes the DNS query straight into the frame payload region
        // (after the 42-byte Eth/IP/UDP header) — no scratch buffer, no realloc.
        let dns_len = wire_pool.write_with_index(*tmpl_idx, id, &mut buf[frame::OUTER_HDR..]);
        *tmpl_idx += 1;
        let total = state.hdr.write_header(buf, dns_len);
        descs.push(XdpDesc { addr, len: total as u32, options: 0 });
        out_ids.push(id);
    }

    let enqueued = {
        let tx = state.tx.lock();
        tx.produce_tx(descs)
    };
    if enqueued < addrs.len() {
        let mut pool = state.pool.lock();
        for &a in &addrs[enqueued..] { pool.push(a); }
    }
    // ALWAYS kick on a TX-only flood. NEED_WAKEUP can report "no wakeup needed"
    // while no NAPI is running (there is no RX traffic to trigger the softirq on a
    // pure generator), so frames would sit unsent in the TX ring → throughput
    // plateaus and turns noisy. An unconditional sendto kick guarantees the driver
    // services the queue every batch.
    if enqueued > 0 {
        unsafe {
            libc::sendto(
                state.fd, std::ptr::null(), 0, libc::MSG_DONTWAIT,
                &state.sa as *const SockaddrXdp as *const libc::sockaddr,
                std::mem::size_of::<SockaddrXdp>() as libc::socklen_t,
            );
        }
    }
    enqueued
}

#[allow(clippy::too_many_arguments)]
fn xdp_tx_sender_thread(
    state:            Arc<XdpTxState>,
    in_flight:        Arc<InFlight>,
    global_in_flight: Arc<AtomicUsize>,
    wire_pool:        Arc<WireQueryPool>,
    worker_id:        usize,
    num_workers:      usize,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    max_outstanding:  usize,
) {
    // Per-worker disjoint DNS-id range: no shared global_id atomic, and disjoint
    // slots in the lock-free in_flight table → zero cross-core contention.
    let nw      = num_workers.max(1);
    let id_span = (65536usize / nw).max(1);
    let id_base = ((worker_id % nw) * id_span) as u16;
    let mut id_ctr: usize = 0;
    let mut tmpl_idx: usize = worker_id;        // private template cursor (no shared atomic)
    let mut single_buf = vec![0u8; MAX_QUERY];
    // Reused per-worker scratch for the batched TX path — zero allocation in the hot
    // loop. The old per-packet `vec![0u8; MAX_QUERY]` hammered the global allocator
    // across cores and flat-lined throughput at ~700k regardless of core count.
    let mut descs_scratch: Vec<XdpDesc> = Vec::with_capacity(TX_BATCH);
    let mut ids_scratch:   Vec<u16>     = Vec::with_capacity(TX_BATCH);
    let mut addrs_scratch: Vec<u64>     = Vec::with_capacity(TX_BATCH);

    // Sharded counters: accumulate locally, flush to the shared atomics only every
    // FLUSH_N sends. A per-batch fetch_add on stats.sent / global_in_flight bounces
    // ONE cache line across every core (cross-CCX on the Threadripper) → that is the
    // anti-scaling (500k/worker @ c=2 collapsing to 81k/worker @ c=8). Flushing rarely
    // keeps the hot path per-core, zero shared per-packet state (the Runbound model).
    const FLUSH_N: usize = 1024;
    let mut local_sent: usize = 0;

    let mut next_send    = Instant::now();
    let mut last_qps: u64 = 0;
    let mut send_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // Recycle completed TX frames back to pool
        {
            let done = state.comp.lock().dequeue_all();
            if !done.is_empty() { state.pool.lock().extend_from_slice(&done); }
        }

        let qps = qps_per_worker.load(Ordering::Relaxed);

        if qps > 0 {
            if qps != last_qps {
                send_interval = Duration::from_secs_f64(1.0 / qps as f64);
                next_send = Instant::now();
                last_qps = qps;
            }
            if max_outstanding > 0
                && global_in_flight.load(Ordering::Relaxed) >= max_outstanding
            {
                next_send = Instant::now() + send_interval;
                continue;
            }
            let now = Instant::now();
            if now < next_send {
                std::thread::sleep(next_send - now);
                if shutdown.load(Ordering::Relaxed) { break; }
            }
            next_send += send_interval;
            if next_send < Instant::now() { next_send = Instant::now(); }

            let id  = id_base.wrapping_add((id_ctr % id_span) as u16); id_ctr += 1;
            let len = wire_pool.write_with_index(tmpl_idx, id, &mut single_buf); tmpl_idx += 1;
            if xdp_tx_one(&state, &single_buf[..len]) {
                in_flight.insert(id);
                local_sent += 1;
                if local_sent >= FLUSH_N {
                    stats.inc_sent_n(local_sent);
                    if max_outstanding > 0 { global_in_flight.fetch_add(local_sent, Ordering::Relaxed); }
                    local_sent = 0;
                }
                if verbose { tracing::debug!(id, "XDP TX sent query"); }
            }
        } else {
            // Unlimited: burst up to headroom
            let headroom = if max_outstanding > 0 {
                let cur = global_in_flight.load(Ordering::Relaxed);
                if cur >= max_outstanding {
                    std::thread::yield_now();
                    last_qps = 0;
                    continue;
                }
                (max_outstanding - cur).min(TX_BATCH)
            } else {
                TX_BATCH
            };

            // Zero-alloc batched TX: DNS written straight into the UMEM frames via
            // the wire pool, reused scratch, one produce_tx + one kick. No per-packet
            // allocation (the allocator contention that flat-lined the per-core scaling).
            let sent = xdp_tx_batch_inline(
                &state, &wire_pool, id_base, id_span, &mut id_ctr, &mut tmpl_idx,
                headroom, &mut descs_scratch, &mut ids_scratch, &mut addrs_scratch,
            );
            if sent > 0 {
                for &id in ids_scratch.iter().take(sent) { in_flight.insert(id); }
                local_sent += sent;
                if local_sent >= FLUSH_N {
                    stats.inc_sent_n(local_sent);
                    if max_outstanding > 0 { global_in_flight.fetch_add(local_sent, Ordering::Relaxed); }
                    local_sent = 0;
                }
            } else {
                std::thread::yield_now();
            }
            last_qps = 0;
        }
    }

    // Flush any remaining locally-counted sends so the final stats are exact.
    if local_sent > 0 {
        stats.inc_sent_n(local_sent);
        if max_outstanding > 0 { global_in_flight.fetch_add(local_sent, Ordering::Relaxed); }
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Load the XDP program, create AF_XDP sockets (one per NIC queue),
/// spawn the XDP receiver thread, and return the XdpHandle (RAII).
///
/// If `iface` is a virtual interface (Proxmox bridge, vmbr*, veth, ipvlan,
/// macvlan), the function automatically retries on the physical parent. If no
/// parent is found, XDP is disabled and `Err` is returned so the caller can
/// fall back to the standard UDP receive path.
///
/// The receiver thread exits when `shutdown` is set to true.
/// The XdpHandle must stay alive for the duration of the test.
#[allow(clippy::too_many_arguments)]
pub fn start_xdp_receive_path(
    iface:            &str,
    server:           IpAddr,
    server_port:      u16,
    in_flight:        Arc<InFlight>,
    global_in_flight: Arc<AtomicUsize>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_dur:      Duration,
) -> Result<XdpHandle, String> {
    if is_virtual_interface(iface) {
        match parent_interface(iface) {
            Some(ref parent) => {
                tracing::warn!(
                    virt = %iface, parent = %parent,
                    "XDP: '{}' is a virtual interface — retrying on parent '{}'",
                    iface, parent
                );
                return do_start_xdp_receive_path(
                    parent, server, server_port,
                    in_flight, global_in_flight, stats, shutdown, timeout_dur,
                );
            }
            None => {
                let msg = format!(
                    "XDP: '{}' is a virtual interface (Proxmox bridge / vmbr / veth) \
                     — XDP disabled, falling back to UDP receive. \
                     For native XDP use the physical NIC directly.",
                    iface
                );
                tracing::warn!("{msg}");
                return Err(msg);
            }
        }
    }
    do_start_xdp_receive_path(iface, server, server_port, in_flight, global_in_flight, stats, shutdown, timeout_dur)
}

/// Parse the NIC's NUMA-local logical CPUs from /sys (e.g. "32-39,96-103").
/// Lower half = physical cores, upper half = their HT siblings (same node).
fn nic_local_logical_cpus(iface: &str) -> Vec<usize> {
    let s = std::fs::read_to_string(format!("/sys/class/net/{iface}/device/local_cpulist"))
        .unwrap_or_default();
    let mut v = Vec::new();
    for part in s.trim().split(',') {
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                v.extend(a..=b);
            }
        } else if let Ok(a) = part.trim().parse::<usize>() {
            v.push(a);
        }
    }
    v
}

/// Pin the calling thread to a specific logical CPU id.
fn pin_thread_to(cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

#[allow(clippy::too_many_arguments)]
fn do_start_xdp_receive_path(
    iface:            &str,
    server:           IpAddr,
    server_port:      u16,
    in_flight:        Arc<InFlight>,
    global_in_flight: Arc<AtomicUsize>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_dur:      Duration,
) -> Result<XdpHandle, String> {
    let ifidx = iface_index(iface)
        .ok_or_else(|| format!("interface {iface} not found"))?;

    let mut handle = XdpHandle::load(iface)?;

    // Build Ethernet frame header for XDP TX (IPv4 only; IPv6 falls back to sendmmsg).
    let frame_hdr_opt: Option<FrameHeader> = (|| {
        let IpAddr::V4(dst_ip) = server else {
            tracing::warn!("[XDP TX] server is IPv6 — AF_XDP TX unavailable, using sendmmsg");
            return None;
        };
        let src_ip = match frame::local_ipv4(iface) {
            Some(v) => v,
            None => { tracing::warn!("[XDP TX] no IPv4 on {} — using sendmmsg", iface); return None; }
        };
        let src_mac = match frame::local_mac(iface) {
            Some(v) => v,
            None => { tracing::warn!("[XDP TX] no MAC on {} — using sendmmsg", iface); return None; }
        };
        let dst_mac = match frame::resolve_server_mac(dst_ip) {
            Some(v) => v,
            None => {
                tracing::warn!(
                    "[XDP TX] could not resolve MAC of {} (ARP) — FALLING BACK TO sendmmsg \
                     (NOT zero-copy XDP TX!). Populate ARP: `ip neigh replace {} lladdr <mac> dev {} nud permanent`",
                    dst_ip, dst_ip, iface
                );
                return None;
            }
        };
        tracing::info!(
            "[XDP TX] frame header: src={} dst={} sport=12345 dport={}",
            src_ip, dst_ip, server_port
        );
        Some(FrameHeader::new(src_mac, dst_mac, src_ip, dst_ip, 12345, server_port))
    })();

    let hw_queue_count = get_rx_queue_count(iface);
    let mut tx_states: Vec<Arc<XdpTxState>> = Vec::new();
    let mut n_unified: usize = 0;

    // NIC-local NUMA node + logical CPUs. The UMEM is bound here and the receiver
    // threads are pinned to local HT siblings so neither the DMA nor the response
    // parsing ever crosses to a remote node (measured: NIC on node 4, receivers
    // floating onto remote cpus 18/31/83/95 → cross-NUMA, per-core throughput cap).
    let nic_node = crate::autodetect::numa_node_for_iface(iface);
    let local_cpus = nic_local_logical_cpus(iface);
    if let Some(node) = nic_node {
        tracing::info!("[XDP] NIC node {}, local cpus {:?}", node, local_cpus);
    }

    // One busy-poll RX+TX worker per NIC-local PHYSICAL core. Binding one XSK
    // per HW queue (often = num CPUs, e.g. 40) oversubscribes the few NIC-local
    // cores the workers are pinned to (measured: 40 workers on 10 cores = 265k
    // qps vs 1.3M with 10). Cap to the local physical-core count; this needs no
    // `ethtool -L` (reconfiguring channels around an active ZC bind wedges the
    // ixgbe queue state until a module reload).
    // Spread workers across ALL physical cores, NUMA-local node first (the X520
    // can fan RSS to many queues, but one busy-poll worker per PHYSICAL core is
    // the stable point; >1 worker/core overdrives the ixgbe ZC datapath and
    // collapses throughput). Cross-NUMA cores are used only after local ones.
    // Worker core pool = NIC-LOCAL logical CPUs (physical cores first, then their
    // HT siblings — see nic_local_logical_cpus ordering). Cap per NIC to the
    // physical-core count: a single X520 port is link-bound at ~that many busy
    // workers. For dual fibre the global cursor gives NIC1 the physical cores and
    // NIC2 their HT siblings — both stay on the NIC-local NUMA node (cross-NUMA
    // node1 placement is QPI-bound and ~30% slower).
    // PHYSICAL cores only — NEVER HyperThread siblings: the ASM/SIMD wire path
    // saturates a physical core's execution units, so an HT-sibling worker steals
    // throughput instead of adding it. NIC-local NUMA node first, then at most
    // REMOTE_CORE_CAP cross-NUMA cores: the inter-socket QPI saturates beyond ~6
    // remote workers feeding a node-0 NIC, and past that the ixgbe ZC datapath
    // collapses (the dual-Xeon-v2 "16 = 10+6" limit, empirically verified).
    const REMOTE_CORE_CAP: usize = 6;
    let phys_sorted = crate::autodetect::physical_cores_numa_sorted(nic_node);
    let n_local = phys_sorted.iter()
        .filter(|&&c| crate::autodetect::numa_node_for_cpu(c) == nic_node)
        .count().max(1);
    let n_remote = phys_sorted.len().saturating_sub(n_local).min(REMOTE_CORE_CAP);
    let core_pool: Vec<usize> = phys_sorted.into_iter().take(n_local + n_remote).collect();
    let queue_count = (hw_queue_count as usize).min(n_local).max(1) as u32;
    tracing::info!("[XDP] {} HW queues, spawning {} worker(s) (NIC-local physical cores)", hw_queue_count, queue_count);

    for q in 0..queue_count {
        // Global cross-NIC cursor: NIC1 fills its node-local cores, NIC2 takes the
        // remaining cross-NUMA budget; stop once the 10+6 pool is spent (no wrap).
        let gi = GLOBAL_CORE_IDX.fetch_add(1, Ordering::Relaxed);
        if gi >= core_pool.len() { break; }
        let assigned_core = core_pool[gi];
        let mut sock = match unsafe { create_xsk_socket(ifidx, q, true) } {
            Ok(s) => { tracing::info!(queue = q, "AF_XDP bound ZERO-COPY"); s }
            Err(zc_err) => match unsafe { create_xsk_socket(ifidx, q, false) } {
                Ok(s) => { tracing::warn!(queue = q, %zc_err, "AF_XDP zero-copy FAILED — fell back to COPY mode (slow)"); s }
                Err(e) => return Err(format!("AF_XDP socket q={q}: {e}")),
            },
        };

        // Migrate this queue's UMEM to the NIC's local node (MAP_POPULATE faulted it
        // on the main thread, usually remote). Zero-copy DMA then stays NUMA-local.
        if let Some(node) = nic_node {
            mbind_to_node(sock.umem.area, sock.umem.area_len, node);
        }

        handle.register_socket(q, sock.fd)?;

        let ifl = in_flight.clone();
        let gif = global_in_flight.clone();
        let st  = stats.clone();
        let sd  = shutdown.clone();

        // ── Unified path (default when the engine set the config + IPv4 frame hdr) ──
        // One thread per queue owns the WHOLE socket and does RX+TX in one loop,
        // pinned to one NIC-local PHYSICAL core (lower half of local_cpus, 32-39).
        // No thread split, no Mutex, no HT sibling stealing the core.
        if let (Some(cfg), Some(hdr)) = (UNIFIED_CFG.get(), frame_hdr_opt.clone()) {
            let sa = SockaddrXdp {
                sxdp_family:         AF_XDP as u16,
                sxdp_flags:          0,
                sxdp_ifindex:        ifidx,
                sxdp_queue_id:       q,
                sxdp_shared_umem_fd: 0,
            };
            let core = Some(assigned_core);
            let wp  = cfg.wire_pool.clone();
            let qps = cfg.qps_per_worker.clone();
            let mo  = cfg.max_outstanding;
            let qc  = queue_count as usize;
            XDP_UNIFIED.store(true, Ordering::Relaxed);
            n_unified += 1;
            std::thread::Builder::new()
                .name(format!("xdp-worker-q{q}"))
                .spawn(move || {
                    if let Some(c) = core { pin_thread_to(c); }
                    xdp_unified_worker(sock, hdr, sa, wp, st, sd, qps, q as usize, qc, mo, timeout_dur);
                })
                .map_err(|e| format!("thread spawn: {e}"))?;
            continue;
        }

        // ── Legacy split path (no unified cfg, or IPv6): extract TX, RX-only recv ──
        if let Some(ref hdr) = frame_hdr_opt {
            let area = sock.umem.ptr_at(0);
            let fd   = sock.fd;
            let sa   = SockaddrXdp {
                sxdp_family:         AF_XDP as u16,
                sxdp_flags:          0,
                sxdp_ifindex:        ifidx,
                sxdp_queue_id:       q,
                sxdp_shared_umem_fd: 0,
            };
            let (tx_ring, comp_ring, tx_pool) = sock.extract_tx();
            tx_states.push(Arc::new(XdpTxState {
                fd,
                tx:   Mutex::new(tx_ring),
                comp: Mutex::new(comp_ring),
                pool: Mutex::new(tx_pool),
                hdr:  hdr.clone(),
                sa,
                area,
            }));
        }

        // Pin the receiver to a NIC-local HT sibling (upper half of local_cpus).
        let recv_cpu = if local_cpus.len() >= 2 {
            let half = local_cpus.len() / 2;
            Some(local_cpus[half + (q as usize % half)])
        } else {
            None
        };

        std::thread::Builder::new()
            .name(format!("xdp-recv-q{q}"))
            .spawn(move || {
                if let Some(c) = recv_cpu { pin_thread_to(c); }
                xdp_receiver_thread(sock, ifl, gif, st, sd, timeout_dur)
            })
            .map_err(|e| format!("thread spawn: {e}"))?;
    }

    // Store TX states globally for sender workers to access (legacy path only).
    if !tx_states.is_empty() {
        XDP_TX_STATES.set(tx_states).ok();
        tracing::info!("[XDP TX] TX ring active on {} queue(s)", queue_count);
    }

    // Recalibrate qps_per_worker based on the number of workers actually spawned.
    // engine/mod.rs computed initial_qps_per_worker = total_qps / concurrent, but
    // queue_count may be < concurrent (e.g. 1-queue virtio-net with -c 8 gives 1
    // worker instead of 8 → each worker should get total_qps/1, not total_qps/8).
    if n_unified > 0 {
        if let Some(cfg) = UNIFIED_CFG.get() {
            let total = cfg.total_qps;
            if total > 0 {
                let per_worker = (total / n_unified as u64).max(1);
                cfg.qps_per_worker.store(per_worker, Ordering::Relaxed);
                tracing::info!(
                    "qps_per_worker recalibrated: {total}/{n_unified}={per_worker}"
                );
            }
        }
    }

    Ok(handle)
}

/// Find the outbound interface for a DNS server IP.
pub fn iface_for_benchmark(server: std::net::IpAddr) -> String {
    iface_for_server(server)
        .or_else(default_interface)
        .unwrap_or_else(|| "eth0".to_string())
}

/// Async wrapper: spawns the XDP sender OS thread (no receiver — XDP handles it).
/// Uses AF_XDP TX ring when available (set by start_xdp_receive_path),
/// otherwise falls back to sendmmsg on a regular UDP socket.
#[allow(clippy::too_many_arguments)]
pub async fn run_xdp_sender_worker(
    server_addr:      SocketAddr,
    query_source:     Arc<dyn QuerySource>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_ms:       u64,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    worker_id:        usize,
    max_outstanding:  usize,
    global_in_flight: Arc<AtomicUsize>,
    xdp_in_flight:    Arc<InFlight>,
    global_id:        Arc<AtomicU16>,
    wire_pool:        Arc<WireQueryPool>,
    num_workers:      usize,
) {
    // Unified workers (spawned in start_xdp_receive_path, one per queue/core) own
    // the whole datapath. This engine task then has nothing to do — return.
    if XDP_UNIFIED.load(Ordering::Relaxed) {
        return;
    }

    let _ = timeout_ms;
    let _ = &query_source;
    let _ = &global_id;

    // AF_XDP zero-copy TX is now the DEFAULT (per-worker, lock-free): the NIC DMAs
    // straight from the UMEM, no kernel/sendmmsg. DNSMARK_XDP_TX=0 forces the
    // sendmmsg fallback (kernel-capped ~800k). Each worker owns a disjoint DNS-id
    // range + a private template cursor — zero shared per-packet state, scales per core.
    let use_xdp_tx = std::env::var("DNSMARK_XDP_TX").map(|v| v != "0").unwrap_or(true);
    if use_xdp_tx {
      if let Some(states) = XDP_TX_STATES.get() {
        if !states.is_empty() {
            let state = states[worker_id % states.len()].clone();
            let ifl   = xdp_in_flight;
            let gif   = global_in_flight;
            let wp    = wire_pool;
            let st    = stats;
            let sd    = shutdown;
            let qps   = qps_per_worker;
            let sender = std::thread::spawn(move || {
                super::super::pin_to_cpu(worker_id);
                xdp_tx_sender_thread(state, ifl, gif, wp, worker_id, num_workers, st, sd, qps, verbose, max_outstanding);
            });
            tokio::task::spawn_blocking(move || { sender.join().ok(); }).await.ok();
            return;
        }
      }
    }

    // Fallback (default): regular UDP socket + sendmmsg TX, AF_XDP RX stays active.
    let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { tracing::error!("XDP sender bind: {e}"); return; }
    };
    if let Err(e) = socket.connect(server_addr) {
        tracing::error!("XDP sender connect: {e}"); return;
    }
    let fd = {
        use std::os::unix::io::AsRawFd;
        socket.as_raw_fd()
    };
    let ifl  = xdp_in_flight;
    let gif  = global_in_flight;
    let gid  = global_id;
    let qs   = query_source;
    let st   = stats;
    let sd   = shutdown;
    let qps  = qps_per_worker;
    let sender = std::thread::spawn(move || {
        super::super::pin_to_cpu(worker_id);
        let _sock = socket;
        xdp_sender_thread(fd, ifl, gif, gid, qs, st, sd, qps, verbose, max_outstanding);
    });
    tokio::task::spawn_blocking(move || { sender.join().ok(); }).await.ok();
}
