pub mod compare;
pub mod ramp;
pub mod receiver;
pub mod sender;

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::Context;

use crate::config::{Config, Protocol};
use crate::query::{file::FileQuerySource, random::RandomQuerySource, QuerySource};
use crate::stats::{oom_guard, StatsCollector, StatsSnapshot};

pub async fn run(config: Arc<Config>) -> anyhow::Result<StatsSnapshot> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let result = run_with_shutdown(config, shutdown.clone()).await;

    // Ensure shutdown is set so OOM guard and other tasks exit
    shutdown.store(true, Ordering::Relaxed);
    result
}

pub async fn run_with_shutdown(
    config: Arc<Config>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<StatsSnapshot> {
    let stats = Arc::new(StatsCollector::new());

    // OOM guard
    {
        let sd = shutdown.clone();
        tokio::spawn(oom_guard::run(sd));
    }

    // Build query source
    let query_source: Arc<dyn QuerySource> = if config.random {
        Arc::new(RandomQuerySource::new(&config.random_domain, config.random_qtype))
    } else if let Some(path) = &config.query_file {
        Arc::new(FileQuerySource::load(path).context("load query file")?)
    } else {
        Arc::new(RandomQuerySource::new(&config.random_domain, config.random_qtype))
    };

    // Verify server is reachable (quick UDP probe for UDP mode)
    let server_addr: SocketAddr = (config.server, config.port).into();
    if config.protocol == Protocol::Udp {
        let probe = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .context("bind probe socket")?;
        probe.connect(server_addr).await.context(format!(
            "server {}:{} is unreachable",
            config.server, config.port
        ))?;
    }

    // QPS per worker — ramp starts at 1000 total, normal mode uses -Q (0 = unlimited)
    let initial_qps_per_worker = if config.ramp {
        (ramp::RampController::new().current_qps / config.concurrent.max(1) as u64).max(1)
    } else if config.qps > 0 {
        (config.qps / config.concurrent.max(1) as u64).max(1)
    } else {
        0
    };
    let shared_qps = Arc::new(AtomicU64::new(initial_qps_per_worker));

    // Spawn workers
    let mut handles = Vec::with_capacity(config.concurrent);
    for _ in 0..config.concurrent {
        let qs = query_source.clone();
        let st = stats.clone();
        let sd = shutdown.clone();
        let qps_arc = shared_qps.clone();
        let cfg = config.clone();
        let sn = config.server.to_string();

        let handle = match config.protocol {
            Protocol::Udp => tokio::spawn(sender::run_udp_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose,
            )),
            Protocol::Tcp => tokio::spawn(sender::run_tcp_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose,
            )),
            Protocol::Dot => tokio::spawn(sender::run_dot_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose, sn,
            )),
        };
        handles.push(handle);
    }

    // Notify used to wake the main select when ramp saturates
    let ramp_done = Arc::new(tokio::sync::Notify::new());

    // Ramp mode controller
    let ramp_handle = if config.ramp {
        let st = stats.clone();
        let sd = shutdown.clone();
        let qps_arc = shared_qps.clone();
        let concurrent = config.concurrent;
        let notify = ramp_done.clone();
        Some(tokio::spawn(async move {
            let mut ctrl = ramp::RampController::new();
            let mut prev_sent = 0u64;
            let mut prev_timeouts = 0u64;
            let mut prev_servfail = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if sd.load(Ordering::Relaxed) { break; }

                let cur_sent = st.sent.load(Ordering::Relaxed);
                let cur_timeouts = st.timeouts.load(Ordering::Relaxed);
                let cur_servfail = st.rcode_servfail.load(Ordering::Relaxed);
                let p99_us = st.p99_us();

                let delta_sent = cur_sent.saturating_sub(prev_sent);
                let delta_timeouts = cur_timeouts.saturating_sub(prev_timeouts);
                let delta_sf = cur_servfail.saturating_sub(prev_servfail);

                let target_qps = ctrl.current_qps;
                let (new_qps, saturated, max_sustainable) =
                    ctrl.advance(delta_sent, delta_timeouts, delta_sf, p99_us);

                if saturated {
                    let reported = if max_sustainable == 0 { target_qps } else { max_sustainable };
                    let timeout_rate = if delta_sent > 0 {
                        delta_timeouts as f64 / delta_sent as f64
                    } else {
                        0.0
                    };
                    let sf_rate = if delta_sent > 0 {
                        delta_sf as f64 / delta_sent as f64
                    } else {
                        0.0
                    };
                    let reason = if p99_us > 50_000 {
                        format!("p99 {}ms > 50ms", p99_us / 1000)
                    } else if timeout_rate > 0.01 {
                        format!("timeout rate {:.1}%", timeout_rate * 100.0)
                    } else if sf_rate > 0.05 {
                        format!("SERVFAIL rate {:.1}%", sf_rate * 100.0)
                    } else {
                        "hard cap (20 doublings)".to_string()
                    };
                    println!("\nMax sustainable QPS: {} ({})", reported, reason);
                    sd.store(true, Ordering::Relaxed);
                    notify.notify_one();
                    break;
                }

                // Update shared QPS (per worker)
                let per_worker = (new_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(per_worker, Ordering::Relaxed);
                println!("Ramp: target QPS -> {}", new_qps);

                prev_sent = cur_sent;
                prev_timeouts = cur_timeouts;
                prev_servfail = cur_servfail;

            }
        }))
    } else {
        None
    };

    // TUI
    let tui_handle = if !config.no_tui
        && !config.quiet
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        let st = stats.clone();
        let sd = shutdown.clone();
        let cfg = config.clone();
        Some(tokio::spawn(crate::output::tui::run_tui(st, sd, cfg)))
    } else {
        None
    };

    let start = Instant::now();

    // Wait for duration, ramp completion, or Ctrl-C
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(config.duration_secs)), if !config.ramp => {
            shutdown.store(true, Ordering::Relaxed);
        }
        _ = ramp_done.notified(), if config.ramp => {
            // Ramp controller already set shutdown and printed the result
        }
        _ = tokio::signal::ctrl_c() => {
            shutdown.store(true, Ordering::Relaxed);
        }
    }

    // Ensure shutdown is set
    shutdown.store(true, Ordering::Relaxed);

    // Wait for workers
    for h in handles {
        let _ = h.await;
    }
    if let Some(h) = ramp_handle {
        let _ = h.await;
    }
    if let Some(h) = tui_handle {
        let _ = h.await;
    }

    let elapsed = start.elapsed().as_secs_f64();
    Ok(stats.snapshot(elapsed))
}

