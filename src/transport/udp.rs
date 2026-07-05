use std::io;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crate::query::{WireQueryPool, MAX_QUERY};
use crate::stats::StatsCollector;

/// Datagrams received per recvmmsg(2) syscall.
const RECV_BATCH: usize = 64;
/// Maximum DNS-over-UDP packet size we accept.
const MAX_MSG_SIZE: usize = 512;
/// Timeout sweep interval.
const TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_millis(10);

// ─── Per-worker in-flight table ───────────────────────────────────────────────

// Legacy per-worker open-addressed table — used only by `unified_udp_worker` (the old
// single-thread closed loop, superseded by the dnsperf-modelled two-thread path) and its
// unit tests. Kept for reference / the tests.
#[allow(dead_code)]
struct InFlight {
    slots: Vec<(u16, u64)>, // (query_id, send_time_ns); 0 = empty
}

impl InFlight {
    fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(256);
        Self { slots: vec![(0, 0); cap] }
    }

    /// Insert a new in-flight entry.
    ///
    /// Returns `Some(evicted_age_us)` if the slot was occupied by a **different** id
    /// (hash collision in flood mode). The caller must account for it as a timeout.
    /// Returns `None` in the common case (empty slot or same id re-sent).
    #[inline]
    fn insert(&mut self, id: u16, now_ns: u64) -> Option<u64> {
        let idx = (id as usize) & (self.slots.len() - 1);
        let slot = &mut self.slots[idx];
        let evicted = if slot.1 != 0 && slot.0 != id {
            // Collision: a different query occupies this slot. Evict it explicitly.
            Some(now_ns.saturating_sub(slot.1) / 1000)
        } else {
            None
        };
        *slot = (id, now_ns);
        evicted
    }

    #[inline]
    fn take(&mut self, id: u16, now_ns: u64) -> Option<u64> {
        let idx = (id as usize) & (self.slots.len() - 1);
        let slot = &mut self.slots[idx];
        if slot.1 != 0 && slot.0 == id {
            let rtt_ns = now_ns.saturating_sub(slot.1);
            *slot = (0, 0);
            Some(rtt_ns)
        } else {
            None
        }
    }

    fn sweep(&mut self, now_ns: u64, timeout_ns: u64) -> Vec<u64> {
        let mut expired = Vec::new();
        for slot in self.slots.iter_mut() {
            if slot.1 != 0 && now_ns.saturating_sub(slot.1) >= timeout_ns {
                expired.push(now_ns.saturating_sub(slot.1) / 1000);
                *slot = (0, 0);
            }
        }
        expired
    }

    fn drain(&mut self, now_ns: u64) -> Vec<u64> {
        let mut ages = Vec::new();
        for slot in self.slots.iter_mut() {
            if slot.1 != 0 {
                ages.push(now_ns.saturating_sub(slot.1) / 1000);
                *slot = (0, 0);
            }
        }
        ages
    }
}

// ─── Shared (TX/RX-split) in-flight table ──────────────────────────────────────
//
// One AtomicU64 slot per 16-bit DNS id (65536 slots) → the id IS the index, so a
// lock-free store/swap needs no id tag and never collides. The TX thread stores the
// send timestamp; the RX thread swaps it out to compute the RTT. This lets TX flood at
// full sendmmsg speed on one core while a dedicated RX core drains responses promptly
// (accurate RTT + completion count) — fixing the ramp's ~440k self-cap (#14) and the RX
// under-count (#5), both of which came from draining RX in the TX thread.
struct SharedInFlight {
    slots: Box<[AtomicU64]>,
}

