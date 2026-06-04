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

struct InFlight {
    slots: Vec<(u16, u64)>, // (query_id, send_time_ns); 0 = empty
    count: usize,
}

impl InFlight {
    fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(256);
        Self { slots: vec![(0, 0); cap], count: 0 }
    }

    #[inline]
    fn insert(&mut self, id: u16, now_ns: u64) {
        let idx = (id as usize) & (self.slots.len() - 1);
        if self.slots[idx].1 == 0 { self.count += 1; }
        self.slots[idx] = (id, now_ns);
    }

    #[inline]
    fn take(&mut self, id: u16, now_ns: u64) -> Option<u64> {
        let idx = (id as usize) & (self.slots.len() - 1);
        let slot = &mut self.slots[idx];
        if slot.1 != 0 && slot.0 == id {
            let rtt_ns = now_ns.saturating_sub(slot.1);
            *slot = (0, 0);
            self.count -= 1;
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
                self.count -= 1;
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
        self.count = 0;
        ages
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

#[allow(clippy::too_many_arguments)]
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
            global_in_flight.load(Ordering::Relaxed) < max_outstanding
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
                    in_flight.insert(next_id, send_ns);
                    global_in_flight.fetch_add(1, Ordering::Relaxed);
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
                                global_in_flight.fetch_sub(1, Ordering::Relaxed);
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
                    global_in_flight.fetch_sub(1, Ordering::Relaxed);
                    if verbose { tracing::debug!(id, rtt_us, rcode, "response"); }
                }
            }
        }

        // ── 4. Timeout sweep every 10ms ───────────────────────────────────────
        if now.duration_since(last_timeout_check) >= TIMEOUT_CHECK_INTERVAL {
            let now_ns = base.elapsed().as_nanos() as u64;
            let expired = in_flight.sweep(now_ns, timeout_ns);
            let n_exp = expired.len();
            for age_us in expired {
                stats.record_response(0xff, age_us);
                stats.inc_timeout();
            }
            if n_exp > 0 {
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
                    |x| Some(x.saturating_sub(n_exp))).ok();
            }
            last_timeout_check = now;
        }
    }

    // ── End of run: drain in-flight ───────────────────────────────────────────
    let now_ns = base.elapsed().as_nanos() as u64;
    let remaining = in_flight.drain(now_ns);
    let n_rem = remaining.len();
    for age_us in remaining {
        stats.record_response(0xff, age_us);
        stats.inc_timeout();
    }
    if n_rem > 0 {
        global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |x| Some(x.saturating_sub(n_rem))).ok();
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
        super::pin_to_cpu(worker_id);
        let _sock = socket;
        unified_udp_worker(fd, wire_pool, stats, shutdown, qps_per_worker,
            verbose, max_outstanding, global_in_flight, timeout_dur);
    }).await.ok();
}
