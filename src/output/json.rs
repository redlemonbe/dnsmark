use crate::config::Config;
use crate::stats::StatsSnapshot;

pub fn print_json(snap: &StatsSnapshot, config: &Config) -> anyhow::Result<()> {
    let qps_cap = if config.qps == 0 {
        serde_json::Value::String("unlimited".to_string())
    } else {
        serde_json::Value::Number(config.qps.into())
    };

    let source = if config.random {
        let qtype = if config.random_qtype == 28 { "aaaa" } else { "a" };
        serde_json::json!({ "type": "random", "domain": config.random_domain, "qtype": qtype })
    } else if let Some(ref f) = config.query_file {
        serde_json::json!({ "type": "file", "path": f.to_string_lossy() })
    } else {
        // No -d and not --random ⇒ the engine uses the built-in 2000-domain corpus.
        serde_json::json!({ "type": "builtin", "domains": 2000 })
    };

    // Line-rate context: whether the run is wire-bound or server-bound, from dnsmark's
    // own hardware observations (server_rx_qps + avg reply size + link speed). Fixed/flood
    // only — in --ramp the NIC rx counter spans the whole ramp-up (avg ≪ peak), so the DSD
    // "Capacity (NIC-verified)" summary is the throughput answer, not this ratio.
    let lr = if config.ramp { None } else { crate::output::line_rate(snap) };
    let line_rate_json = lr.as_ref().map(|l| serde_json::json!({
        "rate_qps":        l.rate_qps,
        "avg_reply_bytes": l.avg_reply_bytes,
        "link_mbps":       l.link_mbps,
        "line_rate_pps":   l.line_rate_pps,
        "percent_of_line": l.pct,
        "verdict":         if l.wire_bound { "wire-bound" } else { "link-headroom" },
    }));

    let mut notes: Vec<String> = Vec::new();
    if let Some(l) = lr.as_ref() {
        if l.wire_bound {
            notes.push(format!(
                "WIRE-BOUND: {:.0}% of {} Gb/s line rate at {:.0} B/reply. The Ethernet link is \
                 saturated — the server is NOT the limit. server_rx_qps is the true throughput \
                 (line rate); faster/more NICs (25/40/100G) are needed to push higher.",
                l.pct, l.link_mbps / 1000, l.avg_reply_bytes));
        } else {
            notes.push(format!(
                "link has headroom: {:.0}% of {} Gb/s line rate. The bottleneck is the server or \
                 the generator, NOT the wire — server_rx_qps is the authoritative reply rate.",
                l.pct, l.link_mbps / 1000));
        }
    }

    let json = serde_json::json!({
        "dnsmark_version": env!("CARGO_PKG_VERSION"),
        "host": crate::autodetect::host_info_json(config.server),
        "parameters": {
            "server": config.server.to_string(),
            "port":   config.port,
            "protocol": config.protocol.as_str(),
            "concurrent": config.concurrent,
            "qps_cap":   qps_cap,
            "duration_secs": config.duration_secs,
            "timeout_ms":    config.timeout_ms,
            "mode": if config.ramp { "ramp" } else { "fixed" },
            "source": source,
        },
        "statistics": snap,
        "line_rate": line_rate_json,
        "notes": notes,
    });

    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}