impl SharedInFlight {
    fn new() -> Self {
        Self { slots: (0..65536).map(|_| AtomicU64::new(0)).collect() }
    }
    /// Record the send time for `id`. `send_ns` is forced non-zero (0 = empty slot);
    /// the 1 ns floor is far below measurement resolution.
    #[inline]
    fn insert(&self, id: u16, send_ns: u64) {
        self.slots[id as usize].store(send_ns.max(1), Ordering::Relaxed);
    }
    /// Match a response `id` to its send time, returning the RTT (ns) or None if the slot
    /// was empty (already taken, or the id was reused before the response arrived).
    #[inline]
    fn take(&self, id: u16, recv_ns: u64) -> Option<u64> {
        let s = self.slots[id as usize].swap(0, Ordering::Relaxed);
        if s != 0 { Some(recv_ns.saturating_sub(s)) } else { None }
    }
    /// Expire entries older than `timeout_ns` (sent before `now_ns - timeout_ns`); returns
    /// how many were cleared. Used by the closed-loop recv thread to count timed-out queries
    /// as losses and free their outstanding slots (mirrors dnsperf's process_timeouts).
    fn sweep(&self, now_ns: u64, timeout_ns: u64) -> usize {
        let mut n = 0;
        for s in self.slots.iter() {
            let v = s.load(Ordering::Relaxed);
            if v != 0 && now_ns.saturating_sub(v) >= timeout_ns
                && s.compare_exchange(v, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok()
            { n += 1; }
        }
        n
    }
}

// ─── Unified worker ───────────────────────────────────────────────────────────
//
// Single thread: send → poll(until next send) → recv → repeat.
// Identical structure to dnsperf's main loop:
//   1. If send slot available (rate or outstanding): sendmsg(), timestamp BEFORE.
//   2. poll(POLLIN, µs_until_next_send) — wakes immediately on response or at deadline.
//   3. recvmmsg(DONTWAIT) — drain all pending.
//   4. Sweep timeouts every 10ms.
//
// This eliminates the sender→receiver context-switch (+34µs) because RTT is
// measured start-to-finish in the same thread on the same clock.

#[allow(clippy::too_many_arguments, dead_code)]
fn unified_udp_worker(
    fd: i32,
    wire_pool: Arc<WireQueryPool>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
    max_outstanding: usize,
    global_in_flight: Arc<AtomicUsize>,
    timeout_dur: Duration,
) {
    let timeout_ns = timeout_dur.as_nanos() as u64;
    let base = Instant::now();

    let mut in_flight = InFlight::new(max_outstanding.max(1024));
    // #noxdp-perf: PER-WORKER in-flight gate. Was a GLOBAL atomic shared across all
    // workers, so --max-outstanding=100 meant ~100/N_workers each (~5 on a 20-worker
    // host) — a starved closed loop (measured 1845 qps). Per-worker matches dnsperf's
    // per-client -q and keeps the hot path free of a contended shared atomic.
    let mut local_inflight: usize = 0;
    let mut next_id: u16 = rand::random();
    let mut tmpl_idx: usize = rand::random();

    let mut last_qps: u64 = 0;
    let mut send_interval = Duration::ZERO;
    let mut next_send = Instant::now();
    let mut last_timeout_check = Instant::now();

    let mut single_buf = [0u8; MAX_QUERY];

    // recvmmsg buffers — allocated once
    let mut flat_buf: Vec<u8> = vec![0u8; RECV_BATCH * MAX_MSG_SIZE];
    let mut iovecs: Vec<libc::iovec> = (0..RECV_BATCH)
        .map(|i| libc::iovec {
            iov_base: unsafe {
                flat_buf.as_mut_ptr().add(i * MAX_MSG_SIZE) as *mut libc::c_void
            },
            iov_len: MAX_MSG_SIZE,
        })
        .collect();
    let mut msgs: Vec<libc::mmsghdr> = iovecs
        .iter_mut()
        .map(|iov| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov = iov as *mut libc::iovec;
            m.msg_hdr.msg_iovlen = 1;
            m
        })
        .collect();

    // ── pollfd for waiting on responses ──────────────────────────────────────
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        let qps = qps_per_worker.load(Ordering::Relaxed);
        let now = Instant::now();

        // ── Update rate-limiter state ─────────────────────────────────────────
        if qps != last_qps {
            send_interval = if qps > 0 {
                Duration::from_secs_f64(1.0 / qps as f64)
            } else {
                Duration::ZERO
            };
            next_send = now;
            last_qps = qps;
        }

        // ── 1. Send if slot available ─────────────────────────────────────────
        let outstanding_ok = if max_outstanding > 0 {
            local_inflight < max_outstanding
        } else {
            true
        };

        if outstanding_ok {
            let should_send = qps == 0 || now >= next_send;
            if should_send {
                // Timestamp BEFORE sendmsg — identical to dnsperf's gettimeofday() point.
                let send_ns = base.elapsed().as_nanos() as u64;
                let qlen = wire_pool.write_with_index(tmpl_idx, next_id, &mut single_buf);
                tmpl_idx = tmpl_idx.wrapping_add(1);

                let ret = unsafe {
                    libc::send(fd, single_buf.as_ptr() as *const libc::c_void, qlen, libc::MSG_DONTWAIT)
                };
                if ret >= 0 {
                    // insert() returns Some(_) if a different query was evicted from this
                    // slot (flood mode, table full). Count it as a timeout — a loss, not a
                    // completion — so `lost` reflects it and `sent == completed + lost`.
                    if in_flight.insert(next_id, send_ns).is_some() {
                        stats.inc_timeout();
                        global_in_flight.fetch_sub(1, Ordering::Relaxed); local_inflight = local_inflight.saturating_sub(1);
                    }
                    global_in_flight.fetch_add(1, Ordering::Relaxed);
                    local_inflight += 1;
                    stats.inc_sent();
                    if verbose { tracing::debug!(id = next_id, "sent query"); }
                    next_id = next_id.wrapping_add(1);
                    if qps > 0 {
                        next_send += send_interval;
                        // Cap: never burst-catch-up after a stall
                        if next_send < now { next_send = now + send_interval; }
                    }
                } else {
                    let e = io::Error::last_os_error();
                    if e.kind() != io::ErrorKind::WouldBlock {
                        tracing::debug!("UDP send: {}", e);
                        stats.inc_error();
                    }
                }
            }
        }

