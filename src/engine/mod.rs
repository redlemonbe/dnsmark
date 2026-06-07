pub mod compare;
pub mod multi_nic;
pub use multi_nic::run_multi_nic;
pub mod ramp;
pub mod receiver;
pub mod sender;

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::config::{Config, Protocol};
use crate::query::{file::FileQuerySource, random::RandomQuerySource, QuerySource, WireQueryPool};
use crate::stats::{oom_guard, StatsCollector, StatsSnapshot};

#[cfg(feature = "xdp")]
/// PHY-confirmed transmitted packet count for one interface. Prefers the driver
/// NIC-level counter (the truth on ixgbe AF_XDP zero-copy, where the netdev
/// tx_packets can report descriptors that never reached the wire); falls back to
/// the portable netdev counter when no NIC-level counter is exposed.
fn nic_wire_tx_packets(iface: &str) -> Option<u64> {
    if let Ok(out) = std::process::Command::new("ethtool").arg("-S").arg(iface).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            for key in ["tx_pkts_nic", "port.tx_unicast", "tx_unicast"] {
                for line in s.lines() {
                    let l = line.trim();
                    if let Some(rest) = l.strip_prefix(key) {
                        let v = rest.trim_start_matches([':', ' ']).trim();
                        if let Some(tok) = v.split_whitespace().next() {
                            if let Ok(n) = tok.parse::<u64>() { return Some(n); }
                        }
                    }
                }
            }
        }
    }
    // No netdev tx_packets fallback on purpose: under AF_XDP zero-copy that counter
    // reports descriptors *submitted*, not transmitted on the wire — exactly the
    // fiction this guard exists to catch. Only PHY-level *_nic counters count.
    None
}

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

    // Lock-free in-flight table + global ID counter — used only when XDP is active.
    // Declared here so they outlive the spawn loop.
    #[cfg(feature = "xdp")]
    let xdp_in_flight = Arc::new(crate::transport::xdp::InFlight::new());
    #[cfg(feature = "xdp")]
    let xdp_global_id = Arc::new(AtomicU16::new(rand::random::<u16>()));

    // xdp_active = Some(XdpHandle) when XDP is running, None otherwise.
    // The handle must stay alive until after all workers finish.
    #[cfg(feature = "xdp")]
    let _xdp_handle: Option<crate::transport::xdp::XdpHandle>;

    #[cfg(feature = "xdp")]
    let use_xdp = if config.force_xdp && config.protocol == Protocol::Udp {
        use crate::transport::xdp;

        let iface = xdp::iface_for_benchmark(config.server);

        // Enable the unified RX+TX-per-queue datapath (the default). DNSMARK_XDP_TX=0
        // keeps the legacy split sender/receiver path (sendmmsg fallback).
        if std::env::var("DNSMARK_XDP_TX").map(|v| v != "0").unwrap_or(true) {
            xdp::set_unified_cfg(xdp::UnifiedCfg {
                wire_pool:       wire_pool.clone(),
                qps_per_worker:  shared_qps.clone(),
                max_outstanding: config.max_outstanding,
                total_qps:       config.qps,
            });
        }

        match xdp::start_xdp_receive_path(
            &iface,
            config.server,
            config.port,
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
        if !config.quiet {
            println!("XDP disabled — using UDP receive path (use --xdp to enable)");
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
            let wp  = wire_pool.clone();
            let nw  = config.concurrent;

            handles.push(tokio::spawn(crate::transport::xdp::run_xdp_sender_worker(
                server_addr, qs, st, sd, cfg.timeout_ms, qps, cfg.verbose, i,
                cfg.max_outstanding, gif, xif, xid, wp, nw,
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
            let mut best_ok_offered: u64 = 0;  // highest offered load that held the p50 SLO
            loop {
                // Measure the achieved rate via SENT (submitted descriptors) — reliable
                // on every datapath. completed (round-trip) is unusable under XDP
                // zero-copy when the generator can't drain all RX queues (X520).
                let burst_start = st.sent.load(Ordering::Relaxed);
                qps_arc.store(0, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if sd.load(Ordering::Relaxed) { break; }
                let burst_completions = st.sent.load(Ordering::Relaxed)
                    .saturating_sub(burst_start);

                let per_worker = (ctrl.current_qps / concurrent.max(1) as u64).max(1);
                let _ = st.ramp_step_latency();        // drop burst-phase RTTs, open clean window
                let sent_w0 = st.sent.load(Ordering::Relaxed);
                let comp_w0 = st.completed.load(Ordering::Relaxed);
                qps_arc.store(per_worker, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                if sd.load(Ordering::Relaxed) { break; }

                // Per-step latency + answered ratio for the 4 s paced window: the
                // methodology curve (percentiles vs offered load, step by step).
                let (p50, p95, p99, samples) = st.ramp_step_latency();
                let sent_w = st.sent.load(Ordering::Relaxed).saturating_sub(sent_w0);
                let _ = comp_w0;

                if p50 <= 1_000 { best_ok_offered = best_ok_offered.max(sent_w / 4); }
                let target_qps = ctrl.current_qps;
                let (new_qps, saturated, max_sustainable) = ctrl.advance(sent_w / 4, p50);

                // Per-step methodology line: offered load vs RTT percentiles. `samples`
                // is the RTTs actually measured this step; when it collapses relative to
                // the offered rate, the round-trip path (not the server) is saturated.
                println!(
                    "Ramp step: offered {:>9} q/s | rtt-samples {:>8} | \
                     p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms",
                    sent_w / 4, samples,
                    p50 as f64 / 1000.0, p95 as f64 / 1000.0, p99 as f64 / 1000.0,
                );
                let _ = target_qps;

                if saturated {
                    let _ = max_sustainable; let _ = target_qps;
                    println!(
                        "\nMax offered load under p50<1ms SLO: {} q/s (highest step that held the SLO)",
                        best_ok_offered,
                    );
                    sd.store(true, Ordering::Relaxed);
                    notify.notify_one();
                    break;
                }

                let new_per_worker = (new_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(new_per_worker, Ordering::Relaxed);
                let _ = new_qps; let _ = burst_completions; let _ = target_qps;
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

    // Auto warm-up: let XSK bind, rings fill and the NIC ramp, then reset the
    // measurement window so the reported rate is steady-state (env DNSMARK_WARMUP
    // overrides; default 3 s; skipped for ramp and very short runs).
    if !config.ramp {
        let warmup = std::env::var("DNSMARK_WARMUP").ok()
            .and_then(|v| v.parse::<u64>().ok()).unwrap_or(3);
        if warmup > 0 && config.duration_secs > warmup {
            tokio::time::sleep(std::time::Duration::from_secs(warmup)).await;
            stats.reset_window();
        }
    }
    let start = Instant::now();
    // wire-truth guard: snapshot the egress NIC PHY tx counter to confirm that the
    // reported throughput actually reached the wire (catches a wedged ZC TX path,
    // where descriptors are submitted/completed but tx_pkts_nic never advances).
    #[cfg(feature = "xdp")]
    let wt_baseline: Option<(Vec<String>, u64)> = if use_xdp {
        let mut ifs: Vec<String> = Vec::new();
        let mut srvs = config.servers.clone();
        if srvs.is_empty() { srvs.push(config.server); }
        for srv in srvs {
            let i = crate::transport::xdp::iface_for_benchmark(srv);
            if !ifs.contains(&i) { ifs.push(i); }
        }
        let tx0: u64 = ifs.iter().filter_map(|i| nic_wire_tx_packets(i)).sum();
        Some((ifs, tx0))
    } else { None };

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
    #[allow(unused_mut)]
    let mut snap = stats.snapshot(elapsed);
    #[cfg(feature = "xdp")]
    if let Some((ifs, tx0)) = wt_baseline {
        let tx1: u64 = ifs.iter().filter_map(|i| nic_wire_tx_packets(i)).sum();
        let secs = elapsed.max(1e-9);
        snap.wire_qps = Some(tx1.saturating_sub(tx0) as f64 / secs);
    }
    Ok(snap)
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
