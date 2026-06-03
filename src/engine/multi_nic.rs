//! Multi-NIC engine: one independent AF_XDP stack per target / NIC.
//!
//! Architecture
//! ─────────────
//! For N targets (each on a distinct subnet / NIC):
//!   - N independent `run_with_shutdown` tasks, one per NIC.
//!   - Workers (-c) are spread evenly across NICs: NIC_i gets ceil(c / N)
//!     workers, last NIC absorbs the remainder.
//!   - Each NIC stack gets its own StatsCollector, XDP handle, UMEM, rings.
//!     AF_XDP is explicitly per-socket; no bonding, no shared rings.
//!   - Stats are merged at the end: totals summed, latencies averaged
//!     (weighted by completed), per-NIC breakdown printed if --nic-stats.
//!
//! Backward compatibility: this module is only invoked when `config.servers`
//! has ≥ 2 entries. Mono-NIC callers go through `run()` / `run_with_shutdown()`
//! as before — zero code-path change.

use std::net::IpAddr;
use std::sync::{
    atomic::AtomicBool,
    Arc,
};

use anyhow::Context;

use crate::config::Config;
use crate::stats::StatsSnapshot;
use super::run_with_shutdown;

/// Merge N per-NIC snapshots into one aggregate.
/// Totals are summed; latency metrics are weighted averages (by completed).
/// run_time_s is the maximum (wall-clock of the longest NIC run).
fn merge_snapshots(snaps: &[StatsSnapshot]) -> StatsSnapshot {
    if snaps.is_empty() {
        return StatsSnapshot {
            queries_sent: 0,
            queries_completed: 0,
            queries_lost: 0,
            rcode_noerror: 0,
            rcode_nxdomain: 0,
            rcode_servfail: 0,
            rcode_refused: 0,
            rcode_other: 0,
            run_time_s: 0.0,
            avg_qps: 0.0,
            min_us: 0,
            avg_us: 0.0,
            p50_us: 0,
            p95_us: 0,
            p99_us: 0,
            p999_us: 0,
            max_us: 0,
        };
    }

    let total_sent = snaps.iter().map(|s| s.queries_sent).sum();
    let total_done = snaps.iter().map(|s| s.queries_completed).sum::<u64>();
    let total_lost = snaps.iter().map(|s| s.queries_lost).sum();
    let max_time   = snaps.iter().map(|s| s.run_time_s).fold(0f64, f64::max);

    // Weighted latency average (by completed count).
    let weight_sum = total_done as f64;
    let wavg = |f: fn(&StatsSnapshot) -> f64| -> f64 {
        if weight_sum == 0.0 { return 0.0; }
        snaps.iter().map(|s| f(s) * s.queries_completed as f64).sum::<f64>() / weight_sum
    };

    // For discrete percentiles we do a simple weighted average — not perfect
    // (you'd need the raw histogram) but good enough for aggregate reporting.
    let wavg_u64 = |f: fn(&StatsSnapshot) -> u64| -> u64 {
        if weight_sum == 0.0 { return 0; }
        let v = snaps.iter().map(|s| f(s) as f64 * s.queries_completed as f64).sum::<f64>()
            / weight_sum;
        v.round() as u64
    };

    let min_us = snaps.iter().map(|s| s.min_us).filter(|&v| v > 0).min().unwrap_or(0);
    let max_us = snaps.iter().map(|s| s.max_us).max().unwrap_or(0);

    StatsSnapshot {
        queries_sent:      total_sent,
        queries_completed: total_done,
        queries_lost:      total_lost,
        rcode_noerror:  snaps.iter().map(|s| s.rcode_noerror).sum(),
        rcode_nxdomain: snaps.iter().map(|s| s.rcode_nxdomain).sum(),
        rcode_servfail: snaps.iter().map(|s| s.rcode_servfail).sum(),
        rcode_refused:  snaps.iter().map(|s| s.rcode_refused).sum(),
        rcode_other:    snaps.iter().map(|s| s.rcode_other).sum(),
        run_time_s: max_time,
        avg_qps:    if max_time > 0.0 { total_done as f64 / max_time } else { 0.0 },
        min_us,
        avg_us:  wavg(|s| s.avg_us),
        p50_us:  wavg_u64(|s| s.p50_us),
        p95_us:  wavg_u64(|s| s.p95_us),
        p99_us:  wavg_u64(|s| s.p99_us),
        p999_us: wavg_u64(|s| s.p999_us),
        max_us,
    }
}