        // ── 2. poll(POLLIN, timeout = µs_until_next_send) ────────────────────
        //
        // This is the key: poll wakes immediately when a response arrives,
        // so RTT = recv_time - send_time measured in the same thread.
        // Timeout = time until next send opportunity — so we never overshoot.
        // In unlimited mode (qps=0): poll(0) = non-blocking check.
        {
            let poll_us: i64 = if qps > 0 && now < next_send {
                (next_send - now).as_micros() as i64
            } else {
                0
            };

            if poll_us >= 1000 {
                // poll() resolution is ms — use it for the bulk of the wait
                let poll_ms = (poll_us / 1000) as i32;
                unsafe { libc::poll(&mut pfd, 1, poll_ms); }
            } else if poll_us > 0 {
                // Sub-ms: busy-spin until deadline or response
                let deadline = now + Duration::from_micros(poll_us as u64);
                loop {
                    // Quick non-blocking recv peek
                    let mut peek: libc::mmsghdr = unsafe { std::mem::zeroed() };
                    let mut iov = iovecs[0];
                    peek.msg_hdr.msg_iov = &mut iov;
                    peek.msg_hdr.msg_iovlen = 1;
                    let r = unsafe {
                        libc::recvmmsg(fd, &mut peek, 1, libc::MSG_DONTWAIT as _, std::ptr::null_mut())
                    };
                    if r > 0 {
                        // Got a response early — handle it immediately
                        let recv_ns = base.elapsed().as_nanos() as u64;
                        let len = peek.msg_len as usize;
                        if len >= 2 {
                            let buf = &flat_buf[0..len];
                            let id = u16::from_be_bytes([buf[0], buf[1]]);
                            let rcode = if len >= 4 { buf[3] & 0x0f } else { 0 };
                            if let Some(rtt_ns) = in_flight.take(id, recv_ns) {
                                stats.record_response(rcode, (rtt_ns / 1000).max(1));
                                global_in_flight.fetch_sub(1, Ordering::Relaxed); local_inflight = local_inflight.saturating_sub(1);
                            }
                        }
                        break;
                    }
                    if Instant::now() >= deadline { break; }
                    std::hint::spin_loop();
                }
            }
        }

        // ── 3. Drain all pending responses ───────────────────────────────────
        let n = unsafe {
            libc::recvmmsg(fd, msgs.as_mut_ptr(), RECV_BATCH as libc::c_uint,
                libc::MSG_DONTWAIT as _, std::ptr::null_mut())
        };
        if n > 0 {
            let recv_ns = base.elapsed().as_nanos() as u64;
            for i in 0..(n as usize) {
                let len = msgs[i].msg_len as usize;
                if len < 2 { continue; }
                let buf = &flat_buf[i * MAX_MSG_SIZE..i * MAX_MSG_SIZE + len];
                let id = u16::from_be_bytes([buf[0], buf[1]]);
                let rcode = if len >= 4 { buf[3] & 0x0f } else { 0 };
                if let Some(rtt_ns) = in_flight.take(id, recv_ns) {
                    let rtt_us = (rtt_ns / 1000).max(1);
                    stats.record_response(rcode, rtt_us);
                    global_in_flight.fetch_sub(1, Ordering::Relaxed); local_inflight = local_inflight.saturating_sub(1);
                    if verbose { tracing::debug!(id, rtt_us, rcode, "response"); }
                }
            }
        }

        // ── 4. Timeout sweep every 10ms ───────────────────────────────────────
        if now.duration_since(last_timeout_check) >= TIMEOUT_CHECK_INTERVAL {
            let now_ns = base.elapsed().as_nanos() as u64;
            let expired = in_flight.sweep(now_ns, timeout_ns);
            let n_exp = expired.len();
            // A timeout is a loss (no response arrived in time): count it, but do not
            // record it as a completion or as a latency sample. The histogram holds
            // real response latencies only.
            for _ in 0..n_exp {
                stats.inc_timeout();
            }
            if n_exp > 0 {
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
                    |x| Some(x.saturating_sub(n_exp))).ok();
                local_inflight = local_inflight.saturating_sub(n_exp);
            }
            last_timeout_check = now;
        }
    }

    // ── End of run: drain in-flight ───────────────────────────────────────────
    let now_ns = base.elapsed().as_nanos() as u64;
    let remaining = in_flight.drain(now_ns);
    let n_rem = remaining.len();
    // Queries still in flight when the run ends never got a response → losses.
    for _ in 0..n_rem {
        stats.inc_timeout();
    }
    if n_rem > 0 {
        global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |x| Some(x.saturating_sub(n_rem))).ok();
    }
}


