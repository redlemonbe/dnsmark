# dnsmark

**High-performance DNS benchmark — drop-in `dnsperf` replacement.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GitHub release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)
[![cargo audit](https://img.shields.io/badge/cargo_audit-clean-brightgreen.svg)](https://github.com/redlemonbe/dnsmark)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

---

> **Designed to benchmark [Runbound](https://github.com/redlemonbe/Runbound)**
> — a hardened Rust DNS server. Works against any RFC 1035-compliant resolver.
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.

---

## Disclaimer

> **dnsmark is provided for authorized performance testing only.**
> The authors disclaim all liability for any unauthorized, abusive,
> or malicious use of this tool.
> Only use dnsmark against DNS servers you own or have explicit
> written authorization to test.

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
| AF/XDP kernel-bypass | ❌ | ✅ opt-in |
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
| **dnsmark 0.3.2** | **1** | **128 000** |

dnsmark auto-detects physical cores, pins each worker with CPU
affinity, and uses `sendmmsg()` batch sends — zero manual tuning
required.

→ Full benchmark methodology: [docs/benchmark-report-v0.4.6.md](docs/benchmark-report-v0.4.6.md)

---

## Installation

```bash
# Static binary — no dependencies, recommended
curl -LO https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-0.4.3-linux-x86_64-musl
chmod +x dnsmark-0.4.3-linux-x86_64-musl
sudo mv dnsmark-0.4.3-linux-x86_64-musl /usr/local/bin/dnsmark

# From source
cargo build --release

# With AF/XDP kernel-bypass (kernel 5.4+, CAP_NET_ADMIN)
cargo build --release --features xdp
```

## Quick start

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
| `--xdp` | — | Force AF/XDP (needs `--features xdp`) |
| `--no-xdp` | — | Disable XDP |

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

```
DNS Performance Testing Tool — dnsmark 0.4.3
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
| Global in-flight counter (`--max-outstanding`) | `Arc<AtomicUsize>` shared across all workers — no semaphore, no blocking |
| OOM guard | Background thread monitors `/proc/meminfo`, stops cleanly before the kernel OOM killer intervenes |
| HDR histogram | Lock-free, pre-allocated, zero allocation in hot path |

---

## Contributing

- `cargo clippy --all-targets` — zero warnings required
- `cargo test` — all tests must pass
- `make lint && make audit` before submitting

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
