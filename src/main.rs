pub mod simd;
mod autodetect;
mod config;
mod dns;
mod engine;
mod output;
mod query;
mod stats;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;

use config::{Config, Protocol};

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const DISCLAIMER: &str = "dnsmark is provided for authorized performance testing only.\n\
    The authors disclaim all liability for any unauthorized,\n\
    abusive, or malicious use of this tool.";

#[derive(Parser)]
#[command(
    name = "dnsmark",
    version = env!("CARGO_PKG_VERSION"),
    about = "High-performance DNS benchmark — drop-in dnsperf replacement",
    after_help = DISCLAIMER,
)]
struct Cli {
    /// Target DNS server IP(s). Repeat -s for multi-NIC flood (one XDP stack per target).
    /// Each target must be on a distinct subnet routed via a distinct NIC.
    /// Single -s = legacy mono-NIC behaviour (unchanged).
    #[arg(short = 's', default_value = "127.0.0.1", action = clap::ArgAction::Append)]
    server: Vec<std::net::IpAddr>,

    /// Target port
    #[arg(short = 'p', default_value_t = 53)]
    port: u16,

    /// Query file (dnsperf format: "domain type" per line)
    #[arg(short = 'd')]
    query_file: Option<PathBuf>,

    /// Concurrent clients (auto = physical cores, HT excluded; 0 = auto; or integer)
    #[arg(short = 'c', default_value = "auto")]
    clients: String,

    /// Max QPS target (0 = unlimited)
    #[arg(short = 'Q', default_value_t = 0)]
    qps: u64,

    /// Test duration in seconds
    #[arg(short = 'l', default_value_t = 30)]
    duration: u64,

    /// Query timeout in milliseconds
    #[arg(short = 't', default_value_t = 3000)]
    timeout_ms: u64,

    /// Tokio worker threads (default: num_cpus)
    #[arg(short = 'T')]
    threads: Option<usize>,

    /// Quiet mode (no TUI, final result only)
    #[arg(short = 'q')]
    quiet: bool,

    /// Verbose mode (log each query)
    #[arg(short = 'v')]
    verbose: bool,

    /// Intermediate stats interval in seconds
    #[arg(short = 'S', default_value_t = 1)]
    stats_interval: u64,

    /// Ramp mode: auto-scale from 1000 QPS doubling every 5s until saturation
    #[arg(long)]
    ramp: bool,

    /// Generate random UUID subdomain queries
    #[arg(long)]
    random: bool,

    /// Base domain for random queries
    #[arg(long, default_value = "bench.invalid.")]
    random_domain: String,

    /// Record type for --random queries: a or aaaa
    #[arg(long, default_value = "a")]
    random_type: String,

    /// Compare with a second server (run both in parallel)
    #[arg(long)]
    compare: Option<std::net::IpAddr>,

    /// Transport protocol: udp|tcp|dot
    #[arg(long, default_value = "udp")]
    protocol: String,

    /// Output results as JSON
    #[arg(long)]
    json: bool,

    /// Write results to CSV file
    #[arg(long)]
    csv: Option<PathBuf>,

    /// Disable live TUI dashboard
    #[arg(long)]
    no_tui: bool,

    /// Enable AF_XDP datapath (XDP mode, for benchmarking AF_XDP servers like Runbound).
    /// Default transport is UDP kernel socket (comparable to dnsperf).
    /// Use --xdp only for symmetric XDP-vs-XDP measurements; never mix transports.
    #[arg(long)]
    xdp: bool,

    /// Disable AF_XDP (default: AF_XDP is already disabled unless --xdp is set)
    #[arg(long)]
    no_xdp: bool,

    /// Max outstanding queries total across all workers, 0 = unlimited (mirrors dnsperf -q N×clients)
    #[arg(long, default_value_t = 100)]
    max_outstanding: usize,

    /// Show per-NIC stats breakdown in multi-NIC mode
    #[arg(long)]
    nic_stats: bool,
}