// ─── Throughput worker (max_outstanding == 0 / flood mode) ───────────────────
//
// This path is active ONLY when max_outstanding == 0 (saturation/flood).
//
// Design: reduce syscall count per packet.
//   Closed-loop path (latency): ~3 syscalls/pkt — send()+poll()+recvmmsg()
//   Throughput path:            ~1/64 sendmmsg() + ~1/256 recvmmsg() = <<1/pkt
//
// Trade-offs (documented — do NOT use for latency measurement):
//   - send timestamp is per-batch, not per-query → p50/p99 marked "throughput"
//   - no poll() → recv is drained periodically, not on each response
//   - no global_in_flight gate → local counters only, zero shared state on hot path
//
// The closed-loop path (max_outstanding > 0) is BIT-FOR-BIT unchanged.

const TX_BATCH: usize = 64;         // sendmmsg batch size
const FLUSH_STATS: usize = 16;      // flush local counters to Arc<StatsCollector> every N batches

#[allow(clippy::too_many_arguments)]
fn throughput_udp_tx(
    fd: i32,
    wire_pool: Arc<WireQueryPool>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    qps_per_worker: Arc<AtomicU64>,
    track_latency: bool, // ramp: rate-pace to qps_per_worker + record send time. flood: false.
    in_flight: Arc<SharedInFlight>,
    base: Instant,
) {
    // TX buffers (allocated once)
    let mut tx_flat: Vec<u8> = vec![0u8; TX_BATCH * MAX_QUERY];
    let mut tx_iovecs: Vec<libc::iovec> = (0..TX_BATCH)
        .map(|i| libc::iovec {
            iov_base: unsafe { tx_flat.as_mut_ptr().add(i * MAX_QUERY) as *mut libc::c_void },
            iov_len:  MAX_QUERY,
        })
        .collect();
    let mut tx_msgs: Vec<libc::mmsghdr> = tx_iovecs
        .iter_mut()
        .map(|iov| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov    = iov as *mut libc::iovec;
            m.msg_hdr.msg_iovlen = 1;
            m
        })
        .collect();

    let mut local_sent: u64 = 0;
    let mut next_id:  u16   = rand::random();
    let mut tmpl_idx: usize = rand::random();
    let mut batch_ctr: usize = 0;
    let mut next_batch = Instant::now();
    let mut last_qps: u64 = 0;
    let mut batch_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // Pacing (ramp): is a batch due now?
        let mut due = true;
        if track_latency {
            let qps = qps_per_worker.load(Ordering::Relaxed);
            if qps != last_qps {
                batch_interval = if qps > 0 {
                    Duration::from_secs_f64(TX_BATCH as f64 / qps as f64)
                } else { Duration::ZERO };
                next_batch = Instant::now();
                last_qps = qps;
            }
            // qps==0 (burst phase) floods; qps>0 sends only when the next slot is due.
            due = qps == 0 || Instant::now() >= next_batch;
        }

        if due {
            let batch_start_id = next_id;
            for i in 0..TX_BATCH {
                let slot = &mut tx_flat[i * MAX_QUERY..(i + 1) * MAX_QUERY];
                let qlen = wire_pool.write_with_index(tmpl_idx, next_id, slot);
                tx_iovecs[i].iov_len  = qlen;
                tx_msgs[i].msg_hdr.msg_iov    = &mut tx_iovecs[i] as *mut libc::iovec;
                tx_msgs[i].msg_hdr.msg_iovlen = 1;
                tmpl_idx = tmpl_idx.wrapping_add(1);
                next_id  = next_id.wrapping_add(1);
            }

            // Timestamp just before the send syscall (per-batch granularity in ramp mode).
            let send_ns = if track_latency { base.elapsed().as_nanos() as u64 } else { 0 };
            let sent = unsafe {
                libc::sendmmsg(fd, tx_msgs.as_mut_ptr(), TX_BATCH as libc::c_uint, libc::MSG_DONTWAIT as _)
            };
            if sent > 0 {
                local_sent += sent as u64;
                if track_latency {
                    // Register each sent id with this batch's send time; the RX thread
                    // matches the response and records the RTT.
                    let mut id = batch_start_id;
                    for _ in 0..sent as usize {
                        in_flight.insert(id, send_ns);
                        id = id.wrapping_add(1);
                    }
                }
            }

            batch_ctr += 1;
            if track_latency && last_qps > 0 {
                next_batch += batch_interval;
                let now = Instant::now();
                if next_batch < now { next_batch = now; } // behind: no burst-catch-up
            }
        }

        if batch_ctr % FLUSH_STATS == 0 && local_sent > 0 {
            stats.inc_sent_n(local_sent as usize);
            local_sent = 0;
        }
    }
    if local_sent > 0 { stats.inc_sent_n(local_sent as usize); }
}

