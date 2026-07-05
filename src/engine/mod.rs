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
use crate::query::{builtin::BuiltinQuerySource, file::FileQuerySource, random::RandomQuerySource, QuerySource, WireQueryPool};
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

/// Replies that physically ARRIVED on `iface` = rx_packets + NIC ring-overflow drops.
///
/// This is the authoritative server reply rate on a dedicated benchmark link, in EVERY
/// mode. The subtlety the user flagged: in kernel-UDP at multi-Mpps the NIC ring overflows
/// (the softirq cannot drain it), so `rx_packets` alone under-counts — the dropped frames
/// land in `rx_missed_errors` (i40e) / `rx_fifo_errors` / `rx_over_errors`. Adding those
/// back recovers the true number of replies the server put on the wire (it equals the
/// SERVER's tx counter), without needing to read the remote server. In XDP the ring is
/// drained zero-copy so the drop counters stay ~0 and this equals rx_packets. Reads /sys
/// (portable, no ethtool parsing). rx_dropped is excluded (it also counts non-overflow drops).
fn nic_rx_packets(iface: &str) -> Option<u64> {
    let read = |f: &str| -> u64 {
        std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{f}"))
            .ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0)
    };
    let pkts = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_packets"))
        .ok().and_then(|s| s.trim().parse::<u64>().ok())?;
    Some(pkts + read("rx_missed_errors") + read("rx_fifo_errors") + read("rx_over_errors"))
}

/// On-wire RX bytes on `iface` (`/sys` rx_bytes, L2 frame bytes, FCS-excluded on most
/// drivers). Divided by the rx_packets delta over the same window it gives the average
/// reply size, which — with the link speed — yields the % of line rate (wire-bound test).
fn nic_rx_bytes(iface: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
        .ok().and_then(|s| s.trim().parse::<u64>().ok())
}

/// MAC-level unicast RX count from the NIC HARDWARE (`ethtool -S <iface>` `rx_unicast`).
/// Unlike the netdev `/sys` rx_packets, this counts frames the driver consumed via
/// **XDP_REDIRECT** too (on a bridged/virtio path the netdev counter misses them), so it
/// is the physical "replies that arrived in the card" — the correct, datapath-independent
/// throughput source for the ramp (no `sent` open-loop runaway, no XSK software under-count).
#[cfg(feature = "xdp")]
fn nic_rx_unicast(iface: &str) -> Option<u64> {
    let out = std::process::Command::new("ethtool").arg("-S").arg(iface).output().ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let l = line.trim();
        // the port counter is "rx_unicast:"; skip "veb.rx_unicast" and per-queue variants.
        if let Some(rest) = l.strip_prefix("rx_unicast:") {
            if let Ok(n) = rest.trim().parse::<u64>() { return Some(n); }
        }
    }
    None
}

/// Resolve the egress/return NIC(s) for the configured server(s) — where queries leave
/// and replies come back. Used to read the authoritative rx counter in both modes.
#[cfg(feature = "xdp")]
fn return_ifaces(config: &Config) -> Vec<String> {
    let mut srvs = config.servers.clone();
    if srvs.is_empty() { srvs.push(config.server); }
    let mut ifs: Vec<String> = Vec::new();
    for srv in srvs {
        let i = crate::transport::xdp::iface_for_benchmark(srv);
        let i = crate::transport::xdp::parent_interface(&i).unwrap_or(i);
        if !ifs.contains(&i) { ifs.push(i); }
    }
    ifs
}

