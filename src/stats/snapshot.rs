#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsSnapshot {
    pub queries_sent: u64,
    pub queries_completed: u64,
    pub queries_lost: u64,
    pub rcode_noerror: u64,
    pub rcode_nxdomain: u64,
    pub rcode_servfail: u64,
    pub rcode_refused: u64,
    pub rcode_other: u64,
    pub run_time_s: f64,
    pub send_qps: f64,   // egress throughput: completions/s from TX CQ
    pub avg_qps: f64,
    pub min_us: u64,
    pub avg_us: f64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
    pub inflight_mean: f64,
    pub inflight_max: u64,
    /// PHY-confirmed wire egress (NIC tx_pkts_nic delta / run). None if not measured.
    pub wire_qps: Option<f64>,
}