// ─── Throughput RX worker (dedicated drain thread, paired with the TX worker) ───
//
// Drains the socket continuously on its own core so responses are timestamped promptly
// (accurate RTT) and every response is counted (no under-count), regardless of how hard
// the TX thread floods. Blocks with a 200 ms recvmmsg timeout so it parks when idle yet
// still polls the shutdown flag.
fn throughput_udp_rx(
    fd: i32,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    in_flight: Arc<SharedInFlight>,
    track_latency: bool,
    base: Instant,
    timeout_ns: u64,
) {
    const RX_BATCH: usize = 256;
    let mut rx_flat: Vec<u8> = vec![0u8; RX_BATCH * MAX_MSG_SIZE];
    let mut rx_iovecs: Vec<libc::iovec> = (0..RX_BATCH)
        .map(|i| libc::iovec {
            iov_base: unsafe { rx_flat.as_mut_ptr().add(i * MAX_MSG_SIZE) as *mut libc::c_void },
            iov_len:  MAX_MSG_SIZE,
        })
        .collect();
    let mut rx_msgs: Vec<libc::mmsghdr> = rx_iovecs
        .iter_mut()
        .map(|iov| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov    = iov as *mut libc::iovec;
            m.msg_hdr.msg_iovlen = 1;
            m
        })
        .collect();

    let mut rc = [0u64; 5];
    let mut since_flush: usize = 0;
    let mut last_sweep = Instant::now();
    // Latency-hygiene horizon (ramp only): a slot older than this can't be a valid sample for a
    // primed server (sub-ms RTTs; the DSD stops the moment p50 exceeds a few × the floor), so a
    // reply that old is an id-reuse alias or an effective loss — not latency. Capping the
    // timeout at 200 ms clears such slots before they can alias the histogram, killing the
    // residual p99 spikes, without touching loss accounting (lost = sent − completed, computed
    // from RX rcode counts, is independent of these slots).
    let lat_horizon_ns = timeout_ns.min(200_000_000);

    // poll() before draining: its timeout is honoured even when zero datagrams arrive
    // (unlike recvmmsg's, which blocks forever on an empty queue), so the thread parks
    // when idle yet still polls the shutdown flag every 200 ms.
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    loop {
        if shutdown.load(Ordering::Relaxed) { break; }
        // Sweep stale in-flight slots every 10 ms (ramp/latency only). Without it, at LOW
        // per-worker rates the 16-bit id space wraps only every several SECONDS, so the slot
        // of a lost (or pre-measurement warm-up/prime) query lingers — and a late/duplicate
        // reply then matches it, recording a fictitious multi-hundred-ms / multi-second RTT
        // that poisons p95/p99. Sweeping at the timeout horizon clears those, so the tail
        // reflects real server+network latency (the fast id-wrap already self-cleans at high
        // rate, which is why only the low/mid steps were affected).
        if track_latency && last_sweep.elapsed() >= TIMEOUT_CHECK_INTERVAL {
            let now_ns = base.elapsed().as_nanos() as u64;
            let expired = in_flight.sweep(now_ns, lat_horizon_ns);
            for _ in 0..expired { stats.inc_timeout(); }
            last_sweep = Instant::now();
        }
        let pr = unsafe { libc::poll(&mut pfd, 1, 200) };
        if pr <= 0 { continue; } // timeout / EINTR → re-check shutdown
        let n = unsafe {
            libc::recvmmsg(fd, rx_msgs.as_mut_ptr(), RX_BATCH as libc::c_uint,
                libc::MSG_DONTWAIT as _, std::ptr::null_mut())
        };
        if n <= 0 { continue; }
        #[allow(clippy::needless_range_loop)]
        for i in 0..n as usize {
            let len = (rx_msgs[i].msg_len as usize).min(MAX_MSG_SIZE);
            let off = i * MAX_MSG_SIZE;
            let buf = &rx_flat[off..off + len];
            let idx = match crate::dns::response::parse_response(buf).map(|r| r.rcode) {
                Some(0) => 0, Some(3) => 1, Some(2) => 2, Some(5) => 3, _ => 4,
            };
            rc[idx] += 1;
            if track_latency && len >= 2 {
                let id = u16::from_be_bytes([buf[0], buf[1]]);
                let recv_ns = base.elapsed().as_nanos() as u64;
                if let Some(rtt_ns) = in_flight.take(id, recv_ns) {
                    // Reject implausibly old matches (id-reuse aliases / replies past the
                    // horizon): such a reply is an effective loss, not a latency sample.
                    if rtt_ns <= lat_horizon_ns {
                        stats.record_latency_us((rtt_ns / 1000).max(1));
                    } else {
                        stats.inc_timeout();
                    }
                }
            }
        }
        since_flush += 1;
        if since_flush >= FLUSH_STATS {
            stats.record_rcodes(rc[0], rc[1], rc[2], rc[3], rc[4]);
            rc = [0u64; 5];
            since_flush = 0;
        }
    }
    if rc.iter().any(|&x| x > 0) {
        stats.record_rcodes(rc[0], rc[1], rc[2], rc[3], rc[4]);
    }
}

