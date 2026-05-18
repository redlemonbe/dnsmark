use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

pub async fn run_tcp_worker(
    server_addr: SocketAddr,
    query_source: Arc<dyn QuerySource>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_ms: u64,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
) {
    let timeout_dur = std::time::Duration::from_millis(timeout_ms);
    let mut id: u16 = rand::random();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Reconnect each iteration (simple, avoids state machine)
        let stream = match tokio::time::timeout(timeout_dur, TcpStream::connect(server_addr)).await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::debug!("TCP connect {}: {}", server_addr, e);
                stats.inc_error();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            Err(_) => {
                stats.inc_timeout();
                continue;
            }
        };

        let entry = query_source.next();
        let qbytes = build_query(id, &entry.name, entry.qtype);

        let result = async {
            let (mut reader, mut writer) = stream.into_split();
            // RFC 1035 §4.2.2: 2-byte length prefix
            let len = qbytes.len() as u16;
            writer.write_all(&len.to_be_bytes()).await.context("write length")?;
            writer.write_all(&qbytes).await.context("write query")?;
            writer.flush().await.context("flush")?;

            let sent_at = Instant::now();
            stats.inc_sent();

            let mut len_buf = [0u8; 2];
            reader.read_exact(&mut len_buf).await.context("read length")?;
            let resp_len = u16::from_be_bytes(len_buf) as usize;
            let mut resp_buf = vec![0u8; resp_len];
            reader.read_exact(&mut resp_buf).await.context("read response")?;

            if let Some(resp) = parse_response(&resp_buf) {
                let rtt_us = sent_at.elapsed().as_micros() as u64;
                stats.record_response(resp.rcode, rtt_us);
                if verbose {
                    tracing::debug!(id = resp.id, rcode = resp.rcode, rtt_us, "TCP response");
                }
            }
            anyhow::Ok(())
        };

        match tokio::time::timeout(timeout_dur, result).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::debug!("TCP error: {}", e);
                stats.inc_error();
            }
            Err(_) => {
                stats.inc_timeout();
            }
        }

        id = id.wrapping_add(1);

        // Rate limit
        let qps = qps_per_worker.load(Ordering::Relaxed);
        if qps > 0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(1.0 / qps as f64)).await;
        }
    }
}
