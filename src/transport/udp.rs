use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

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

    let mut in_flight: HashMap<u16, Instant> = HashMap::new();
    let mut next_id: u16 = rand::random();
    let mut recv_buf = [0u8; 4096];
    let timeout_dur = std::time::Duration::from_millis(timeout_ms);

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

        // Rate limit
        let qps = qps_per_worker.load(Ordering::Relaxed);
        if qps > 0 {
            let sleep = std::time::Duration::from_secs_f64(1.0 / qps as f64);
            tokio::time::sleep(sleep).await;
        } else {
            tokio::task::yield_now().await;
        }

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Send next query
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
    }
}
