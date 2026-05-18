# dnsmark

High-performance DNS benchmark — drop-in `dnsperf` replacement.

**jemalloc · HDR histogram p999 · live TUI · ramp mode · DoT · compare mode**

## Disclaimer

> **dnsmark is provided for authorized performance testing only.**
> The authors disclaim all liability for any unauthorized, abusive, or malicious use of this tool.
> Only use dnsmark against DNS servers you own or have explicit written permission to test.

## Installation

```bash
# From source
cargo install --path .

# Static binary (musl)
cargo build --release --target x86_64-unknown-linux-musl
```

## Usage

### Basic benchmark (dnsperf-compatible)

```bash
# Benchmark with a query file
dnsmark -s 8.8.8.8 -d queries.txt -l 30

# Random queries, 10 000 QPS cap, 60s
dnsmark -s 192.168.1.10 --random -Q 10000 -l 60

# Quiet mode (no TUI, final stats only)
dnsmark -s 192.168.1.10 --random -l 30 -q

# JSON output
dnsmark -s 192.168.1.10 --random -l 10 --json

# CSV export
dnsmark -s 192.168.1.10 --random -l 30 --csv results.csv
```

### Ramp mode (auto-find max QPS)

```bash
dnsmark -s 192.168.1.10 --random --ramp
# Starts at 1 000 QPS, doubles every 5s until saturation
# Outputs: "Max sustainable QPS: XXXXX"
```

### Compare two servers

```bash
dnsmark -s 8.8.8.8 --compare 1.1.1.1 --random -l 30
```

### DNS-over-TLS

```bash
dnsmark -s 1.1.1.1 --protocol dot -l 10 --random
```

### TCP

```bash
dnsmark -s 192.168.1.10 --protocol tcp -d queries.txt -l 10
```

## Flags

### dnsperf-compatible (same letter)

| Flag | Default | Description |
|------|---------|-------------|
| `-s <IP>` | `127.0.0.1` | Target DNS server |
| `-p <PORT>` | `53` | Target port |
| `-d <FILE>` | — | Query file (format: `domain type` per line) |
| `-c <N>` | `num_cpus × 4` | Concurrent clients |
| `-Q <QPS>` | `0` (unlimited) | Max QPS target |
| `-l <SEC>` | `30` | Test duration |
| `-t <MS>` | `3000` | Query timeout |
| `-T <N>` | `num_cpus` | Tokio worker threads |
| `-q` | — | Quiet mode (no TUI) |
| `-v` | — | Verbose (log each query) |
| `-S <SEC>` | `1` | Stats interval |

### dnsmark extensions

| Flag | Description |
|------|-------------|
| `--ramp` | Auto ramp-up from 1 000 QPS until saturation |
| `--random` | Generate random UUID subdomain queries |
| `--random-domain <FQDN>` | Base domain for `--random` (default: `bench.invalid.`) |
| `--compare <IP>` | Run parallel bench against two servers |
| `--protocol udp\|tcp\|dot` | Transport (default: `udp`) |
| `--json` | JSON output on stdout |
| `--csv <FILE>` | Write CSV results |
| `--no-tui` | Disable live dashboard |
| `--xdp` | Force AF_XDP (needs `--features xdp`) |
| `--no-xdp` | Disable XDP |

## Query file format

Same as `dnsperf`: one query per line, `domain type`:

```
google.com A
github.com AAAA
example.com MX
```

## dnsperf vs dnsmark

| Feature | dnsperf | dnsmark |
|---------|---------|---------|
| HDR histogram (p999) | ✗ | ✓ |
| Live TUI dashboard | ✗ | ✓ |
| Ramp mode | ✗ | ✓ |
| Compare mode | ✗ | ✓ |
| DNS-over-TLS | ✗ | ✓ |
| JSON / CSV output | ✗ | ✓ |
| jemalloc allocator | ✗ | ✓ |
| AF/XDP path | ✗ | ✓ (opt-in) |
| Static binary (musl) | ✗ | ✓ |
| dnsperf CLI compat | ✓ | ✓ |

## Output example

```
DNS Performance Testing Tool — dnsmark 0.1.0
[DISCLAIMER: authorized testing only]

Statistics:

  Queries sent:         1500000
  Queries completed:    1498734    (99.92%)
  Queries lost:            1266    (0.08%)

  Response codes:
    NOERROR:            1450000    (96.75%)
    NXDOMAIN:             48734    (3.25%)
    SERVFAIL:                 0    (0.00%)
    REFUSED:                  0    (0.00%)

  Average QPS:          49957
  Throughput:           49957 qps

  Latency:
    min:       0.312 ms
    avg:       1.843 ms
    p50:       1.201 ms
    p95:       4.872 ms
    p99:      12.441 ms
    p999:     38.112 ms
    max:     102.881 ms

  Run time: 30.000 s
```

## Building with XDP

```bash
cargo build --release --features xdp
# Requires: kernel 5.4+, libbpf, CAP_NET_ADMIN
```

## Licence

MIT — see [LICENSE](LICENSE)