fn main() -> anyhow::Result<()> {
    simd::log_simd_info();
    let cli = Cli::parse();

    // Deduplicate servers; preserve order. At least one is always present (default).
    let servers: Vec<std::net::IpAddr> = {
        let mut seen = std::collections::HashSet::new();
        cli.server.iter().filter(|s| seen.insert(**s)).copied().collect()
    };
    let primary = servers[0];

    // Multi-NIC sanity: each target must resolve to a distinct interface.
    if servers.len() > 1 {
        let mut ifaces: std::collections::HashMap<String, std::net::IpAddr> =
            std::collections::HashMap::new();
        for &srv in &servers {
            if let Some(iface) = crate::autodetect::iface_for_addr(srv) {
                if let Some(prev) = ifaces.get(&iface) {
                    anyhow::bail!(
                        "Targets {} and {} both route via '{}'. \
                         Multi-NIC requires distinct NICs per target (no bonding).",
                        prev, srv, iface
                    );
                }
                ifaces.insert(iface, srv);
            }
        }
        if !cli.quiet {
            println!(
                "Multi-NIC mode: {} targets / {} interfaces",
                servers.len(),
                ifaces.len()
            );
        }
    }

    // XDP check (feature guard)
    if cli.xdp {
        #[cfg(feature = "xdp")]
        {
            let auto = autodetect::detect();
            if !auto.af_xdp_socket_available {
                anyhow::bail!("AF_XDP not available on this system");
            }
        }
        #[cfg(not(feature = "xdp"))]
        {
            eprintln!(
                "Built without XDP support. Recompile with --features xdp"
            );
            std::process::exit(1);
        }
    }

    // Tracing
    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    transport::init_cpu_pinning(primary);

    // Auto-detection
    let auto = autodetect::detect();
    let protocol = Protocol::from_str(&cli.protocol)?;

    let physical = autodetect::physical_cores();
    let physical_count = physical.len();
    let auto_concurrent = physical_count.max(8);
    let (concurrent, concurrent_auto) = match cli.clients.as_str() {
        "auto" | "0" => (auto_concurrent, true),
        n => (n.parse::<usize>().context("invalid -c value")?.max(1), false),
    };

    let threads = cli.threads.unwrap_or(auto.cpus).max(1);

    if !cli.quiet {
        if concurrent_auto {
            if physical_count < 8 {
                println!(
                    "Workers: {} (auto — min 8, VM has {} physical core{})",
                    concurrent,
                    physical_count,
                    if physical_count == 1 { "" } else { "s" }
                );
            } else {
                println!(
                    "Workers: {} (auto — physical cores, HT excluded)",
                    concurrent
                );
            }
        } else {
            println!("Workers: {} (manual)", concurrent);
        }
    }

    let config = Arc::new(Config {
        server: primary,
        servers,
        port: cli.port,
        query_file: cli.query_file,
        concurrent,
        qps: cli.qps,
        duration_secs: cli.duration,
        timeout_ms: cli.timeout_ms,
        threads,
        quiet: cli.quiet,
        verbose: cli.verbose,
        stats_interval_secs: cli.stats_interval,
        ramp: cli.ramp,
        random: cli.random,
        random_domain: cli.random_domain,
        random_qtype: match cli.random_type.to_lowercase().as_str() {
            "aaaa" => 28,
            _ => 1,
        },
        compare: cli.compare,
        protocol,
        json_output: cli.json,
        csv_file: cli.csv,
        no_tui: cli.no_tui || cli.quiet,
        force_xdp: cli.xdp,
        no_xdp: cli.no_xdp,
        max_outstanding: cli.max_outstanding,
        nic_stats: cli.nic_stats,
    });

    // Build tokio runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.threads)
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    crate::autodetect::log_host_info(config.server);

    rt.block_on(async {
        if let Some(secondary) = config.compare {
            // Compare mode (mono-NIC, uses config.server)
            let result = engine::compare::run_compare(config.clone(), secondary).await?;
            output::print_output(&result.primary, &config)?;
            engine::compare::print_compare(&result);
        } else if config.servers.len() > 1 {
            // Multi-NIC mode
            let snap = engine::run_multi_nic(config.clone()).await?;
            output::print_output(&snap, &config)?;
        } else {
            // Normal or ramp mode (legacy mono-NIC)
            let snap = engine::run(config.clone()).await?;
            output::print_output(&snap, &config)?;
        }
        anyhow::Ok(())
    })?;

    Ok(())
}
