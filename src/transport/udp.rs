use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

/// Datagrams sent per sendmmsg(2) syscall in unlimited mode.
const BATCH_SIZE: usize = 64;
/// Datagrams received per recvmmsg(2) syscall.
const RECV_BATCH: usize = 16;
/// Maximum DNS-over-UDP packet size we accept.
const MAX_MSG_SIZE: usize = 512;

// ─── sendmmsg helper ────────────────────────────────────────────────────────

fn sendmmsg_batch(fd: i32, bufs: &[Vec<u8>]) -> io::Result<usize> {
    if bufs.is_empty() {
        return Ok(0);
    }
    let mut iovecs: Vec<libc::iovec> = bufs
        .iter()
        .map(|b| libc::iovec {
            iov_base: b.as_ptr() as *mut libc::c_void,
            iov_len: b.len(),
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
    let ret = unsafe {
        libc::sendmmsg(fd, msgs.as_mut_ptr(), msgs.len() as libc::c_uint, libc::MSG_DONTWAIT as _)
    };
    if ret < 0 { Err(io::Error::last_os_error()) } else { Ok(ret as usize) }
}

// ─── Sender OS thread ────────────────────────────────────────────────────────
//
// Rate-limited: drift-compensating sleep via nanosleep (std::thread::sleep).
//   RTT timer starts at the actual send() call, matching dnsperf behaviour.
//   No semaphore — the send rate itself is the natural back-pressure.
//
// Unlimited: sendmmsg(BATCH_SIZE) with MSG_DONTWAIT; brief sleep on WouldBlock.

#[allow(clippy::too_many_arguments)]
fn sender_thread(
    fd: i32,
    in_flight: Arc<Mutex<HashMap<u16, Instant>>>,
    query_source: Arc<dyn QuerySource>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
    max_outstanding: usize,
) {
    let mut next_id: u16 = rand::random();
    let mut next_send = Instant::now();
    let mut last_qps: u64 = 0;
    let mut send_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let qps = qps_per_worker.load(Ordering::Relaxed);

        if qps > 0 {
            // Reset deadline when QPS target changes (ramp step-up or transition
            // from burst/unlimited back to rate-limited).
            if qps != last_qps {
                send_interval = Duration::from_secs_f64(1.0 / qps as f64);
                next_send = Instant::now();
                last_qps = qps;
            }

            // Back-pressure: if too many queries are outstanding (server behind),
            // skip this send slot and let the receiver drain responses first.
            // Mirrors dnsperf's -q (max outstanding per client) behaviour.
            if max_outstanding > 0 && in_flight.lock().len() >= max_outstanding {
                next_send = Instant::now() + send_interval;
                std::thread::sleep(Duration::from_micros(500));
                continue;
            }

            // Sleep until the next absolute send deadline. Using an absolute
            // deadline means timer overshoot on one iteration (nanosleep
            // fires a bit late) is recovered by a shorter sleep on the next,
            // keeping long-run rate accurate — the same drift-correction
            // as dnsperf's req_time += q_step approach.
            let now = Instant::now();
            if now < next_send {
                std::thread::sleep(next_send - now);
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
            }
            next_send += send_interval;
            // Cap to avoid burst catch-up after a long stall (e.g. no qps slot).
            if next_send < Instant::now() {
                next_send = Instant::now();
            }

            // Build and send; RTT clock starts at actual send() call.
            let entry = query_source.next();
            let qbytes = build_query(next_id, &entry.name, entry.qtype);
            let ret = unsafe {
                libc::send(fd, qbytes.as_ptr() as *const libc::c_void, qbytes.len(), 0)
            };
            if ret >= 0 {
                in_flight.lock().insert(next_id, Instant::now());
                stats.inc_sent();
                if verbose {
                    tracing::debug!(id = next_id, name = %entry.name, "sent query");
                }
                next_id = next_id.wrapping_add(1);
            } else {
                tracing::debug!("UDP send error: {}", io::Error::last_os_error());
                stats.inc_error();
            }
        } else {
            // Unlimited mode: sendmmsg batch, no rate limit.
            // Cap batch to respect max_outstanding: compute headroom once per
            // iteration so we don't spike far past the limit between checks.
            let batch_cap = if max_outstanding > 0 {
                let current = in_flight.lock().len();
                if current >= max_outstanding {
                    std::thread::sleep(Duration::from_micros(500));
                    last_qps = 0;
                    continue;
                }
                (max_outstanding - current).min(BATCH_SIZE)
            } else {
                BATCH_SIZE
            };

            let mut batch_bufs: Vec<Vec<u8>> = Vec::with_capacity(batch_cap);
            let mut batch_ids: Vec<u16> = Vec::with_capacity(batch_cap);
            for _ in 0..batch_cap {
                let entry = query_source.next();
                batch_bufs.push(build_query(next_id, &entry.name, entry.qtype));
                batch_ids.push(next_id);
                next_id = next_id.wrapping_add(1);
            }

            let sent = match sendmmsg_batch(fd, &batch_bufs) {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_micros(100));
                    0
                }
                Err(e) => {
                    tracing::debug!("sendmmsg error: {}", e);
                    0
                }
            };

            if sent > 0 {
                let send_time = Instant::now();
                let mut map = in_flight.lock();
                for id in batch_ids.iter().take(sent) {
                    map.insert(*id, send_time);
                    stats.inc_sent();
                }
            }

            // Force re-init of deadline when transitioning back to rate-limited.
            last_qps = 0;
        }
    }
}

