# dnsmark

High-performance DNS benchmark tool.  
Static binary. No dependencies. Runs anywhere.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)

> **Authorized testing only.**  
> Only use dnsmark against DNS servers you own or have explicit written authorization to test.  
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.

---

## Install

```bash
# x86_64
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 (Graviton, Raspberry Pi 4/5, …)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/
```

> Run dnsmark on a **separate machine** from the DNS server under test.

---

## Quick start

```bash
# Find max sustainable QPS (automatic saturation search)
dnsmark -s 192.0.2.1 --random --ramp

# Fixed load — 5 000 QPS for 60 s
dnsmark -s 192.0.2.1 --random -Q 5000 -l 60

# Query file (dnsperf format)
dnsmark -s 192.0.2.1 -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.0.2.1 --compare 192.0.2.2 --random -l 30

# DNS-over-TLS
dnsmark -s 192.0.2.1 --protocol dot --random -l 30

# JSON output (CI/CD)
dnsmark -s 192.0.2.1 --random -l 10 -q --json
```

---

## Ramp mode

```bash
dnsmark -s 192.0.2.1 --random --ramp
```

Starts at 1 000 QPS, doubles every 5 seconds, stops when the server can no longer keep up. Prints the last stable QPS.

```
Ramp: target QPS ->   2000  (burst: 171 017/s)
Ramp: target QPS ->   4000  (burst: 164 892/s)
...
Ramp: target QPS -> 256000  (burst: 153 058/s)

Max sustainable QPS: 128000
```

---

## Query file format

One entry per line — same format as dnsperf:

```
example.com A
example.com AAAA
mail.example.com MX
```

---

## Output

```
DNS Performance Testing Tool — dnsmark 1.0.0
[DISCLAIMER: authorized testing only]

Parameters:

  Server:       192.0.2.1:53
  Protocol:     UDP
  Clients:      10
  QPS cap:      unlimited
  Duration:     30 s
  Timeout:      3000 ms
  Mode:         fixed
  Source:       random (bench.invalid. A)

Statistics:

  Queries sent:         4 890 120
  Queries completed:    4 889 011     (99.98%)
  Queries lost:             1 109     (0.02%)

  Response codes:
    NOERROR:                    0     (0.00%)
    NXDOMAIN:           4 889 011     (100.00%)
    SERVFAIL:                   0     (0.00%)
    REFUSED:                    0     (0.00%)

  Average QPS:            163 000
  Throughput:             163 000 qps

  Latency:
    min:       0.148 ms
    avg:       1.361 ms
    p50:       1.292 ms
    p95:       2.443 ms
    p99:       3.193 ms
    p999:      3.999 ms
    max:       7.979 ms

  Run time: 30.001 s
```

> `--random` generates random UUID subdomain queries against `bench.invalid.` — NXDOMAIN is the expected response from a correct resolver. Use `-d queries.txt` to get NOERROR responses.

---

## Flags

### Core (dnsperf-compatible)

| Flag | Default | Description |
|------|---------|-------------|
| `-s <IP>` | `127.0.0.1` | Target DNS server |
| `-p <PORT>` | `53` | Target port |
| `-d <FILE>` | — | Query file (`domain type` per line) |
| `-c <N\|auto>` | `auto` | Workers (auto = physical cores, HT excluded) |
| `-Q <QPS>` | `0` (unlimited) | Max QPS cap |
| `-l <SEC>` | `30` | Test duration |
| `-t <MS>` | `3000` | Query timeout |
| `-q` | — | Quiet — no TUI, final result only |
| `-v` | — | Verbose — log each query |

### Extensions

| Flag | Default | Description |
|------|---------|-------------|
| `--ramp` | — | Auto ramp-up until saturation |
| `--random` | — | Infinite random UUID subdomain queries |
| `--random-domain <FQDN>` | `bench.invalid.` | Base domain for `--random` |
| `--random-type a\|aaaa` | `a` | Record type for `--random` |
| `--compare <IP>` | — | Parallel bench against a second server |
| `--protocol udp\|tcp\|dot` | `udp` | Transport protocol |
| `--json` | — | JSON output on stdout |
| `--csv <FILE>` | — | Write per-interval CSV |
| `--no-tui` | — | Disable live TUI dashboard |
| `--max-outstanding <N>` | `100` | Max in-flight queries across all workers |
| `--xdp` | — | Force AF/XDP (error if unavailable) |
| `--no-xdp` | — | Disable AF/XDP |
| `-S <SEC>` | `1` | Stats print interval |
| `-T <N>` | num_cpus | Tokio worker threads |

---

## Build from source

```bash
# Standard build (no XDP)
cargo build --release

# With AF/XDP support (requires clang + libbpf-dev at build time only)
apt install clang libbpf-dev
cargo build --release --features xdp
```

---

## Contributing

`cargo clippy --all-targets` — zero warnings  
`cargo test` — all tests must pass

---

## Support

[![Sponsor](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

**Bitcoin** — `3FP8hkkiu4kwCD1PDFgAv2oq1ZTyXwy3yy`  
**Ethereum** — `0xB5eEAf89edA4204Aa9305B068b37A93439cBb680`

Security issues: redlemonbe@codix.be (private disclosure before opening a public issue)

---

## License

MIT — see [LICENSE](LICENSE)

*dnsmark is a companion tool for [Runbound](https://github.com/redlemonbe/Runbound).*  
Copyright (C) 2026 RedLemonBe
