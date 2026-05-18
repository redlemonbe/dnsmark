use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
    Dot,
}

impl Protocol {
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "udp" => Ok(Self::Udp),
            "tcp" => Ok(Self::Tcp),
            "dot" => Ok(Self::Dot),
            other => anyhow::bail!("unknown protocol '{}', use udp|tcp|dot", other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Udp => "UDP",
            Self::Tcp => "TCP",
            Self::Dot => "DoT",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub server: IpAddr,
    pub port: u16,
    pub query_file: Option<PathBuf>,
    pub concurrent: usize,
    pub qps: u64,
    pub duration_secs: u64,
    pub timeout_ms: u64,
    pub threads: usize,
    pub quiet: bool,
    pub verbose: bool,
    pub stats_interval_secs: u64,
    pub ramp: bool,
    pub random: bool,
    pub random_domain: String,
    pub compare: Option<IpAddr>,
    pub protocol: Protocol,
    pub json_output: bool,
    pub csv_file: Option<PathBuf>,
    pub no_tui: bool,
    pub force_xdp: bool,
    pub no_xdp: bool,
}