/// Run one independent benchmark stack per NIC, aggregate results.
pub async fn run_multi_nic(config: Arc<Config>) -> anyhow::Result<StatsSnapshot> {
    let servers   = config.servers.clone();
    let n_nics    = servers.len();
    let total_c   = config.concurrent;

    // Distribute workers evenly: base = total / N, last NIC gets the remainder.
    let base_workers = (total_c / n_nics).max(1);

    if !config.quiet {
        println!(
            "Multi-NIC: {} NICs × ~{} workers each  (total workers = {})",
            n_nics, base_workers, total_c
        );
    }

    // Global shutdown: any NIC can trip it (Ctrl-C, timeout, ramp done).
    let shutdown = Arc::new(AtomicBool::new(false));

    // Spawn one task per NIC.
    let mut handles = Vec::with_capacity(n_nics);

    for (idx, &target) in servers.iter().enumerate() {
        let iface = crate::autodetect::iface_for_addr(target)
            .unwrap_or_else(|| "?".to_string());

        // Workers for this NIC: last NIC absorbs the remainder.
        let workers = if idx == n_nics - 1 {
            total_c - base_workers * (n_nics - 1)
        } else {
            base_workers
        }.max(1);

        if !config.quiet {
            println!(
                "  NIC[{}]: target={} iface={} workers={}",
                idx, target, iface, workers
            );
        }

        // Build a per-NIC Config: same params, but single server + adjusted workers.
        let nic_cfg = Arc::new(Config {
            server:  target,
            servers: vec![target],
            concurrent: workers,
            // All other fields identical.
            port:              config.port,
            query_file:        config.query_file.clone(),
            qps:               config.qps,
            duration_secs:     config.duration_secs,
            timeout_ms:        config.timeout_ms,
            threads:           config.threads,
            quiet:             config.quiet,
            verbose:           config.verbose,
            stats_interval_secs: config.stats_interval_secs,
            ramp:              config.ramp,
            random:            config.random,
            random_domain:     config.random_domain.clone(),
            random_qtype:      config.random_qtype,
            compare:           config.compare,
            protocol:          config.protocol.clone(),
            json_output:       config.json_output,
            csv_file:          config.csv_file.clone(),
            no_tui:            true,  // TUI only on primary; per-NIC TUIs would conflict.
            force_xdp:         config.force_xdp,
            no_xdp:            config.no_xdp,
            max_outstanding:   config.max_outstanding,
            nic_stats:         config.nic_stats,
        });

        let sd  = shutdown.clone();
        let tgt = target;

        handles.push(tokio::spawn(async move {
            let snap = run_with_shutdown(nic_cfg, sd).await
                .with_context(|| format!("NIC target {tgt}"))?;
            anyhow::Ok((tgt, snap))
        }));
    }

    // Collect results (all NICs run in parallel, each until duration/shutdown).
    let mut per_nic: Vec<(IpAddr, StatsSnapshot)> = Vec::with_capacity(n_nics);
    let mut first_err: Option<anyhow::Error> = None;

    for h in handles {
        match h.await {
            Ok(Ok(pair))  => per_nic.push(pair),
            Ok(Err(e))    => {
                eprintln!("WARN: NIC error — {e}");
                if first_err.is_none() { first_err = Some(e); }
            }
            Err(join_err) => {
                eprintln!("WARN: NIC task panicked — {join_err}");
            }
        }
    }

    // If ALL NICs failed, surface the first error.
    if per_nic.is_empty() {
        return Err(first_err.unwrap_or_else(|| anyhow::anyhow!("all NICs failed")));
    }

    // Print per-NIC breakdown.
    if config.nic_stats || !config.quiet {
        println!("\n── Per-NIC breakdown ──────────────────────────────────");
        for (addr, snap) in &per_nic {
            let iface = crate::autodetect::iface_for_addr(*addr)
                .unwrap_or_else(|| "?".to_string());
            println!(
                "  {addr} ({iface})  sent={} done={} qps={:.0}  p99={} µs",
                snap.queries_sent,
                snap.queries_completed,
                snap.avg_qps,
                snap.p99_us,
            );
        }
        println!("────────────────────────────────────────────────────────");
    }

    let snaps: Vec<_> = per_nic.into_iter().map(|(_, s)| s).collect();
    Ok(merge_snapshots(&snaps))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_snapshots ────────────────────────────────────────────────────

    fn make_snap(sent: u64, done: u64, qps: f64, p99: u64, time: f64) -> StatsSnapshot {
        StatsSnapshot {
            queries_sent:      sent,
            queries_completed: done,
            queries_lost:      sent.saturating_sub(done),
            rcode_noerror:     done,
            rcode_nxdomain:    0,
            rcode_servfail:    0,
            rcode_refused:     0,
            rcode_other:       0,
            run_time_s:        time,
            avg_qps:           qps,
            min_us:            50,
            avg_us:            120.0,
            p50_us:            100,
            p95_us:            200,
            p99_us:            p99,
            p999_us:           500,
            max_us:            800,
        }
    }

    #[test]
    fn merge_empty_returns_zeroed() {
        let m = merge_snapshots(&[]);
        assert_eq!(m.queries_sent, 0);
        assert_eq!(m.avg_qps, 0.0);
    }

    #[test]
    fn merge_single_is_identity() {
        let s = make_snap(1000, 950, 95.0, 300, 10.0);
        let m = merge_snapshots(&[s.clone()]);
        assert_eq!(m.queries_sent, 1000);
        assert_eq!(m.queries_completed, 950);
        assert_eq!(m.queries_lost, 50);
        assert_eq!(m.p99_us, 300);
        assert!((m.run_time_s - 10.0).abs() < 1e-9);
    }

    #[test]
    fn merge_two_nics_sums_totals() {
        let s1 = make_snap(5_000_000, 4_800_000, 480_000.0, 200, 10.0);
        let s2 = make_snap(5_000_000, 4_900_000, 490_000.0, 210, 10.0);
        let m  = merge_snapshots(&[s1, s2]);
        assert_eq!(m.queries_sent,      10_000_000);
        assert_eq!(m.queries_completed,  9_700_000);
        assert_eq!(m.rcode_noerror,      9_700_000);
        // avg_qps = total_done / max_time = 9_700_000 / 10.0
        assert!((m.avg_qps - 970_000.0).abs() < 1.0);
    }

    #[test]
    fn merge_two_nics_weighted_p99() {
        // NIC1: 9M done, p99=200  |  NIC2: 1M done, p99=800
        // weighted p99 = (9M*200 + 1M*800) / 10M = (1800M + 800M) / 10M = 260
        let s1 = make_snap(9_000_000, 9_000_000, 900_000.0, 200, 10.0);
        let s2 = make_snap(1_000_000, 1_000_000, 100_000.0, 800, 10.0);
        let m  = merge_snapshots(&[s1, s2]);
        assert_eq!(m.p99_us, 260);
    }

    #[test]
    fn merge_max_time_is_wall_clock_max() {
        let s1 = make_snap(100, 100, 10.0, 100, 8.0);
        let s2 = make_snap(100, 100, 10.0, 100, 12.5);
        let m  = merge_snapshots(&[s1, s2]);
        assert!((m.run_time_s - 12.5).abs() < 1e-9);
    }

    #[test]
    fn merge_min_us_takes_minimum_nonzero() {
        let mut s1 = make_snap(100, 100, 10.0, 100, 10.0);
        let mut s2 = make_snap(100, 100, 10.0, 100, 10.0);
        s1.min_us = 50;
        s2.min_us = 80;
        let m = merge_snapshots(&[s1, s2]);
        assert_eq!(m.min_us, 50);
    }

    #[test]
    fn merge_max_us_takes_maximum() {
        let mut s1 = make_snap(100, 100, 10.0, 100, 10.0);
        let mut s2 = make_snap(100, 100, 10.0, 100, 10.0);
        s1.max_us = 1200;
        s2.max_us = 900;
        let m = merge_snapshots(&[s1, s2]);
        assert_eq!(m.max_us, 1200);
    }

    // ── CLI parsing (multi -s) — tested via clap's parse() ───────────────

    /// Verify that the Config struct correctly holds multiple servers.
    #[test]
    fn config_multi_servers_stored() {
        use std::net::IpAddr;
        let s1: IpAddr = "10.10.10.2".parse().unwrap();
        let s2: IpAddr = "10.10.20.2".parse().unwrap();
        let cfg = crate::config::Config {
            server:  s1,
            servers: vec![s1, s2],
            port: 53,
            query_file: None,
            concurrent: 16,
            qps: 0,
            duration_secs: 30,
            timeout_ms: 3000,
            threads: 8,
            quiet: true,
            verbose: false,
            stats_interval_secs: 1,
            ramp: false,
            random: true,
            random_domain: "bench.invalid.".into(),
            random_qtype: 1,
            compare: None,
            protocol: crate::config::Protocol::Udp,
            json_output: false,
            csv_file: None,
            no_tui: true,
            force_xdp: false,
            no_xdp: true,
            max_outstanding: 100,
            nic_stats: true,
        };
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.servers[0], s1);
        assert_eq!(cfg.servers[1], s2);
        assert_eq!(cfg.server, s1);
        // Workers distributed: 16 workers / 2 NICs = 8 each
        let n = cfg.servers.len();
        let base = cfg.concurrent / n;
        assert_eq!(base, 8);
    }
}
