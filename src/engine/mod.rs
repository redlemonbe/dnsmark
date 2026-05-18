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

    // max_in_flight per worker: 4× the worker count gives enough pipeline depth
    // to sustain the QPS target under normal latency without blocking the sender.
    let max_in_flight = (config.concurrent * 4).max(1);

    // Spawn workers
    let mut handles = Vec::with_capacity(config.concurrent);
    for i in 0..config.concurrent {
        let qs = query_source.clone();
        let st = stats.clone();
        let sd = shutdown.clone();
        let qps_arc = shared_qps.clone();
        let cfg = config.clone();
        let sn = config.server.to_string();

        let handle = match config.protocol {
            Protocol::Udp => tokio::spawn(sender::run_udp_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose, i, max_in_flight,
            )),
            Protocol::Tcp => tokio::spawn(sender::run_tcp_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose, i, max_in_flight,
            )),
            Protocol::Dot => tokio::spawn(sender::run_dot_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps_arc, cfg.verbose, sn, i, max_in_flight,
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
            loop {
                // 1s burst: unlimited mode (sendmmsg) to probe real achievable completions
                let burst_start = st.completed.load(Ordering::Relaxed);
                qps_arc.store(0, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if sd.load(Ordering::Relaxed) { break; }
                let burst_completions = st.completed.load(Ordering::Relaxed)
                    .saturating_sub(burst_start);

                // Restore rate limit, let server stabilise for 4s
                let per_worker = (ctrl.current_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(per_worker, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                if sd.load(Ordering::Relaxed) { break; }

                let target_qps = ctrl.current_qps;
                let (new_qps, saturated, max_sustainable) = ctrl.advance(burst_completions);

                if saturated {
                    let reported = if max_sustainable == 0 { target_qps } else { max_sustainable };
                    let reason = if burst_completions < (target_qps as f64 * 0.80) as u64 {
                        format!("burst {}/s < {}/s target", burst_completions, target_qps)
                    } else {
                        "hard cap (20 doublings)".to_string()
                    };
                    println!("\nMax sustainable QPS: {} ({})", reported, reason);
                    sd.store(true, Ordering::Relaxed);
                    notify.notify_one();
                    break;
                }

                let new_per_worker = (new_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(new_per_worker, Ordering::Relaxed);
                println!("Ramp: target QPS -> {} (burst: {}/s)", new_qps, burst_completions);
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

