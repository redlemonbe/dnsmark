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

`dnsperf` gives you an average QPS and a mean latency. That's not
enough to understand how a DNS server behaves under real load.

dnsmark gives you p50 / p95 / p99 / p999, a live TUI dashboard,
an automatic ramp mode that finds your server's saturation point,
and JSON output for CI/CD pipelines — all from a static binary
with no system dependencies.

| Feature | dnsperf | dnsmark |
|---------|:-------:|:-------:|
| HDR histogram (p50→p999) | ❌ | ✅ |
| Live TUI dashboard | ❌ | ✅ |
| Ramp mode (auto saturation) | ❌ | ✅ |
| Compare two servers | ❌ | ✅ |
| DNS-over-TLS | ❌ | ✅ |
| JSON / CSV output | ❌ | ✅ |
| jemalloc allocator | ❌ | ✅ |
| AF/XDP kernel-bypass | ❌ | ✅ opt-in |
| Static binary (musl, no deps) | ❌ | ✅ |
| dnsperf CLI compatibility | ✅ | ✅ |

---

## Benchmark results — Runbound v0.4.6

Measured with dnsmark v0.1.0 on a VM-to-VM setup
(AMD Threadripper PRO 5995WX → Dell T620, LAN):

| Scenario | QPS | p50 | p99 | Completion |
|---|---|---|---|---|
| Controlled 500 QPS | 439 | 9.1 ms | 10.0 ms | 99.97 % |
| Moderate 2 000 QPS | 1 568 | 5.1 ms | 6.0 ms | 99.98 % |
| High 8 000 QPS | 5 098 | 3.2 ms | 4.0 ms | 99.99 % |
| **Ramp — max sustainable** | **16 000** | 17.5 ms | 223 ms | — |

→ Full methodology: [docs/benchmark-report-v0.4.6.md](docs/benchmark-report-v0.4.6.md)

---

## Installation

```bash
# Static binary (no dependencies) — recommended
curl -LO https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-musl
chmod +x dnsmark-x86_64-linux-musl

# From source
cargo build --release

# With AF/XDP fast path (kernel 5.4+, CAP_NET_ADMIN)
cargo build --release --features xdp
```

---

## Usage

```bash
# Ramp mode — find your server's saturation point
dnsmark -s 192.168.1.10 --random --ramp

# Controlled load — 2 000 QPS, 30 s, JSON output
dnsmark -s 192.168.1.10 --random -Q 2000 -l 30 --json

# Query file (dnsperf format)
dnsmark -s 192.168.1.10 -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.168.1.10 --compare 192.168.1.11 --random -l 30

# DNS-over-TLS
dnsmark -s YOUR_DNS_SERVER --protocol dot --random -l 10

# Quiet mode — no TUI, final stats only
dnsmark -s 192.168.1.10 --random -l 30 -q

# CSV export
dnsmark -s 192.168.1.10 --random -l 30 --csv results.csv
```

---

## Flags

### dnsperf-compatible

| Flag | Default | Description |
|------|---------|-------------|
| `-s <IP>` | `127.0.0.1` | Target DNS server |
| `-p <PORT>` | `53` | Target port |
| `-d <FILE>` | — | Query file (`domain type` per line) |
| `-c <N>` | `num_cpus × 4` | Concurrent clients |
| `-Q <QPS>` | `0` (unlimited) | Max QPS cap |
| `-l <SEC>` | `30` | Test duration |
| `-t <MS>` | `3000` | Query timeout |
| `-T <N>` | `num_cpus` | Tokio worker threads |
| `-q` | — | Quiet — no TUI, final result only |
| `-v` | — | Verbose — log each query |
| `-S <SEC>` | `1` | Stats print interval |

### dnsmark extensions

| Flag | Description |
|------|-------------|
| `--ramp` | Start at 1 000 QPS, double every 5 s until saturation |
| `--random` | Infinite random UUID subdomain queries (no file needed) |
| `--random-domain <FQDN>` | Base domain for `--random` (default: `bench.invalid.`) |
| `--random-type a\|aaaa` | Record type for random queries (default: `a`) |
| `--compare <IP>` | Run parallel bench against two servers, diff output |
| `--protocol udp\|tcp\|dot` | Transport (default: `udp`) |
| `--json` | JSON output on stdout |
| `--csv <FILE>` | Write results to CSV |
| `--no-tui` | Disable live dashboard |
| `--xdp` | Force AF/XDP (needs `--features xdp`) |
| `--no-xdp` | Disable XDP |

---

## Query file format

Same as `dnsperf`: one entry per line, `domain type`:

```
google.com A
github.com AAAA
example.com MX
```

---

## Output

```
DNS Performance Testing Tool — dnsmark 0.1.0
[DISCLAIMER: authorized testing only]

Statistics:

  Queries sent:         152975
  Queries completed:    152958  (99.99%)
  Queries lost:             17  (0.01%)

  Response codes:
    NOERROR:             27425  (17.93%)
    NXDOMAIN:                0  (0.00%)
    SERVFAIL:                0  (0.00%)
    REFUSED:            125533  (82.07%)

  Average QPS:           5098
  Throughput:            5098 qps

  Latency:
    min:       2.019 ms
    avg:       3.160 ms
    p50:       3.245 ms
    p95:       3.823 ms
    p99:       4.019 ms
    p999:      8.975 ms
    max:      18.191 ms

  Run time: 30.004 s
```

---

## Licence

MIT — see [LICENSE](LICENSE)
