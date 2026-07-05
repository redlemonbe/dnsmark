use std::net::IpAddr;
use std::sync::{
    atomic::AtomicBool,
    Arc,
};

use crate::config::Config;
use crate::stats::StatsSnapshot;

pub struct CompareResult {
    pub primary: StatsSnapshot,
    pub secondary: StatsSnapshot,
    pub primary_addr: IpAddr,
    pub secondary_addr: IpAddr,
}

pub async fn run_compare(
    config: Arc<Config>,
    secondary: IpAddr,
) -> anyhow::Result<CompareResult> {
    let primary_addr = config.server;

    let mut cfg_b = (*config).clone();
    cfg_b.server = secondary;
    cfg_b.no_tui = true;
    let cfg_b = Arc::new(cfg_b);

    let shutdown_a = Arc::new(AtomicBool::new(false));
    let shutdown_b = Arc::new(AtomicBool::new(false));

    let cfg_a_clone = config.clone();
    let cfg_b_clone = cfg_b.clone();
    let sd_a = shutdown_a.clone();
    let sd_b = shutdown_b.clone();

    let (snap_a, snap_b) = tokio::join!(
        super::run_with_shutdown(cfg_a_clone, sd_a),
        super::run_with_shutdown(cfg_b_clone, sd_b),
    );

    Ok(CompareResult {
        primary: snap_a?,
        secondary: snap_b?,
        primary_addr,
        secondary_addr: secondary,
    })
}

pub fn print_compare(result: &CompareResult) {
    println!("\n{:=<72}", "");
    println!("  Comparison: {} vs {}", result.primary_addr, result.secondary_addr);
    println!("{:=<72}", "");
    println!(
        "  {:30} {:>18} {:>18}",
        "Metric", result.primary_addr, result.secondary_addr
    );
    println!("  {:30} {:>18} {:>18}", "-".repeat(30), "-".repeat(18), "-".repeat(18));

    let a = &result.primary;
    let b = &result.secondary;

    println!(
        "  {:30} {:>17.0}  {:>17.0}",
        "Average QPS", a.avg_qps, b.avg_qps
    );
    println!(
        "  {:30} {:>16.3}ms {:>16.3}ms",
        "p50 latency",
        a.p50_us as f64 / 1000.0,
        b.p50_us as f64 / 1000.0
    );
    println!(
        "  {:30} {:>16.3}ms {:>16.3}ms",
        "p95 latency",
        a.p95_us as f64 / 1000.0,
        b.p95_us as f64 / 1000.0
    );
    println!(
        "  {:30} {:>16.3}ms {:>16.3}ms",
        "p99 latency",
        a.p99_us as f64 / 1000.0,
        b.p99_us as f64 / 1000.0
    );
    println!(
        "  {:30} {:>17.2}% {:>17.2}%",
        "Completion rate",
        if a.queries_sent > 0 {
            a.queries_completed as f64 / a.queries_sent as f64 * 100.0
        } else {
            0.0
        },
        if b.queries_sent > 0 {
            b.queries_completed as f64 / b.queries_sent as f64 * 100.0
        } else {
            0.0
        }
    );
    println!("{:=<72}\n", "");
}
