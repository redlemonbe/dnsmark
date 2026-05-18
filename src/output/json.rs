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

    let json = serde_json::json!({
        "dnsmark_version": env!("CARGO_PKG_VERSION"),
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
    });

    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}