// ─── Closed-loop datapath — dnsperf-faithful single thread per worker ────────────
//
// Clean-room reimplementation of how DNS-OARC dnsperf runs its kernel-UDP closed loop (NOT a
// copy): ONE thread per worker that, in a single loop, FILLS the pipe up to `max_outstanding`
// (dnsperf's -q gate, rate-limited to `qps_per_worker` when set) and then DRAINS replies with
// one batched `recvmmsg`, matching each by its DNS id (the id IS the index into a local slot
// table → O(1), no collisions). Because send and recv live in the same thread, the in-flight
// table and the outstanding counter are plain locals — no Arc/atomic bouncing a cache line per
// packet, and no second thread oversubscribing the cores. The earlier two-thread split paid
// both of those costs and topped out ~30 % under dnsperf at saturation; this matches it. RTTs
// past the timeout are id-reuse aliases / effective losses → counted, not put in the histogram.
#[allow(clippy::too_many_arguments)]
fn closed_loop_unified(
    fd: i32,
    wire_pool: Arc<WireQueryPool>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    qps_per_worker: Arc<AtomicU64>,
    max_outstanding: usize,
    timeout_dur: Duration,
    ramp: bool,
    verbose: bool,
) {
    let base = Instant::now();
    let timeout_ns = timeout_dur.as_nanos() as u64;
    // Local id-indexed in-flight table: the 16-bit DNS id is the slot index. u64 send-timestamp
    // (ns since `base`); 0 = empty. Single-thread-owned → no atomics.
    let mut slots: Vec<u64> = vec![0u64; 65536];
    let mut outstanding: usize = 0;

    let mut next_id: u16 = rand::random();
    let mut tmpl_idx: usize = rand::random();
    let mut sbuf = [0u8; MAX_QUERY];

    // Rate state. Fixed at a steady -Q run; in ramp mode the controller rewrites the target
    // each step, so we re-read it.
    let mut qps = qps_per_worker.load(Ordering::Relaxed);
    let mut step = if qps > 0 { Duration::from_secs_f64(1.0 / qps as f64) } else { Duration::ZERO };
    let mut next_send = Instant::now();

    const RX_BATCH: usize = 256;
    let mut rx_flat: Vec<u8> = vec![0u8; RX_BATCH * MAX_MSG_SIZE];
    let mut rx_iovecs: Vec<libc::iovec> = (0..RX_BATCH)
        .map(|i| libc::iovec {
            iov_base: unsafe { rx_flat.as_mut_ptr().add(i * MAX_MSG_SIZE) as *mut libc::c_void },
            iov_len:  MAX_MSG_SIZE,
        })
        .collect();
    let mut rx_msgs: Vec<libc::mmsghdr> = rx_iovecs.iter_mut()
        .map(|iov| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov = iov as *mut libc::iovec;
            m.msg_hdr.msg_iovlen = 1;
            m
        })
        .collect();

    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let mut last_sweep = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        if ramp {
            let q = qps_per_worker.load(Ordering::Relaxed);
            if q != qps {
                qps = q;
                step = if q > 0 { Duration::from_secs_f64(1.0 / q as f64) } else { Duration::ZERO };
                next_send = Instant::now();
            }
        }

        // ── SEND: fill the pipe up to the outstanding gate (rate-limited if qps>0) ──
        let mut did_send = false;
        while outstanding < max_outstanding {
            if qps > 0 && Instant::now() < next_send { break; } // not due yet
            let id = next_id; next_id = next_id.wrapping_add(1);
            let len = wire_pool.write_with_index(tmpl_idx, id, &mut sbuf);
            tmpl_idx = tmpl_idx.wrapping_add(1);
            let send_ns = base.elapsed().as_nanos() as u64; // timestamp just before send (dnsperf point)
            let r = unsafe {
                libc::send(fd, sbuf.as_ptr() as *const libc::c_void, len, libc::MSG_DONTWAIT)
            };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::WouldBlock { stats.inc_error(); }
                break; // sndbuf full → go drain, retry next iteration
            }
            slots[id as usize] = send_ns.max(1);
            outstanding += 1;
            stats.inc_sent();
            did_send = true;
            if qps > 0 {
                next_send += step;
                // Allow bounded catch-up: when we fall a little behind (loop/recv jitter at high
                // per-worker rates), the while-loop sends several queued slots in a row to hold
                // the target rate — dnsperf does the same, and it's what keeps offered ≈ -Q
                // instead of drifting low. Only a real stall (>2 ms behind) resets the schedule
                // so we don't unleash a huge burst; the outstanding gate bounds it regardless.
                let now = Instant::now();
                if next_send + Duration::from_millis(2) < now { next_send = now; }
            }
        }

        // ── RECV: drain replies in one batched syscall, match by id ──
        let n = unsafe {
            libc::recvmmsg(fd, rx_msgs.as_mut_ptr(), RX_BATCH as libc::c_uint,
                libc::MSG_DONTWAIT as _, std::ptr::null_mut())
        };
        if n > 0 {
            let recv_ns = base.elapsed().as_nanos() as u64;
            #[allow(clippy::needless_range_loop)]
            for i in 0..n as usize {
                let len = (rx_msgs[i].msg_len as usize).min(MAX_MSG_SIZE);
                if len < 4 { continue; }
                let off = i * MAX_MSG_SIZE;
                let buf = &rx_flat[off..off + len];
                let id = u16::from_be_bytes([buf[0], buf[1]]);
                let rcode = buf[3] & 0x0f;
                let s = slots[id as usize];
                if s != 0 {
                    slots[id as usize] = 0;
                    outstanding = outstanding.saturating_sub(1);
                    let rtt_ns = recv_ns.saturating_sub(s);
                    if rtt_ns <= timeout_ns {
                        stats.record_response(rcode, (rtt_ns / 1000).max(1));
                    } else {
                        // late / id-reuse alias: count the reply, but its RTT is not a sample.
                        match rcode {
                            0 => stats.record_rcodes(1, 0, 0, 0, 0),
                            3 => stats.record_rcodes(0, 1, 0, 0, 0),
                            2 => stats.record_rcodes(0, 0, 1, 0, 0),
                            5 => stats.record_rcodes(0, 0, 0, 1, 0),
                            _ => stats.record_rcodes(0, 0, 0, 0, 1),
                        }
                    }
                    if verbose { tracing::debug!(id, rcode, "response"); }
                }
            }
        }

        // ── Timeout sweep every 10 ms: unanswered queries are losses; free their slots ──
        if last_sweep.elapsed() >= TIMEOUT_CHECK_INTERVAL {
            let now_ns = base.elapsed().as_nanos() as u64;
            let mut freed = 0usize;
            for s in slots.iter_mut() {
                if *s != 0 && now_ns.saturating_sub(*s) >= timeout_ns { *s = 0; freed += 1; }
            }
            if freed > 0 {
                for _ in 0..freed { stats.inc_timeout(); }
                outstanding = outstanding.saturating_sub(freed);
            }
            last_sweep = Instant::now();
        }

        // ── Idle handling when nothing was sent and nothing arrived ──
        if !did_send && n <= 0 {
            if qps > 0 {
                // Rate-gated: spin (no syscall) and loop — the recvmmsg at the top of the next
                // pass drains replies promptly, so pacing stays exact AND latency stays tight. A
                // poll() here has a 1 ms floor that throttles sub-ms pacing to ~1k q/s/worker.
                std::hint::spin_loop();
            } else {
                // Unlimited, gate full, nothing arrived yet: wait for a reply (don't busy-spin).
                unsafe { libc::poll(&mut pfd, 1, 1); }
            }
        }
    }
}

