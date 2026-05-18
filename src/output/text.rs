use crate::config::Config;
use crate::stats::StatsSnapshot;

pub fn print_result(snap: &StatsSnapshot, config: &Config) {
    println!(
        "DNS Performance Testing Tool — dnsmark {}",
        env!("CARGO_PKG_VERSION")
    );
    println!("[DISCLAIMER: authorized testing only]");
    println!();

    // --- Parameters (for reproducibility) ---
    println!("Parameters:");
    println!();
    let source = if config.random {
        let qtype = if config.random_qtype == 28 { "AAAA" } else { "A" };
        format!("random ({} {})", config.random_domain, qtype)
    } else if let Some(ref f) = config.query_file {
        format!("file ({})", f.display())
    } else {
        "random".to_string()
    };
    let qps_str = if config.qps == 0 {
        "unlimited".to_string()
    } else {
        config.qps.to_string()
    };
    let mode = if config.ramp { "ramp" } else { "fixed" };
    println!("  Server:       {}:{}", config.server, config.port);
    println!("  Protocol:     {}", config.protocol.as_str());
    println!("  Clients:      {}", config.concurrent);
    println!("  QPS cap:      {}", qps_str);
    println!("  Duration:     {} s", config.duration_secs);
    println!("  Timeout:      {} ms", config.timeout_ms);
    println!("  Mode:         {}", mode);
    println!("  Source:       {}", source);
    println!();

    // --- Statistics ---
    let sent = snap.queries_sent;
    let completed = snap.queries_completed;
    let lost = snap.queries_lost;

    let pct = |n: u64, d: u64| -> f64 {
        if d == 0 { 0.0 } else { n as f64 / d as f64 * 100.0 }
    };

    println!("Statistics:");
    println!();
    println!("  Queries sent:         {}", sent);
    println!("  Queries completed:    {:<10}  ({:.2}%)", completed, pct(completed, sent));
    println!("  Queries lost:         {:<10}  ({:.2}%)", lost, pct(lost, sent));
    println!();
    println!("  Response codes:");
    println!("    NOERROR:            {:<10}  ({:.2}%)", snap.rcode_noerror,  pct(snap.rcode_noerror,  completed));
    println!("    NXDOMAIN:           {:<10}  ({:.2}%)", snap.rcode_nxdomain, pct(snap.rcode_nxdomain, completed));
    println!("    SERVFAIL:           {:<10}  ({:.2}%)", snap.rcode_servfail, pct(snap.rcode_servfail, completed));
    println!("    REFUSED:            {:<10}  ({:.2}%)", snap.rcode_refused,  pct(snap.rcode_refused,  completed));
    println!();
    println!("  Average QPS:          {:.0}", snap.avg_qps);
    println!("  Throughput:           {:.0} qps", snap.avg_qps);
    println!();
    println!("  Latency:");
    println!("    min:       {:.3} ms", snap.min_us as f64 / 1000.0);
    println!("    avg:       {:.3} ms", snap.avg_us / 1000.0);
    println!("    p50:       {:.3} ms", snap.p50_us  as f64 / 1000.0);
    println!("    p95:       {:.3} ms", snap.p95_us  as f64 / 1000.0);
    println!("    p99:       {:.3} ms", snap.p99_us  as f64 / 1000.0);
    println!("    p999:      {:.3} ms", snap.p999_us as f64 / 1000.0);
    println!("    max:       {:.3} ms", snap.max_us  as f64 / 1000.0);
    println!();
    println!("  Run time: {:.3} s", snap.run_time_s);
}
