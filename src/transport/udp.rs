use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use tokio::io::Interest;
use tokio::net::UdpSocket;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

/// Number of datagrams sent per sendmmsg(2) syscall in unlimited mode.
const BATCH_SIZE: usize = 64;

/// Send up to `bufs.len()` datagrams in a single sendmmsg(2) syscall.
/// The socket must be already connected (msg_name = NULL).
/// Returns Ok(n) with n <= bufs.len(), or Err(WouldBlock) when the
/// kernel send buffer is full.
fn sendmmsg_batch(fd: i32, bufs: &[Vec<u8>]) -> io::Result<usize> {
    if bufs.is_empty() {
        return Ok(0);
    }

    // iovecs must be stable in memory for the duration of sendmmsg
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
        libc::sendmmsg(
            fd,
            msgs.as_mut_ptr(),
            msgs.len() as libc::c_uint,
            libc::MSG_DONTWAIT as _,
        )
    };

    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as usize)
    }
}

pub async fn run_udp_worker(
    server_addr: SocketAddr,
    query_source: Arc<dyn QuerySource>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_ms: u64,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
) {
    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("bind UDP socket: {}", e);
            return;
        }
    };
    if let Err(e) = socket.connect(server_addr).await {
        tracing::error!("connect UDP socket to {}: {}", server_addr, e);
        return;
    }
    let fd = socket.as_raw_fd();

    let mut in_flight: HashMap<u16, Instant> = HashMap::new();
    let mut next_id: u16 = rand::random();
    let mut recv_buf = [0u8; 4096];
    let timeout_dur = Duration::from_millis(timeout_ms);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Drain all pending responses (non-blocking)
        loop {
            match socket.try_recv(&mut recv_buf) {
                Ok(n) => {
                    if let Some(resp) = parse_response(&recv_buf[..n]) {
                        if let Some(sent_at) = in_flight.remove(&resp.id) {
                            let rtt_us = sent_at.elapsed().as_micros() as u64;
                            stats.record_response(resp.rcode, rtt_us);
                            if verbose {
                                tracing::debug!(
                                    id = resp.id,
                                    rcode = resp.rcode,
                                    rtt_us,
                                    "response"
                                );
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // Expire timeouts
        let now = Instant::now();
        in_flight.retain(|_, sent_at| {
            if now.duration_since(*sent_at) > timeout_dur {
                stats.inc_timeout();
                false
            } else {
                true
            }
        });

        let qps = qps_per_worker.load(Ordering::Relaxed);
        if qps > 0 {
            // Rate-limited: keep receiving during the inter-send pause so RTT is not
            // inflated by tokio scheduling delay at low QPS.
            let interval = Duration::from_secs_f64(1.0 / qps as f64);
            let sleep = tokio::time::sleep(interval);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut sleep => break,
                    result = socket.recv(&mut recv_buf) => {
                        if let Ok(n) = result {
                            if let Some(resp) = parse_response(&recv_buf[..n]) {
                                if let Some(sent_at) = in_flight.remove(&resp.id) {
                                    let rtt_us = sent_at.elapsed().as_micros() as u64;
                                    stats.record_response(resp.rcode, rtt_us);
                                    if verbose {
                                        tracing::debug!(
                                            id = resp.id,
                                            rcode = resp.rcode,
                                            rtt_us,
                                            "response"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Single send per rate-limit interval
            let entry = query_source.next();
            let qbytes = build_query(next_id, &entry.name, entry.qtype);
            match socket.send(&qbytes).await {
                Ok(_) => {
                    in_flight.insert(next_id, Instant::now());
                    stats.inc_sent();
                    if verbose {
                        tracing::debug!(id = next_id, name = %entry.name, "sent query");
                    }
                    next_id = next_id.wrapping_add(1);
                }
                Err(e) => {
                    tracing::debug!("UDP send error: {}", e);
                    stats.inc_error();
                }
            }
        } else {
            // Unlimited mode: BATCH_SIZE datagrams per sendmmsg(2) syscall.
            // try_io integrates with tokio's epoll readiness: if the closure
            // returns WouldBlock, tokio clears the cached ready state and the
            // next writable().await waits for a real EPOLLOUT event.
            let mut batch_bufs: Vec<Vec<u8>> = Vec::with_capacity(BATCH_SIZE);
            let mut batch_ids: Vec<u16> = Vec::with_capacity(BATCH_SIZE);
            for _ in 0..BATCH_SIZE {
                let entry = query_source.next();
                batch_bufs.push(build_query(next_id, &entry.name, entry.qtype));
                batch_ids.push(next_id);
                next_id = next_id.wrapping_add(1);
            }

            let sent = match socket.try_io(Interest::WRITABLE, || sendmmsg_batch(fd, &batch_bufs)) {
                Ok(n) => n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Kernel send buffer full — wait for space via epoll
                    let _ = socket.writable().await;
                    0
                }
                Err(e) => {
                    tracing::debug!("sendmmsg error: {}", e);
                    0
                }
            };

            let send_time = Instant::now();
            for i in 0..sent {
                in_flight.insert(batch_ids[i], send_time);
                stats.inc_sent();
            }

            // Yield once per batch so tokio can schedule other tasks
            // (shutdown signal, ramp controller, duration timer).
            tokio::task::yield_now().await;
        }
    }
}
