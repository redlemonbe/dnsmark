use std::path::Path;

use crate::stats::StatsSnapshot;

pub fn write_csv(snap: &StatsSnapshot, path: &Path) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_path(path)?;
    wtr.write_record([
        "queries_sent",
        "queries_completed",
        "queries_lost",
        "rcode_noerror",
        "rcode_nxdomain",
        "rcode_servfail",
        "rcode_refused",
        "rcode_other",
        "run_time_s",
        "avg_qps",
        "min_us",
        "avg_us",
        "p50_us",
        "p95_us",
        "p99_us",
        "p999_us",
        "max_us",
    ])?;
    wtr.write_record([
        snap.queries_sent.to_string(),
        snap.queries_completed.to_string(),
        snap.queries_lost.to_string(),
        snap.rcode_noerror.to_string(),
        snap.rcode_nxdomain.to_string(),
        snap.rcode_servfail.to_string(),
        snap.rcode_refused.to_string(),
        snap.rcode_other.to_string(),
        format!("{:.3}", snap.run_time_s),
        format!("{:.0}", snap.avg_qps),
        snap.min_us.to_string(),
        format!("{:.1}", snap.avg_us),
        snap.p50_us.to_string(),
        snap.p95_us.to_string(),
        snap.p99_us.to_string(),
        snap.p999_us.to_string(),
        snap.max_us.to_string(),
    ])?;
    wtr.flush()?;
    Ok(())
}
