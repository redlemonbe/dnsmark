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
        // No -d and not --random ⇒ the engine uses the built-in 2000-domain corpus.
        "builtin (2000 domains)".to_string()
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
    // Egress throughput = what the NIC actually sent (TX completions)
    println!("  Send throughput (egress):  {:.0} qps  ← match this to rx_packets on receiver NIC", snap.send_qps);

    // wire-truth guard: the submitted-descriptor egress above is fictional if
    // the NIC never put the frames on the wire. Show the PHY-confirmed rate and
    // shout if they diverge (wedged ixgbe ZC TX, bad queue setup, etc.).
    if let Some(wire) = snap.wire_qps {
        println!("  Wire egress (NIC PHY):     {:.0} qps  (confirmed transmitted)", wire);
        if snap.send_qps > 1_000.0 && wire < snap.send_qps * 0.5 {
            eprintln!(
                "\x1b[1;31m[dnsmark] WARNING: reported egress ({:.0} qps) is NOT reaching the wire \
                 - NIC PHY confirmed only {:.0} qps. The XDP TX path is wedged or \
                 misconfigured; the throughput number above is fictional. \
                 (ixgbe X520: try a host reboot; modprobe reload is insufficient.)\x1b[0m",
                snap.send_qps, wire);
        }
    }
    // Round-trip metric = responses matched/counted in userspace (latency/loss tool).
    let rt_pct = if snap.send_qps > 0.0 { snap.avg_qps / snap.send_qps * 100.0 } else { 0.0 };
    println!("  Round-trip completed:      {:.0} qps  ({:.1}% of egress, userspace)", snap.avg_qps, rt_pct);

    // Authoritative server throughput: replies that physically arrived on the NIC.
    // In kernel-UDP the socket can drop replies (RcvbufErrors) so userspace round-trip
    // under-counts; the NIC rx counter is the truth. In XDP it confirms round-trip.
    if let Some(rx) = snap.server_rx_qps {
        println!("  Server throughput (NIC rx):  {:.0} qps  (authoritative — replies on the wire)", rx);
        if snap.avg_qps > 1_000.0 && rx > snap.avg_qps * 1.10 {
            let dropped = rx - snap.avg_qps;
            eprintln!(
                "\x1b[33m[dnsmark] NOTE: the SERVER answered {:.0} qps (counted at the NIC, \
                 incl. ring-overflow drops) — that is the authoritative throughput. The \
                 generator's receive path only delivered {:.0} qps to userspace; ~{:.0} qps \
                 of replies were dropped on the GENERATOR side (kernel socket/NIC-ring overflow, \
                 or an RX queue not drained fast enough — e.g. a non-NUMA-local stack in \
                 multi-NIC). The SERVER is fine. In kernel-UDP, use --xdp; in --xdp multi-NIC, \
                 ensure each stack is NUMA-local to its NIC.\x1b[0m",
                rx, snap.avg_qps, dropped);
        }
    }

    // --ramp: the whole-run averages above include the ramp-up and sit far below the knee.
    // The NIC-verified Capacity (peak served, summed across NICs in multi-NIC) is the answer.
    if let Some(cap) = snap.ramp_capacity {
        println!("  Capacity (ramp knee):      {:.0} qps  (NIC-verified peak — the --ramp throughput answer)", cap);
    }

    // Line-rate context: dnsmark states, from its own hardware observations, whether the
    // run is limited by the Ethernet link or by the server — no receiver-side reading.
    // Fixed/flood mode only: server_rx_qps then reflects one steady window. In --ramp the
    // NIC rx counter spans the whole ramp-up (its average is far below the peak), so the DSD
    // "Capacity: … (NIC-verified)" line is the throughput answer there, not this ratio.
    if !config.ramp {
    if let Some(lr) = crate::output::line_rate(snap) {
        println!(
            "  Line rate:                 {:.0}% of {} Gb/s wire  ({:.0} B replies, ceiling {:.2} M/s)",
            lr.pct, lr.link_mbps / 1000, lr.avg_reply_bytes, lr.line_rate_pps / 1e6
        );
        if lr.wire_bound {
            println!("    → WIRE-BOUND: the Ethernet link is saturated — this IS the max for this");
            println!("      reply size on this link. Use faster/more NICs (25/40/100G) to push higher.");
        } else {
            println!("    → link has headroom: the limit is the server or the generator, not the wire");
            println!("      (raise offered load, or check the generator isn't the bottleneck).");
        }
    }
    }
    println!();
    println!("  Latency:");
    println!("    min:       {:.3} ms", snap.min_us as f64 / 1000.0);
    println!("    avg:       {:.3} ms", snap.avg_us / 1000.0);
    println!("    p50:       {:.3} ms", snap.p50_us  as f64 / 1000.0);
    println!("    p95:       {:.3} ms", snap.p95_us  as f64 / 1000.0);
    println!("    p99:       {:.3} ms", snap.p99_us  as f64 / 1000.0);
    println!("    p999:      {:.3} ms", snap.p999_us as f64 / 1000.0);
    println!("    max:       {:.3} ms", snap.max_us  as f64 / 1000.0);
    if snap.inflight_max > 0 {
        println!("  outstanding:");
        println!("    mean:      {:.1}", snap.inflight_mean);
        println!("    max:       {}", snap.inflight_max);
    }
    println!();
    println!("  Run time: {:.3} s", snap.run_time_s);
}