fn format_qps(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
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
        if !config.quiet {
            eprintln!("No corpus specified (-d) — using built-in 2000-domain corpus.");
        }
        Arc::new(BuiltinQuerySource::new())
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
        (ramp::RampController::new(false).current_qps / config.concurrent.max(1) as u64).max(1)
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
            xdp::set_unified_cfg(iface.clone(), xdp::UnifiedCfg {
                wire_pool:       wire_pool.clone(),
                qps_per_worker:  shared_qps.clone(),
                max_outstanding: config.max_outstanding,
                total_qps:       config.qps,
                ramp:            config.ramp,
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
                    cfg.max_outstanding, cfg.ramp, gif,
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
    // NIC-verified peak served (the DSD "Capacity") exported from the ramp task, so the
    // final snapshot / multi-NIC breakdown can report the knee — not the ramp-up average.
    let ramp_peak = Arc::new(AtomicU64::new(0));

    // Return NIC(s) — the ramp reads the HARDWARE rx counter there to measure SERVED
    // throughput (physical replies in the card), the datapath-independent truth.
    #[cfg(feature = "xdp")]
    let ramp_ifs = return_ifaces(&config);
    #[cfg(not(feature = "xdp"))]
    let ramp_ifs: Vec<String> = Vec::new();

    let ramp_handle = if config.ramp {
        let st  = stats.clone();
        let sd  = shutdown.clone();
        let qps_arc  = shared_qps.clone();
        let concurrent = config.concurrent;
        let notify = ramp_done.clone();
        let ifs = ramp_ifs.clone();
        let ramp_peak = ramp_peak.clone();
        Some(tokio::spawn(async move {
            let mut ctrl = ramp::RampController::new(use_xdp);
            let mut best_ok_served: u64 = 0;  // highest SERVED rate (NIC HW) that held the SLO
            let mut peak_served: u64 = 0;     // max SERVED rate (NIC HW) over all steps = ceiling
            // Sum the HARDWARE rx_unicast across the return NIC(s); None if unavailable.
            #[cfg(feature = "xdp")]
            let rx_hw = || -> Option<u64> {
                if ifs.is_empty() { return None; }
                let mut t = 0u64; let mut any = false;
                for i in &ifs { if let Some(v) = nic_rx_unicast(i) { t += v; any = true; } }
                if any { Some(t) } else { None }
            };
            #[cfg(not(feature = "xdp"))]
            let rx_hw = || -> Option<u64> { let _ = &ifs; None };

            // Global pre-ramp prime: warm ARP/switch-FDB AND the server's cache over the WHOLE
            // query set before the first measured step. Without it the low-rate EXP steps run
            // first while the cache is cold — every domain a miss → slow upstream resolution
            // and drops — so they show seconds-scale tails and heavy loss that vanish once the
            // cache fills at higher (later) steps. Priming makes every step measure a warm
            // server. Discarded (cleared) before step 1. Override/skip via DNSMARK_RAMP_PRIME
            // (seconds; 0 = off).
            let prime_s = std::env::var("DNSMARK_RAMP_PRIME").ok()
                .and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
            if prime_s > 0 {
                let pw = (ctrl.current_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(pw, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(prime_s)).await;
                if sd.load(Ordering::Relaxed) { return; }
                let _ = st.ramp_step_latency(); // discard prime-phase RTTs
            }
            loop {
                let per_worker = (ctrl.current_qps / concurrent.max(1) as u64).max(1);
                // Warm up AT the step's target rate — NOT a qps=0 flood. A flood builds a deep
                // in-flight backlog whose late replies land in the measurement window and poison
                // its p95/p99 (seconds-scale tails seen even at low offered rates) — and in the
                // gated closed loop it pins `outstanding` at the cap. Pacing the warm-up keeps
                // in-flight shallow, so after the histogram clear the window measures only
                // steady-state RTTs. The warm-up is also what reaches steady state before we read
                // the NIC HW served counter. (burst_completions was unused — dropped.)
                qps_arc.store(per_worker, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if sd.load(Ordering::Relaxed) { break; }

                let _ = st.ramp_step_latency();        // drop warm-up RTTs, open a clean window
                let sent_w0 = st.sent.load(Ordering::Relaxed);
                let rx0 = rx_hw();                     // NIC HW served at window start
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                if sd.load(Ordering::Relaxed) { break; }

                // Per-step latency for the 4 s paced window.
                let (p50, p95, p99, samples) = st.ramp_step_latency();
                let sent_w = st.sent.load(Ordering::Relaxed).saturating_sub(sent_w0);
                // SERVED = physical replies counted in the card (rx_unicast delta). Falls back
                // to `sent` only if the HW counter is unavailable (non-XDP build / no ethtool).
                let served_qps = match (rx0, rx_hw()) {
                    (Some(a), Some(b)) if b >= a => (b - a) / 4,
                    _ => sent_w / 4,
                };

                peak_served = peak_served.max(served_qps);
                let target_qps = ctrl.current_qps;
                let (new_qps, saturated, max_sustainable) = ctrl.advance(served_qps, p50);
                // Auto SLO (relative to the measured floor) — advance() just refreshed it.
                if p50 <= ctrl.threshold_us() { best_ok_served = best_ok_served.max(served_qps); }

                // Per-step methodology line: offered + SERVED (NIC HW) vs RTT percentiles.
                println!(
                    "Ramp step: offered {:>9} q/s | served {:>9} q/s | rtt-samples {:>8} | \
                     p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms",
                    sent_w / 4, served_qps, samples,
                    p50 as f64 / 1000.0, p95 as f64 / 1000.0, p99 as f64 / 1000.0,
                );
                let _ = target_qps;

                if saturated {
                    let _ = max_sustainable; let _ = target_qps;
                    // Export the NIC-verified peak (the "Capacity") for the final snapshot.
                    ramp_peak.store(peak_served, Ordering::Relaxed);
                    let floor_ms = ctrl.baseline_us as f64 / 1000.0;
                    let slo_ms   = ctrl.threshold_us() as f64 / 1000.0;
                    println!();
                    println!("  Idle latency:  {:.3} ms   (floor — minimum p50 observed)", floor_ms);
                    // In --xdp the ramp is an open-loop firehose with a lossless zero-copy RX,
                    // so peak_served IS the server's raw ceiling ("max on the wire"). In
                    // kernel-UDP the ramp is a GATED closed loop (dnsperf-comparable, honest
                    // latency): the generator's kernel recv drops replies, which clogs the
                    // outstanding slots and caps the OFFERED rate well below the server's true
                    // capacity — so this figure is the closed-loop SLO knee, NOT the raw max.
                    if use_xdp {
                        println!("  Capacity:      {:>12}  (NIC-verified — max replies/s on the wire)", format_qps(peak_served));
                        println!("  Within SLO:    {:>12}  (p50 stays under {:.2} ms at this rate)", format_qps(best_ok_served), slo_ms);
                    } else {
                        println!("  Capacity:      {:>12}  (closed-loop knee — kernel-recv bound, NOT the server's raw max)", format_qps(peak_served));
                        println!("  Within SLO:    {:>12}  (p50 stays under {:.2} ms at this rate; dnsperf-comparable)", format_qps(best_ok_served), slo_ms);
                    }
                    // Final bisection bracket: the knee is pinned between the highest sustained
                    // target and the lowest saturated one (within the 5 % convergence tolerance).
                    let (klo, khi) = ctrl.bracket();
                    if khi > klo {
                        println!(
                            "Knee bracket (DSD bisection): [{} ; {}] q/s  (±{:.1}%)",
                            klo, khi, (khi - klo) as f64 / klo.max(1) as f64 * 100.0 / 2.0,
                        );
                    }
                    if !use_xdp {
                        println!(
                            "  → kernel-UDP caps this ramp (generator recv, not the server). For the \
                             server's RAW ceiling run open-loop:"
                        );
                        println!(
                            "      dnsmark -s <ip> -Q 0 --max-outstanding 0   (reports Server throughput \
                             (NIC rx) = server_rx_qps)"
                        );
                    }
                    sd.store(true, Ordering::Relaxed);
                    notify.notify_one();
                    break;
                }

                let new_per_worker = (new_qps / concurrent.max(1) as u64).max(1);
                qps_arc.store(new_per_worker, Ordering::Relaxed);
                let _ = new_qps; let _ = target_qps;
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
    // overrides; default 5 s; skipped for ramp and very short runs). Measured on the
    // X710+X520 rig the XDP/UMEM + NIC + resolver-cache ramp settles at ~5 s; a shorter
    // window leaves the sub-peak ramp in the average and under-reports the steady rate.
    if !config.ramp {
        let warmup = std::env::var("DNSMARK_WARMUP").ok()
            .and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
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
            // #188: PHY tx counter lives on the physical NIC, not the VLAN sub-iface.
            let i = crate::transport::xdp::parent_interface(&i).unwrap_or(i);
            if !ifs.contains(&i) { ifs.push(i); }
        }
        let tx0: u64 = ifs.iter().filter_map(|i| nic_wire_tx_packets(i)).sum();
        Some((ifs, tx0))
    } else { None };

    // Authoritative server throughput: replies arriving on the return NIC(s). Works in
    // BOTH kernel-UDP and XDP — in kernel mode it counts replies the socket later drops
    // (the RcvbufErrors that make userspace round-trip under-count); in XDP it cross-checks
    // the count-only round-trip. Captured over the same steady-state window.
    #[cfg(feature = "xdp")]
    let rx_baseline: Option<(Vec<String>, u64, u64)> = {
        let ifs = return_ifaces(&config);
        let rx0: u64    = ifs.iter().filter_map(|i| nic_rx_packets(i)).sum();
        let bytes0: u64 = ifs.iter().filter_map(|i| nic_rx_bytes(i)).sum();
        if rx0 > 0 || !ifs.is_empty() { Some((ifs, rx0, bytes0)) } else { None }
    };

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
    // In --ramp, carry the NIC-verified peak (Capacity) so reporting shows the knee, not
    // the whole-run average (which includes the ramp-up and reads far below the knee).
    if config.ramp {
        let cap = ramp_peak.load(Ordering::Relaxed);
        if cap > 0 { snap.ramp_capacity = Some(cap as f64); }
    }
    #[cfg(feature = "xdp")]
    if let Some((ifs, tx0)) = wt_baseline {
        let tx1: u64 = ifs.iter().filter_map(|i| nic_wire_tx_packets(i)).sum();
        let secs = elapsed.max(1e-9);
        snap.wire_qps = Some(tx1.saturating_sub(tx0) as f64 / secs);
    }
    #[cfg(feature = "xdp")]
    if let Some((ifs, rx0, bytes0)) = rx_baseline {
        let rx1: u64 = ifs.iter().filter_map(|i| nic_rx_packets(i)).sum();
        let secs = elapsed.max(1e-9);
        let dpkts = rx1.saturating_sub(rx0);
        snap.server_rx_qps = Some(dpkts as f64 / secs);
        // Average on-wire reply size + total egress-NIC link speed → % of line rate
        // (computed in the output layer). Same steady-state window as server_rx_qps.
        let bytes1: u64 = ifs.iter().filter_map(|i| nic_rx_bytes(i)).sum();
        let dbytes = bytes1.saturating_sub(bytes0);
        if dpkts > 0 && dbytes > 0 {
            snap.server_rx_avg_bytes = Some(dbytes as f64 / dpkts as f64);
        }
        let mbps: u64 = ifs.iter().filter_map(|i| crate::autodetect::nic_speed_mbps(i)).sum();
        if mbps > 0 { snap.link_mbps = Some(mbps); }
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
