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
        serde_json::json!({ "type": "random" })
    };

    // Flag a result that may be bounded by the receiver's NIC/bus rather than the server.
    let mut notes: Vec<String> = Vec::new();
    if snap.queries_sent > 0 {
        let loss = snap.queries_lost as f64 / snap.queries_sent as f64;
        if loss > 0.05 {
            notes.push(format!(
                "High loss ({:.1}%): at this offered rate the bottleneck may be the receiver's \
                 NIC/bus (or the generator's RX), not the server software. Read the receiver's \
                 NIC counters (`ethtool -S`) for true throughput — avg_qps under-counts under \
                 saturation. See docs/benchmarking.md §3.",
                loss * 100.0));
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
        "notes": notes,
    });

    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}