// ─── Receiver OS thread ──────────────────────────────────────────────────────
//
// Runs recvmmsg(MSG_DONTWAIT) in a tight loop — independent of the sender.
// When idle (nothing received) it sleeps briefly and checks for timeouts.
// All RTT recording and timeout expiry happen here.

fn receiver_thread(
    fd: i32,
    in_flight: Arc<Mutex<HashMap<u16, Instant>>>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_dur: Duration,
) {
    // Pre-allocate flat buffer and recvmmsg infrastructure once.
    // iovecs point into flat_buf; flat_buf must never be reallocated.
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

    let mut last_timeout_check = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let n = unsafe {
            libc::recvmmsg(
                fd,
                msgs.as_mut_ptr(),
                RECV_BATCH as libc::c_uint,
                libc::MSG_DONTWAIT as _,
                std::ptr::null_mut(),
            )
        };

        if n > 0 {
            let now = Instant::now();

            // Collect (rcode, rtt_us) pairs under a single in_flight lock,
            // then release before taking the histogram lock inside record_response.
            let responses: Vec<(u8, u64)> = {
                let mut map = in_flight.lock();
                (0..n as usize)
                    .filter_map(|i| {
                        let len = msgs[i].msg_len as usize;
                        let data = &flat_buf[i * MAX_MSG_SIZE..i * MAX_MSG_SIZE + len];
                        parse_response(data).and_then(|resp| {
                            map.remove(&resp.id).map(|sent_at| {
                                let rtt_us =
                                    now.duration_since(sent_at).as_micros() as u64;
                                (resp.rcode, rtt_us)
                            })
                        })
                    })
                    .collect()
            }; // in_flight lock released here

            for (rcode, rtt_us) in responses {
                stats.record_response(rcode, rtt_us);
            }
        } else {
            // Nothing received — expire timeouts every 10 ms, then yield briefly.
            let now = Instant::now();
            if now.duration_since(last_timeout_check) >= Duration::from_millis(10) {
                let mut map = in_flight.lock();
                map.retain(|_, sent_at| {
                    if now.duration_since(*sent_at) > timeout_dur {
                        stats.inc_timeout();
                        false
                    } else {
                        true
                    }
                });
                last_timeout_check = now;
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }
}

// ─── Public async entry point ────────────────────────────────────────────────
//
// Creates one std::net::UdpSocket (blocking), clones it for the receiver fd,
// then spawns two OS threads. Waits for both via spawn_blocking so the tokio
// scheduler is never blocked.

#[allow(clippy::too_many_arguments)]
pub async fn run_udp_worker(
    server_addr: SocketAddr,
    query_source: Arc<dyn QuerySource>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_ms: u64,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
    worker_id: usize,
    max_outstanding: usize,
) {
    let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("bind UDP socket: {}", e);
            return;
        }
    };
    if let Err(e) = socket.connect(server_addr) {
        tracing::error!("connect UDP socket to {}: {}", server_addr, e);
        return;
    }
    // dup() — sender and receiver share the same OS socket.
    let recv_socket = match socket.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("clone UDP socket: {}", e);
            return;
        }
    };
    let sender_fd = socket.as_raw_fd();
    let recv_fd = recv_socket.as_raw_fd();

    let in_flight: Arc<Mutex<HashMap<u16, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let timeout_dur = Duration::from_millis(timeout_ms);

    // ── Sender thread ──────────────────────────────────────────────────────
    let in_s = in_flight.clone();
    let stats_s = stats.clone();
    let sd_s = shutdown.clone();
    let qps_s = qps_per_worker.clone();
    let qs = query_source.clone();
    let sender = std::thread::spawn(move || {
        super::pin_to_cpu(worker_id);
        let _sock = socket; // keep fd alive for the lifetime of this thread
        sender_thread(sender_fd, in_s, qs, stats_s, sd_s, qps_s, verbose, max_outstanding);
    });

    // ── Receiver thread ────────────────────────────────────────────────────
    let in_r = in_flight;
    let stats_r = stats;
    let sd_r = shutdown;
    let receiver = std::thread::spawn(move || {
        let _sock = recv_socket; // keep fd alive
        receiver_thread(recv_fd, in_r, stats_r, sd_r, timeout_dur);
    });

    // Wait for both without blocking the tokio runtime.
    tokio::task::spawn_blocking(move || {
        sender.join().ok();
        receiver.join().ok();
    })
    .await
    .ok();
}