// ─── Public async entry point ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_udp_worker(
    server_addr: SocketAddr,
    wire_pool: Arc<WireQueryPool>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_ms: u64,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
    worker_id: usize,
    max_outstanding: usize,
    ramp: bool,
    global_in_flight: Arc<AtomicUsize>,
) {
    let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { tracing::error!("bind UDP socket: {}", e); return; }
    };
    if let Err(e) = socket.connect(server_addr) {
        tracing::error!("connect UDP socket to {}: {}", server_addr, e);
        return;
    }
    unsafe {
        let buf: libc::c_int = 8 * 1024 * 1024;
        let sz = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let fd = socket.as_raw_fd();
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, &buf as *const _ as *const libc::c_void, sz);
        libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, &buf as *const _ as *const libc::c_void, sz);
    }
    let fd = socket.as_raw_fd();
    let timeout_dur = Duration::from_millis(timeout_ms);

    tokio::task::spawn_blocking(move || {
        let _sock = socket;
        if max_outstanding == 0 {
            // Flood / ramp: split TX and RX across two threads so the TX thread floods at
            // full sendmmsg speed on its pinned core while a dedicated RX thread drains
            // responses promptly (accurate RTT + completion count). Draining RX in the TX
            // thread capped the ramp at ~440k (#14) and under-counted completions (#5); the
            // split lets the ramp reach the server's real saturation knee with a p50 SLO.
            let in_flight = Arc::new(SharedInFlight::new());
            let base = Instant::now(); // single clock shared by TX send_ns and RX recv_ns
            let track = ramp;
            let rx_if = Arc::clone(&in_flight);
            let rx_stats = Arc::clone(&stats);
            let rx_shutdown = Arc::clone(&shutdown);
            let rx_timeout_ns = timeout_dur.as_nanos() as u64;
            let rx = std::thread::spawn(move || {
                // RX floats (unpinned): it is lighter than TX and the OS keeps it off the
                // TX core; a fixed pin would risk colliding with another worker's TX core.
                throughput_udp_rx(fd, rx_stats, rx_shutdown, rx_if, track, base, rx_timeout_ns);
            });
            super::pin_to_cpu(worker_id);
            throughput_udp_tx(fd, wire_pool, stats, shutdown, qps_per_worker, track, in_flight, base);
            let _ = rx.join();
        } else {
            // Closed-loop / latency mode — dnsperf-faithful SINGLE thread per worker: send-fill
            // up to the outstanding gate, then one batched recvmmsg drain, in one loop. No
            // second thread and no shared atomics (the in-flight table + outstanding counter are
            // locals), which is what lets it match dnsperf's saturation throughput — the old
            // two-thread split paid a cache-line bounce per packet and oversubscribed the cores,
            // topping out ~30 % low. `global_in_flight` unused (gate = the local outstanding).
            let _ = &global_in_flight;
            super::pin_to_cpu(worker_id);
            closed_loop_unified(fd, wire_pool, stats, shutdown, qps_per_worker,
                max_outstanding, timeout_dur, ramp, verbose);
        }
    }).await.ok();
}

