pub mod csv;
pub mod json;
pub mod text;
pub mod tui;

use crate::config::Config;
use crate::stats::StatsSnapshot;

/// Line-rate context — answers "is this throughput limited by the server, or by the
/// Ethernet link?" from dnsmark's OWN hardware observations: the authoritative reply
/// rate (`server_rx_qps`, NIC rx counter), the average on-wire reply size
/// (`server_rx_avg_bytes`, NIC rx_bytes/rx_packets), and the egress-NIC link speed
/// (`link_mbps`). No receiver-side reading. `None` when the ingredients are absent
/// (e.g. non-XDP build, or NIC counters unavailable).
pub struct LineRate {
    pub rate_qps: f64,
    pub avg_reply_bytes: f64,
    pub link_mbps: u64,
    pub line_rate_pps: f64,
    pub pct: f64,
    /// True once the link is effectively saturated (≥ 90 % of line rate).
    pub wire_bound: bool,
}

/// Compute the line-rate context for a completed run. See [`LineRate`].
pub fn line_rate(snap: &StatsSnapshot) -> Option<LineRate> {
    // Prefer the authoritative NIC-measured reply rate; fall back to the userspace
    // round-trip if the hardware counter was unavailable.
    let rate_qps = snap.server_rx_qps.filter(|&r| r > 0.0).unwrap_or(snap.avg_qps);
    let avg_reply_bytes = snap.server_rx_avg_bytes?;
    let link_mbps = snap.link_mbps?;
    if rate_qps <= 0.0 || avg_reply_bytes <= 0.0 || link_mbps == 0 {
        return None;
    }
    // On-wire footprint per frame = L2 frame + 4 B FCS + 8 B preamble/SFD + 12 B IFG.
    let on_wire = avg_reply_bytes + 24.0;
    let line_rate_pps = (link_mbps as f64 * 1_000_000.0) / (on_wire * 8.0);
    let pct = if line_rate_pps > 0.0 { rate_qps / line_rate_pps * 100.0 } else { 0.0 };
    Some(LineRate {
        rate_qps,
        avg_reply_bytes,
        link_mbps,
        line_rate_pps,
        pct,
        wire_bound: pct >= 90.0,
    })
}

pub fn print_output(snap: &StatsSnapshot, config: &Config) -> anyhow::Result<()> {
    if config.json_output {
        json::print_json(snap, config)?;
    } else {
        text::print_result(snap, config);
    }

    if let Some(ref csv_path) = config.csv_file {
        csv::write_csv(snap, csv_path)?;
    }

    Ok(())
}
