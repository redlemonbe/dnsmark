# dnsmark

**High-performance DNS benchmark — drop-in `dnsperf` replacement.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GitHub release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)
[![cargo audit](https://img.shields.io/badge/cargo_audit-clean-brightgreen.svg)](https://github.com/redlemonbe/dnsmark)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

---

> **Works against any RFC 1035-compliant resolver.**
> Designed as a drop-in replacement for dnsperf with production-grade diagnostics.
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.
> **Authorized testing only** — only use dnsmark against DNS servers you own or have explicit written authorization to test.

---

## Why dnsmark

`dnsperf` reports an average QPS and a mean latency. In production,
averages lie — a p999 spike at 1 second tells you more about your
DNS server than any mean ever will.

dnsmark gives you the full picture: **p50 / p95 / p99 / p999**,
a **live TUI dashboard**, a **ramp mode** that automatically finds
your server's saturation point, and **JSON output** for CI/CD — all
from a single static binary with zero system dependencies.

And it scales: one instance of dnsmark uses **all physical CPU cores
automatically**, pinned with CPU affinity, HyperThreading excluded.
On a 32-core Threadripper, one `dnsmark` instance equals **8 parallel
`dnsperf` instances**.

| Feature | dnsperf | dnsmark |
|---|:---:|:---:|
| HDR histogram (p50 → p999) | ❌ | ✅ |
| Live TUI dashboard | ❌ | ✅ |
| Ramp mode (auto saturation) | ❌ | ✅ |
| Compare two servers | ❌ | ✅ |
| DNS-over-TLS | ❌ | ✅ |
| JSON / CSV output | ❌ | ✅ |
| CPU affinity (physical cores, HT excluded) | ❌ | ✅ |
| Auto-scale to machine size | ❌ | ✅ |
| OOM protection | ❌ | ✅ |
| jemalloc allocator | ❌ | ✅ |
| sendmmsg() batch sending | ❌ | ✅ |
| AF/XDP kernel-bypass | ❌ | ✅ default |
| Static binary (musl, no deps) | ❌ | ✅ |
| dnsperf CLI compatibility | ✅ | ✅ |

---

## One instance. Same result as 8 × dnsperf.

Measured on AMD Threadripper PRO 5995WX (32 physical cores),
loopback, same DNS server:

| Tool | Instances | QPS |
|---|:---:|---|
| dnsperf 2.14 | 1 | 87 000 |
| dnsperf 2.14 | 8 | 127 000 |
| **dnsmark 0.4.4** | **1** | **128 000** |

dnsmark auto-detects physical cores, pins each worker with CPU
affinity, and uses `sendmmsg()` batch sends — zero manual tuning
required.

→ Full benchmark methodology: [docs/benchmark-dnsperf-vs-dnsmark.md](docs/benchmark-dnsperf-vs-dnsmark.md)

---

## Installation

```bash
# Static binary — no dependencies, recommended
# AF/XDP included by default. Replace linux-x86_64-musl with
# linux-aarch64-musl for ARM64, or *-linux-gnu for glibc builds.
curl -LO https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-linux-x86_64-musl
chmod +x dnsmark-linux-x86_64-musl
sudo mv dnsmark-linux-x86_64-musl /usr/local/bin/dnsmark

# Optional: grant XDP capabilities so non-root users can use the fast path
sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip /usr/local/bin/dnsmark

# From source (AF/XDP included — requires clang + libbpf-dev at build time)
apt install clang libbpf-dev   # build deps only, not required at runtime
cargo build --release
```

## Quick start

> Run dnsmark from a dedicated machine — never from the DNS server you are testing.

```bash
# Auto-find your server's max sustainable QPS
dnsmark -s YOUR_DNS_SERVER --random --ramp

# Controlled load — 5 000 QPS for 60 s
dnsmark -s YOUR_DNS_SERVER --random -Q 5000 -l 60

# Query file (dnsperf format) — drop-in replacement
dnsmark -s YOUR_DNS_SERVER -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.168.1.10 --compare 192.168.1.11 --random -l 30

# JSON output for CI/CD
dnsmark -s YOUR_DNS_SERVER --random -l 10 -q --json

# DNS-over-TLS
dnsmark -s YOUR_DNS_SERVER --protocol dot --random -l 30
```

## Ramp mode

```bash
dnsmark -s YOUR_DNS_SERVER --random --ramp
```

Starts at 1 000 QPS. Every 5 seconds, measures the actual burst
throughput and doubles the target. Stops when the measured burst
can no longer reach the next target. Reports the last stable QPS.

```
Ramp: target QPS ->   2000  (burst: 137124/s)
Ramp: target QPS ->   4000  (burst: 136577/s)
Ramp: target QPS ->   8000  (burst: 146048/s)
...
Ramp: target QPS -> 256000  (burst: 143766/s)

Max sustainable QPS: 128000  (burst 143766/s < 256000/s target)
```

No manual tuning. No guessing. One command, one answer.

---

## Common use cases

**Find max capacity before a deployment**

```bash
dnsmark -s YOUR_DNS_SERVER --random --ramp
```

Automatically doubles QPS every 5 s and reports the last stable throughput. No manual iteration.

**Regression test between two server versions**

```bash
dnsmark -s OLD_SERVER --compare NEW_SERVER --random -l 60
```

Runs both in parallel, prints a side-by-side diff of QPS, latency, and completion rate.

**CI/CD gate — fail if p99 exceeds a threshold**

```bash
result=$(dnsmark -s YOUR_DNS_SERVER --random -l 30 -q --json)
p99=$(echo "$result" | python3 -c "import sys,json; s=json.load(sys.stdin)['statistics']; print(s['p99_us'])")
[ "$p99" -lt 50000 ] || { echo "p99 exceeded 50 ms"; exit 1; }
```

JSON output makes it trivial to parse any metric in a pipeline.

**Reproduce a production incident**

```bash
dnsmark -s YOUR_DNS_SERVER -d incident-queries.txt -Q 8000 -l 300
```

Replay the exact query mix, rate, and duration — with full percentile output that `dnsperf` cannot provide.

---

## Flags

### dnsperf-compatible (same letter)

| Flag | Default | Description |
|---|---|---|
| `-s <IP>` | `127.0.0.1` | Target DNS server |
| `-p <PORT>` | `53` | Target port |
| `-d <FILE>` | — | Query file (domain type per line) |
| `-c <N\|auto>` | `auto` | Workers (auto = physical cores, HT excluded) |
| `-Q <QPS>` | `0` unlimited | Max QPS cap |
| `-l <SEC>` | `30` | Test duration |
| `-t <MS>` | `3000` | Query timeout |
| `-T <N>` | num_cpus | Tokio worker threads |
| `-q` | — | Quiet — no TUI, final result only |
| `-v` | — | Verbose — log each query |
| `-S <SEC>` | `1` | Stats print interval |

### dnsmark extensions

| Flag | Default | Description |
|---|---|---|
| `--max-outstanding <N>` | `100` | Max queries in-flight globally (mirrors dnsperf -q) |
| `--ramp` | — | Auto ramp-up until saturation |
| `--random` | — | Infinite random UUID subdomain queries |
| `--random-domain <FQDN>` | `bench.invalid.` | Base domain for `--random` |
| `--random-type a\|aaaa` | `a` | Record type for random queries |
| `--compare <IP>` | — | Parallel bench against two servers, diff output |
| `--protocol udp\|tcp\|dot` | `udp` | Transport protocol |
| `--json` | — | JSON output on stdout |
| `--csv <FILE>` | — | Write per-interval CSV |
| `--no-tui` | — | Disable live dashboard |
| `--xdp` | — | Force AF/XDP (error if unavailable) |
| `--no-xdp` | — | Disable AF/XDP, use recvmmsg UDP path |

---

## Query file format

Same as dnsperf: one entry per line, `domain type`:

```
google.com A
github.com AAAA
example.com MX
_smtp._tcp.example.com SRV
```

---

## Output

> Example: `--random` mode against a recursive resolver. NXDOMAIN is expected —
> random UUID subdomains of `bench.invalid.` have no delegation, so any correct
> resolver returns NXDOMAIN. Use a query file (`-d queries.txt`) to get NOERROR responses.

```
DNS Performance Testing Tool — dnsmark 0.4.4
[DISCLAIMER: authorized testing only]

Parameters:

  Server:       192.168.1.10:53
  Protocol:     UDP
  Clients:      32 (auto — physical cores, HT excluded)
  QPS cap:      unlimited
  Duration:     30 s
  Timeout:      3000 ms
  Mode:         fixed
  Source:       random (bench.invalid. A)

Statistics:

  Queries sent:         1 188 032
  Queries completed:    1 184 847     (99.73%)
  Queries lost:             3 185     (0.27%)

  Response codes:
    NOERROR:                    0     (0.00%)
    NXDOMAIN:           1 184 847     (100.00%)
    SERVFAIL:                   0     (0.00%)
    REFUSED:                    0     (0.00%)

  Average QPS:             78 955
  Throughput:              78 955 qps

  Latency:
    min:       1.632 ms
    avg:      40.068 ms
    p50:      36.095 ms
    p95:      73.471 ms
    p99:      93.567 ms
    p999:    148.223 ms
    max:     184.063 ms

  Run time: 15.007 s
```

---

## Architecture

| Component | Detail |
|---|---|
| jemalloc global allocator | Lower fragmentation under sustained load |
| SO_REUSEPORT | One UDP socket per worker, zero lock contention |
| CPU affinity | Each worker pinned to a physical core, HT excluded |
| sendmmsg() | Batch UDP sends, fewer syscalls per second |
| AF/XDP receive path | DNS responses captured at NIC driver level via eBPF — zero kernel network-stack overhead on the RX hot path. One shared XDP receiver thread per NIC queue; N senders continue using regular UDP sockets. Automatic fallback to recvmmsg on unsupported hardware. |
| Global in-flight counter (`--max-outstanding`) | `Arc<AtomicUsize>` shared across all workers — no semaphore, no blocking |
| OOM guard | Background thread monitors `/proc/meminfo`, stops cleanly before the kernel OOM killer intervenes |
| HDR histogram | Lock-free, pre-allocated, zero allocation in hot path |

---

## Hardware requirements

dnsmark scales horizontally — one worker per physical core.
Below 4 physical cores, `dnsperf` will produce cleaner results
with less overhead. Above 4 cores, dnsmark's advantage compounds
with each additional core.

| Tier | CPU | RAM | Expected max QPS |
|------|-----|-----|-----------------|
| Minimum | 4 physical cores | 2 GB | ~30 000 |
| Recommended | 8+ physical cores | 4 GB | ~60 000 |
| Optimal | 16+ physical cores | 8 GB | 100 000+ |

> Numbers measured UDP, loopback, against a local resolver.
> Real-world LAN results depend on network and target server.

**Important:** always run dnsmark on a **dedicated machine**,
separate from the DNS server under test. Running both on the
same host invalidates results (CPU contention).

**AF/XDP mode** (enabled by default) additionally requires:
kernel 5.10+, `CAP_NET_RAW + CAP_NET_ADMIN + CAP_BPF`, and a NIC with
XDP driver support (Intel ixgbe / i40e / ice / igc — native zero-copy;
virtio / veth — copy mode). Without the required capabilities, dnsmark
prints a hint and falls back to the recvmmsg UDP path automatically.

**Architecture:** x86_64 and aarch64.

---

## Contributing

- `cargo clippy --all-targets --features xdp` — zero warnings required
- `cargo test` — all tests must pass
- `make lint && make audit` before submitting
- Build deps for XDP: `apt install clang libbpf-dev`

---

## Support the project

[![Sponsor](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

**Bitcoin** — `3FP8hkkiu4kwCD1PDFgAv2oq1ZTyXwy3yy`  
**Ethereum** — `0xB5eEAf89edA4204Aa9305B068b37A93439cBb680`

---

## Contact

redlemonbe@codix.be

Security issues: report privately by email before opening a public issue.

---

## License

MIT — see [LICENSE](LICENSE)

---

*dnsmark is a companion tool for [Runbound](https://github.com/redlemonbe/Runbound).*

Copyright (C) 2026 RedLemonBe
