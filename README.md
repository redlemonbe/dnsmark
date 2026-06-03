# dnsmark

**The fastest DNS benchmark tool. Static binary. No dependencies. Runs anywhere.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

> **Authorized testing only.**  
> Only use dnsmark against DNS servers you own or have explicit written authorization to test.  
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.

---

## What you get

| | dnsperf | flamethrower | dnsmark |
|---|:---:|:---:|:---:|
| UDP / TCP | UDP only | UDP / TCP | ✅ UDP / TCP |
| DNS-over-TLS (DoT) | ❌ | ❌ | ✅ |
| Auto ramp (find max QPS) | ❌ | ❌ | ✅ `--ramp` |
| Compare two servers | ❌ | ❌ | ✅ `--compare` |
| Live TUI dashboard | ❌ | ❌ | ✅ |
| p50/p95/p99/p999 latency | basic | basic | ✅ full histogram |
| JSON output | ❌ | ❌ | ✅ `--json` |
| AF/XDP fast-path (optional) | ❌ | ❌ | ✅ |
| Static binary, no deps | ❌ requires libssl | ❌ | ✅ musl |

---

## Install

```bash
# x86_64 static (musl — no dependencies)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# x86_64 glibc (servers with glibc >= 2.17)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 static (Graviton, Raspberry Pi 4/5 — musl)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 glibc
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/
```

> Run dnsmark on a **separate machine** from the DNS server under test.

---

## Quick start

```bash
# Find max sustainable QPS (automatic ramp)
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

## AF_XDP zero-copy — 10G line-rate generation

With `--xdp`, dnsmark builds DNS query frames straight into the NIC's UMEM and
transmits them zero-copy (no kernel, no per-packet syscall). One independent
worker runs per NIC-local physical core; each owns its own queue, UMEM and rings
— no shared per-packet state. On an Intel X520 (82599) this **saturates a 10 GbE
link (~12 M qps)** and scales per core, ~30× a kernel-socket generator.

```bash
# grant capabilities once (or run as root)
sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip $(which dnsmark)

# zero-copy flood, all NIC-local cores
dnsmark -s 10.0.0.2 -d queries.txt --xdp -c 8 --max-outstanding 0
```

**Requirements to reach line rate (all matter):**

1. **Physical NIC only.** XDP binds a physical interface + queue. It **cannot**
   bind a virtual interface (bond, bridge/`vmbr*`, `veth`, `macvlan`) — dnsmark
   detects these and refuses / retries the physical parent.
2. **Disable flow control on the sender NIC.** Otherwise 802.3x PAUSE frames from
   the receiver throttle TX (we measured a hard ~1.36 M pps cap that vanished once
   flow control was off, jumping to 12 M):
   ```bash
   ethtool -A <nic> rx off tx off
   ```
3. **The server's MAC must be ARP-resolvable.** If dnsmark cannot resolve it, it
   logs a loud warning and **falls back to `sendmmsg`** (kernel path, not
   zero-copy). Pin it if needed:
   ```bash
   ip neigh replace 10.0.0.2 lladdr <server-mac> dev <nic> nud permanent
   ```

### Link bonding is not supported (XDP limitation)

AF_XDP cannot transmit over a Linux **bond** — a bond is a virtual interface, and
the kernel XDP layer binds a physical NIC + queue, so it has no way to spread
frames across bond members (the second member becomes a black hole).

**Workaround — saturate 2×10G with two independent paths:** take each port out of
the bond, give each its own subnet, and run **one dnsmark instance per physical
port**:

```bash
# port A
ethtool -A enp1s0f0 rx off tx off
dnsmark -s 10.0.0.2 -d queries.txt --xdp --max-outstanding 0   # uses enp1s0f0
# port B (second terminal / host)
ethtool -A enp1s0f1 rx off tx off
dnsmark -s 10.1.0.2 -d queries.txt --xdp --max-outstanding 0   # uses enp1s0f1
```

A native multi-NIC mode (one process driving several physical ports) is on the
roadmap — see the issues.

### Benchmarking a DNS server (spread across its cores)

dnsmark varies the UDP **source port** per packet by default so the server's NIC
RSS can spread the load across its RX queues/cores. For that to work the **server's
NIC must hash UDP on L4 ports** — most NICs default to hashing IPs only, which pins
the whole single-source flood to one queue → one core:

```bash
# on the SERVER under test
ethtool -N <nic> rx-flow-hash udp4 sdfn   # hash src/dst IP + src/dst port
ethtool -A <nic> rx off tx off            # no PAUSE-frame throttling
```

Measured impact: an Intel X520 resolver went from **1 core / 448k qps** to **16
cores / 4.77M qps** just by enabling `udp4 sdfn` + the per-packet source-port
variation (the 82599's RSS caps at 16 rings). Use `DNSMARK_FIXED_SPORT=1` to pin
the source port (single-flow / single-core testing).

> 📖 **Full 10G benchmarking methodology** — NIC tuning checklist, how to read NIC
> counters correctly (AF_XDP ZC TX bypasses standard `tx_packets`), CPU-bound vs.
> fill-ring bottleneck diagnosis, gotchas and a reference result (8.83 M qps on a
> 2013 dual-Xeon):  
> **[docs/benchmarking.md](docs/benchmarking.md)**

---

## Ramp mode

```bash
dnsmark -s 192.0.2.1 --random --ramp
```

Starts at 1 000 QPS, doubles every 5 seconds, stops when the server can no longer keep up.

```
Ramp: target QPS ->   2000  (burst: 171 017/s)
Ramp: target QPS ->   4000  (burst: 164 892/s)
...
Ramp: target QPS -> 256000  (burst: 153 058/s)

Max sustainable QPS: 128000
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
