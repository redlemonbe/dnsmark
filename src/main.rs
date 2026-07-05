pub mod simd;
mod autodetect;
mod governor;
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
    #[arg(short = 'c', long, default_value = "auto")]
    clients: String,

    /// Max QPS target (0 = unlimited)
    #[arg(short = 'Q', default_value_t = 0)]
    qps: u64,

    /// Test duration in seconds (default: 30; omit with no -Q to auto-run capacity discovery)
    #[arg(short = 'l')]
    duration: Option<u64>,

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

    /// Ramp mode (Dichotomic Saturation Discovery): exponential doubling from 100k QPS to the
    /// throughput ceiling, then bisection to the latency knee (auto-SLO). Default when no -Q/-l.
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

    /// Max outstanding queries PER WORKER (not a global cap), 0 = unlimited. Total in flight
    /// ≈ this × clients (-c). dnsperf's -q is a TOTAL cap, so to reproduce `dnsperf -q N`
    /// use `--max-outstanding N/clients`. Defaults by mode: steady --xdp ⇒ 0 (firehose), steady
    /// kernel ⇒ 100; --ramp --xdp ⇒ 0 (firehose), --ramp kernel ⇒ 32 (gated closed loop, so the
    /// DSD's latency SLO isn't fooled by firehose RX-buffering). Pass a value to override.
    #[arg(long)]
    max_outstanding: Option<usize>,

    /// Show per-NIC stats breakdown in multi-NIC mode
    #[arg(long)]
    nic_stats: bool,

    /// Reference latency mode: measure the WIRE round-trip via kernel SO_TIMESTAMPING
    /// (TX+RX stamps at the driver, raw-HW when available), excluding the generator's
    /// userspace/socket overhead. Serial ping-pong at -Q rate (default 5000), -l × rate
    /// samples. This is the honest server+network latency (§7), not a tool-inflated RTT.
    #[arg(long)]
    wire_latency: bool,
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
    // Auto-NUMA (single-NIC): confine CPUs+memory to the NIC's node so the user never
    // needs `numactl --cpunodebind=N --membind=N`. Multi-NIC NICs live on different
    // nodes → confining would starve one, so per-stack pinning handles that case.
    if servers.len() == 1 {
        transport::confine_to_nic_node(primary);
    }

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

    // Auto-ramp: if the user didn't request a specific duration (-l) or rate (-Q),
    // discover the server's capacity automatically (DSD). Explicit -l or -Q opts out.
    let auto_ramp = !cli.ramp && cli.qps == 0 && cli.duration.is_none()
        && !cli.random && cli.compare.is_none() && !cli.wire_latency;
    let ramp = cli.ramp || auto_ramp;
    let duration_secs = cli.duration.unwrap_or(30);
    if auto_ramp && !cli.quiet {
        eprintln!("No -Q or -l specified — discovering server capacity (DSD). Pass -l <secs> or -Q <qps> for a fixed-load test.");
    }

    let config = Arc::new(Config {
        server: primary,
        servers,
        port: cli.port,
        query_file: cli.query_file,
        concurrent,
        qps: cli.qps,
        duration_secs,
        timeout_ms: cli.timeout_ms,
        threads,
        quiet: cli.quiet,
        verbose: cli.verbose,
        stats_interval_secs: cli.stats_interval,
        ramp,
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
        // #16 auto-config. Ramp:
        //   - --xdp ⇒ firehose (0): the zero-copy RX drains losslessly, so latency stays honest
        //     at line rate and the p50 rises cleanly at the knee — the DSD finds it.
        //   - kernel-UDP (noxdp) ⇒ GATED closed loop (32/worker): an un-gated firehose floods
        //     the kernel RX path and inflates latency (a generator artifact — measured 2.6 ms
        //     vs a dnsperf closed loop's 0.12 ms at the same offered rate against unbound), so
        //     the latency-SLO DSD trips early and under-measures the knee. The gate bounds
        //     in-flight, so p50/p95/p99 match dnsperf and the DSD converges on the real knee.
        // Non-ramp: --xdp ⇒ 0 (firehose throughput), kernel-UDP ⇒ 100 (dnsperf-like).
        // An explicit --max-outstanding always wins.
        max_outstanding: if ramp {
            cli.max_outstanding.unwrap_or(if cli.xdp { 0 } else { 32 })
        } else {
            cli.max_outstanding.unwrap_or(if cli.xdp { 0 } else { 100 })
        },
        nic_stats: cli.nic_stats,
    });

    // Reference latency mode (synchronous probe; no async engine / no high-rate datapath).
    if cli.wire_latency {
        use crate::query::{file::FileQuerySource, random::RandomQuerySource, QuerySource};
        let qs: std::sync::Arc<dyn QuerySource> = if let Some(p) = &config.query_file {
            std::sync::Arc::new(FileQuerySource::load(p).context("load query file")?)
        } else {
            std::sync::Arc::new(RandomQuerySource::new(&config.random_domain, config.random_qtype))
        };
        let rate = if config.qps > 0 { config.qps } else { 5000 };
        // Honour -Q × -l (a floor of 200 keeps the percentiles meaningful); the probe itself is
        // wall-clock bounded, so a slow target can't turn a small request into a multi-minute run.
        let count = rate.saturating_mul(config.duration_secs).clamp(200, 2_000_000) as usize;
        let addr: std::net::SocketAddr = (config.server, config.port).into();
        println!("Wire-latency probe → {} : {} samples @ {} qps (kernel SO_TIMESTAMPING, TX+RX at the driver)",
            addr, count, rate);
        match transport::wire_latency::probe(addr, qs, count, rate, config.timeout_ms) {
            Ok(r) => {
                println!("  stamps:    {}", if r.hw { "raw hardware" } else { "software (driver-level)" });
                println!("  samples:   {}", r.samples);
                println!("  wire RTT (server + network, generator overhead excluded):");
                println!("    min:  {:.3} ms", r.min_us / 1000.0);
                println!("    p50:  {:.3} ms", r.p50_us / 1000.0);
                println!("    p95:  {:.3} ms", r.p95_us / 1000.0);
                println!("    p99:  {:.3} ms", r.p99_us / 1000.0);
                println!("    max:  {:.3} ms", r.max_us / 1000.0);
                return Ok(());
            }
            Err(e) => { eprintln!("wire-latency: {e}"); std::process::exit(1); }
        }
    }

    // Build tokio runtime
    // Pin CPU governor to performance for the whole run (restored on drop).
    let _gov = if cli.xdp { Some(governor::GovernorGuard::pin_performance()) } else { None };

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
