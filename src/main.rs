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
    version = "0.1.0",
    about = "High-performance DNS benchmark — drop-in dnsperf replacement",
    after_help = DISCLAIMER,
)]
struct Cli {
    /// Target DNS server IP
    #[arg(short = 's', default_value = "127.0.0.1")]
    server: std::net::IpAddr,

    /// Target port
    #[arg(short = 'p', default_value_t = 53)]
    port: u16,

    /// Query file (dnsperf format: "domain type" per line)
    #[arg(short = 'd')]
    query_file: Option<PathBuf>,

    /// Concurrent clients (default: num_cpus * 4)
    #[arg(short = 'c')]
    concurrent: Option<usize>,

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

    /// Force AF_XDP (error if unsupported)
    #[arg(long)]
    xdp: bool,

    /// Disable AF_XDP even if available
    #[arg(long)]
    no_xdp: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // XDP check (feature guard)
    if cli.xdp {
        #[cfg(feature = "xdp")]
        {
            let auto = autodetect::detect();
            if !auto.xdp_available {
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

    // Auto-detection
    let auto = autodetect::detect();
    let protocol = Protocol::from_str(&cli.protocol)?;
    let concurrent = cli.concurrent.unwrap_or(auto.cpus * 4).max(1);
    let threads = cli.threads.unwrap_or(auto.cpus).max(1);

    let config = Arc::new(Config {
        server: cli.server,
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
    });

    // Build tokio runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.threads)
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    rt.block_on(async {
        if let Some(secondary) = config.compare {
            // Compare mode
            let result = engine::compare::run_compare(config.clone(), secondary).await?;
            output::print_output(&result.primary, &config)?;
            engine::compare::print_compare(&result);
        } else {
            // Normal or ramp mode
            let snap = engine::run(config.clone()).await?;
            output::print_output(&snap, &config)?;
        }
        anyhow::Ok(())
    })?;

    Ok(())
}