#[cfg(test)]
mod inflight_tests {
    use super::*;

    #[test]
    fn no_eviction_empty_slot() {
        let mut ifl = InFlight::new(64);
        // Insert into empty slot → no eviction
        let evicted = ifl.insert(42, 1000);
        assert!(evicted.is_none(), "empty slot must not evict");
    }

    #[test]
    fn no_eviction_same_id_retry() {
        let mut ifl = InFlight::new(64);
        ifl.insert(42, 1_000_000); // t=1ms (never use 0 — sentinel for "empty")
        // Same id re-sent (retry) → no eviction
        let evicted = ifl.insert(42, 2_000_000);
        assert!(evicted.is_none(), "same-id re-insert must not count as eviction");
        // take should use the latest timestamp
        let rtt = ifl.take(42, 3_000_000).unwrap();
        assert_eq!(rtt, 1_000_000, "rtt = 3ms - 2ms = 1ms in ns");
    }

    #[test]
    fn eviction_collision_flood_mode() {
        // Table size 256; id 0 and id 256 map to slot 0 (x & 255).
        // Use non-zero timestamps — 0 is the "empty" sentinel.
        let mut ifl = InFlight::new(256);
        ifl.insert(0, 1_000_000); // slot 0 ← id=0 at t=1ms
        // id 256 collides at t=3ms → evicts id 0, age = (3ms-1ms) = 2ms = 2000µs
        let evicted = ifl.insert(256, 3_000_000);
        assert!(evicted.is_some(), "collision must return evicted age");
        assert_eq!(evicted.unwrap(), 2000, "evicted age: (3ms-1ms)/1µs = 2000µs");
        // id 0 gone
        assert!(ifl.take(0, 4_000_000).is_none(), "evicted id must not be takeable");
        // id 256 present
        assert!(ifl.take(256, 4_000_000).is_some(), "new id must be present");
    }

    #[test]
    fn controlled_rate_zero_eviction() {
        // Sequential ids 1..=64 (skip 0, sentinel), table=1024 → no collisions.
        let mut ifl = InFlight::new(1024);
        for id in 1u16..=64 {
            let evicted = ifl.insert(id, id as u64 * 1_000_000);
            assert!(evicted.is_none(), "sequential ids must not collide (id={})", id);
        }
        for id in 1u16..=64 {
            assert!(ifl.take(id, 100_000_000).is_some(), "id {} must be present", id);
        }
    }

    #[test]
    fn drain_returns_all_ages() {
        let mut ifl = InFlight::new(64);
        ifl.insert(1, 1_000_000); // t=1ms
        ifl.insert(2, 2_000_000); // t=2ms
        let ages = ifl.drain(5_000_000); // t=5ms
        assert_eq!(ages.len(), 2, "both entries must be drained");
        // Slots cleared — take must return None
        assert!(ifl.take(1, 6_000_000).is_none());
        assert!(ifl.take(2, 6_000_000).is_none());
    }

    #[test]
    fn sweep_expires_old_entries() {
        let mut ifl = InFlight::new(64);
        ifl.insert(10, 1_000_000); // t=1ms
        ifl.insert(11, 3_000_000); // t=3ms
        // sweep at t=6ms, timeout=4ms → id10 (age 5ms ≥ 4ms) expires; id11 (age 3ms) survives
        let expired = ifl.sweep(6_000_000, 4_000_000);
        assert_eq!(expired.len(), 1, "one entry must expire");
        assert_eq!(expired[0], 5000, "age = (6ms-1ms) = 5000µs");
        assert!(ifl.take(11, 7_000_000).is_some(), "non-expired entry must remain");
    }
}
