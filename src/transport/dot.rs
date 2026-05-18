use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

fn make_tls_connector() -> anyhow::Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

pub async fn run_dot_worker(
    server_addr: SocketAddr,
    query_source: Arc<dyn QuerySource>,
    stats: Arc<StatsCollector>,
    shutdown: Arc<AtomicBool>,
    timeout_ms: u64,
    qps_per_worker: Arc<AtomicU64>,
    verbose: bool,
    server_name: String,
) {
    let connector = match make_tls_connector() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("create TLS connector: {}", e);
            return;
        }
    };
    let timeout_dur = std::time::Duration::from_millis(timeout_ms);
    let mut id: u16 = rand::random();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let sni = match rustls::pki_types::ServerName::try_from(server_name.as_str())
            .map(|n| n.to_owned())
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("invalid DoT server name '{}': {}", server_name, e);
                break;
            }
        };

        let result = async {
            let tcp = TcpStream::connect(server_addr)
                .await
                .context("TCP connect for DoT")?;
            let mut tls = connector.connect(sni, tcp).await.context("TLS handshake")?;

            let entry = query_source.next();
            let qbytes = build_query(id, &entry.name, entry.qtype);
            let len = qbytes.len() as u16;
            tls.write_all(&len.to_be_bytes()).await.context("DoT write length")?;
            tls.write_all(&qbytes).await.context("DoT write query")?;
            tls.flush().await.context("DoT flush")?;

            let sent_at = Instant::now();
            stats.inc_sent();

            let mut len_buf = [0u8; 2];
            tls.read_exact(&mut len_buf).await.context("DoT read length")?;
            let resp_len = u16::from_be_bytes(len_buf) as usize;
            let mut resp_buf = vec![0u8; resp_len];
            tls.read_exact(&mut resp_buf).await.context("DoT read response")?;

            if let Some(resp) = parse_response(&resp_buf) {
                let rtt_us = sent_at.elapsed().as_micros() as u64;
                stats.record_response(resp.rcode, rtt_us);
                if verbose {
                    tracing::debug!(id = resp.id, rcode = resp.rcode, rtt_us, "DoT response");
                }
            }
            anyhow::Ok(())
        };

        match tokio::time::timeout(timeout_dur, result).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::debug!("DoT error: {}", e);
                stats.inc_error();
            }
            Err(_) => {
                stats.inc_timeout();
            }
        }

        id = id.wrapping_add(1);

        let qps = qps_per_worker.load(Ordering::Relaxed);
        if qps > 0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(1.0 / qps as f64)).await;
        }
    }
}
