pub mod compare;
pub mod ramp;
pub mod receiver;
pub mod sender;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;

use crate::config::{Config, Protocol};
use crate::query::{file::FileQuerySource, random::RandomQuerySource, QuerySource, WireQueryPool};
use crate::stats::{oom_guard, StatsCollector, StatsSnapshot};

pub async fn run(config: Arc<Config>) -> anyhow::Result<StatsSnapshot> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let result = run_with_shutdown(config, shutdown.clone()).await;
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

    // Build query source + pre-built wire pool (eliminates per-query allocation)
    let query_source: Arc<dyn QuerySource> = if config.random {
        Arc::new(RandomQuerySource::new(&config.random_domain, config.random_qtype))
    } else if let Some(path) = &config.query_file {
        Arc::new(FileQuerySource::load(path).context("load query file")?)
    } else {
        Arc::new(RandomQuerySource::new(&config.random_domain, config.random_qtype))
    };
    let pairs = query_source.all_wire_pairs();
    tracing::debug!(templates = pairs.len(), "pre-building wire query pool");
    let wire_pool = Arc::new(WireQueryPool::from_pairs(&pairs));

    // Verify server reachable (UDP probe)
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

    // QPS per worker
    let initial_qps_per_worker = if config.ramp {
        (ramp::RampController::new().current_qps / config.concurrent.max(1) as u64).max(1)
    } else if config.qps > 0 {
        (config.qps / config.concurrent.max(1) as u64).max(1)
    } else {
        0
    };
    let shared_qps = Arc::new(AtomicU64::new(initial_qps_per_worker));
    let global_in_flight = Arc::new(AtomicUsize::new(0));

    // ── Try to activate XDP receive path ──────────────────────────────────
    //
    // xdp_active: holds the XdpHandle (keeps XDP program attached) plus the
    // shared in_flight map and global ID counter used by sender workers.
    // Keeping it alive until the function returns ensures the XDP program
    // stays attached for the entire benchmark run.

    // Shared in_flight and global ID counter — used only when XDP is active.
    // Declared here so they outlive the spawn loop.
    let xdp_in_flight: Arc<Mutex<HashMap<u16, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let xdp_global_id = Arc::new(AtomicU16::new(rand::random::<u16>()));

    // xdp_active = Some(XdpHandle) when XDP is running, None otherwise.
    // The handle must stay alive until after all workers finish.
    #[cfg(feature = "xdp")]
    let _xdp_handle: Option<crate::transport::xdp::XdpHandle>;

    #[cfg(feature = "xdp")]
    let use_xdp = if config.protocol == Protocol::Udp && !config.no_xdp {
        use crate::transport::xdp;

        let iface = xdp::iface_for_benchmark(config.server);

        match xdp::start_xdp_receive_path(
            &iface,
            xdp_in_flight.clone(),
            global_in_flight.clone(),
            stats.clone(),
            shutdown.clone(),
            Duration::from_millis(config.timeout_ms),
        ) {
            Ok(guard) => {
                if !config.quiet {
                    println!("XDP receive path active (iface={iface})");
                }
                _xdp_handle = Some(guard);
                true
            }
            Err(e) => {
                _xdp_handle = None;
                let hint = xdp_cap_hint(&e);
                if config.force_xdp {
                    if hint.is_empty() {
                        return Err(anyhow::anyhow!("XDP unavailable: {e}"));
                    }
                    return Err(anyhow::anyhow!("XDP unavailable: {e}\n{hint}"));
                }
                if !config.quiet {
                    if hint.is_empty() {
                        eprintln!("XDP unavailable: {e} — using UDP receive path");
                    } else {
                        eprintln!("XDP unavailable: {e}");
                        eprintln!("{hint}");
                        eprintln!("Falling back to UDP receive path.");
                    }
                }
                false
            }
        }
    } else {
        _xdp_handle = None;
        if !config.quiet && config.no_xdp {
            println!("XDP disabled (--no-xdp) — using UDP receive path");
        }
        false
    };

    #[cfg(not(feature = "xdp"))]
    let use_xdp = false;

    // ── Spawn workers ──────────────────────────────────────────────────────

    let mut handles = Vec::with_capacity(config.concurrent);

    // XDP path: sender-only workers (receiver is the shared XDP thread).
    #[cfg(feature = "xdp")]
    if use_xdp {
        for i in 0..config.concurrent {
            let qs  = query_source.clone();
            let st  = stats.clone();
            let sd  = shutdown.clone();
            let qps = shared_qps.clone();
            let cfg = config.clone();
            let gif = global_in_flight.clone();
            let xif = xdp_in_flight.clone();
            let xid = xdp_global_id.clone();

            handles.push(tokio::spawn(crate::transport::xdp::run_xdp_sender_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps, cfg.verbose, i,
                cfg.max_outstanding, gif, xif, xid,
            )));
        }
    }

    // Regular path: per-worker sender+receiver via recvmmsg (UDP / TCP / DoT).
    if !use_xdp {
        for i in 0..config.concurrent {
            let qs  = query_source.clone();
            let st  = stats.clone();
            let sd  = shutdown.clone();
            let qps = shared_qps.clone();
            let cfg = config.clone();
            let sn  = config.server.to_string();
            let gif = global_in_flight.clone();

            let wp  = wire_pool.clone();
            let handle = match config.protocol {
                Protocol::Udp => tokio::spawn(sender::run_udp_worker(
                    server_addr, wp, st, sd, cfg.timeout_ms, qps, cfg.verbose, i,
                    cfg.max_outstanding, gif,
                )),
                Protocol::Tcp => tokio::spawn(sender::run_tcp_worker(
                    server_addr, qs, st, sd, cfg.timeout_ms, qps, cfg.verbose, i,
                    cfg.max_outstanding,
                )),
                Protocol::Dot => tokio::spawn(sender::run_dot_worker(
                    server_addr, qs, st, sd, cfg.timeout_ms, qps, cfg.verbose, sn, i,
                    cfg.max_outstanding,
                )),
            };
            handles.push(handle);
        }
    }

    // ── Ramp controller ────────────────────────────────────────────────────

    let ramp_done = Arc::new(tokio::sync::Notify::new());

    let ramp_handle = if config.ramp {
        let st  = stats.clone();
        let sd  = shutdown.clone();
        let qps_arc  = shared_qps.clone();
        let concurrent = config.concurrent;
        let notify = ramp_done.clone();
        Some(tokio::spawn(async move {
            let mut ctrl = ramp::RampController::new();
            loop {
                let burst_start = st.completed.load(Ordering::Relaxed);
                qps_arc.store(0, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if sd.load(Ordering::Relaxed) { break; }
                let burst_completions = st.completed.load(Ordering::Relaxed)
                    .saturating_sub(burst_start);

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

    // ── TUI ────────────────────────────────────────────────────────────────

    let tui_handle = if !config.no_tui
        && !config.quiet
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        let st  = stats.clone();
        let sd  = shutdown.clone();
        let cfg = config.clone();
        Some(tokio::spawn(crate::output::tui::run_tui(st, sd, cfg)))
    } else {
        None
    };

    let start = Instant::now();

    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(config.duration_secs)), if !config.ramp => {
            shutdown.store(true, Ordering::Relaxed);
        }
        _ = ramp_done.notified(), if config.ramp => {}
        _ = tokio::signal::ctrl_c() => {
            shutdown.store(true, Ordering::Relaxed);
        }
    }

    shutdown.store(true, Ordering::Relaxed);

    for h in handles        { let _ = h.await; }
    if let Some(h) = ramp_handle { let _ = h.await; }
    if let Some(h) = tui_handle  { let _ = h.await; }

    // _xdp_handle drops here, detaching XDP program from the NIC.

    let elapsed = start.elapsed().as_secs_f64();
    Ok(stats.snapshot(elapsed))
}

#[cfg(feature = "xdp")]
fn xdp_cap_hint(e: &str) -> String {
    if e.contains("Operation not permitted")
        || e.contains("EPERM")
        || e.contains("BPF_PROG_LOAD")
    {
        "To enable XDP: sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip $(which dnsmark)".into()
    } else {
        String::new()
    }
}
