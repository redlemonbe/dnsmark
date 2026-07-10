# dnsmark

**The DNS load generator that measures at the wire — and tells you when the wire is the wall.**

A single static binary that drives a DNS server from a few thousand to **line-rate
millions of queries per second**, reads throughput straight off the NIC hardware
counters, and refuses to quote a number it can't prove. Honest latency, an optional
zero-copy AF_XDP datapath, and a saturation-finding ramp — with every mechanism
written down in the [whitepaper](docs/WHITEPAPER.md), referenced to its source.

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

> **Authorized testing only.**
> Only use dnsmark against DNS servers you own or have explicit written authorization to test.
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.

---

## Why dnsmark

Most DNS benchmarks report a number and ask you to trust it. dnsmark reports a number
**and the evidence behind it** — read at the NIC, decomposed into its parts, and
labelled with what it does and does not mean.

- **Line-rate generation.** An optional zero-copy **AF_XDP** datapath builds DNS frames
  straight into the NIC's UMEM and transmits them with no kernel stack and no per-packet
  syscall. On a 10 GbE NIC it saturates the link — well beyond what a kernel-socket
  generator reaches — and scales per core.

- **Truth read at the wire.** Throughput is the receiver NIC's own hardware counter
  (`rx_packets + rx_missed_errors + rx_fifo_errors + rx_over_errors` — the ring-overflow
  terms recover replies the host never drained), reported by dnsmark itself as
  **`Server throughput (NIC rx)`**. No manual `ethtool -S` step. It counts the replies
  that physically left the server, so it stays comparable across tools and datapaths.

- **It tells you what the wall is.** From its own hardware observations, dnsmark divides
  that authoritative rate by the line-rate ceiling implied by the average on-wire reply
  size and the link speed, and prints a **`% of line rate`** with a verdict —
  **wire-bound** (≥ 90 %, the Ethernet link is the limit) or **link-headroom** (< 90 %,
  the server or the generator is). Self-contained, both AF_XDP and kernel-UDP; emitted for
  fixed/flood runs (in `--ramp`, the DSD Capacity summary is the throughput answer).

- **Honest latency, or none at all.** Timeouts and end-of-run in-flight queries are
  counted as **losses** (`sent == completed + lost`, dnsperf-compatible), never as latency
  samples — the histogram holds real responses only, so the tail is never padded with
  fabricated RTTs. And in a pure firehose, dnsmark reports `0.000 ms` rather than a
  meaningless per-batch figure. Absolute latency is validated against a **wire capture**,
  not asserted.

- **Finds the knee, not just an octave.** `--ramp` runs **Dichotomic Saturation Discovery**:
  it doubles QPS to bracket the maximum, then binary-searches inside the bracket to within
  5 % — so it reports the *real* saturation point, not the nearest power of two.

- **Deterministic placement.** Workers pin to NIC-local **physical** cores (HT excluded, no
  real-time scheduling) for repeatable results, and confine themselves to the NIC's NUMA
  node automatically — no `numactl`, no tuning ritual.

The trade-offs and the exact measurement methodology are written down, not glossed over —
see the **[whitepaper](docs/WHITEPAPER.md)**.

---

## Measured — server ceilings (2026-07-03)

Four resolvers in strict parity (forward + cache, DNSSEC off, 100k-domain corpus warmed to
all-cache-hit), single 10 GbE Intel X710 (i40e), driven by four generators.
**Truth = receiver NIC tx_packets ÷ 20 s** — the replies the server actually put on the
wire, the one measure that makes cross-tool numbers comparable. Peak qps over all four
generators:

| Server | Peak (NIC tx) | vs BIND9 |
|---|---:|---:|
| Runbound (xdp:yes) | **12.5 M** | ~6.6× |
| Runbound (xdp:no)  | 6.8 M | ~3.6× |
| Unbound 1.22       | 3.1 M | ~1.6× |
| BIND9 9.x          | 1.9 M | 1× |

Runbound's AF_XDP fast path roughly doubles its own kernel-path ceiling (~6.8 M → ~12.5 M).

> **Read this before quoting a headline.** The number a benchmark reports depends on the
> *generator's* load discipline as much as on the server. Open-loop AF_XDP firehoses
> (dnsmark `--xdp`, kxdpgun) push a robust server hardest — but BIND9 **collapses** under the
> 13 M XDP firehose (872 k / 1.03 M qps) versus 1.89 M under the gentler kernel-UDP load:
> classic receiver livelock. The receiver-NIC-tx counter captures the truth in every case,
> which is exactly why it is the reference here.

Caveats, honestly: single 20 s runs (±10–15 % run-to-run, not averaged). The 12.5 M figure
exceeds the ~9.85 M/s line rate for 103 B replies because this warm produced smaller average
replies (line rate is reply-size dependent). Full method, per-generator table, and analysis:
**[docs/benchmarking.md](docs/benchmarking.md)**.

---

## What dnsmark adds over dnsperf

The canonical closed-loop DNS load tool is **dnsperf** (DNS-OARC). dnsmark speaks the same
UDP/TCP/DoT and is designed to be directly comparable to it — this table is what dnsmark
adds on top.

| | dnsperf | dnsmark |
|---|:---:|:---:|
| UDP / TCP / DoT | ✅ | ✅ |
| Built-in auto-ramp to max QPS | separate companion tool | ✅ `--ramp` (Dichotomic Saturation Discovery) |
| Side-by-side two-server compare | ❌ | ✅ `--compare` |
| Live TUI dashboard | ❌ | ✅ |
| p50 / p95 / p99 / p999 | average + per-query `-v` | ✅ built-in HDR histogram |
| Wire-bound vs. server-bound verdict | ❌ | ✅ line-rate awareness (`% of line rate`) |
| Throughput read at the NIC hardware counter | ❌ | ✅ `Server throughput (NIC rx)` |
| JSON output | ❌ | ✅ `--json` |
| AF_XDP zero-copy datapath | ❌ | ✅ `--xdp` |
| Single static binary, no deps | ❌ | ✅ musl |

> **kxdpgun** (the AF_XDP DNS generator shipped with Knot DNS) also does zero-copy XDP and
> JSON — it is the open-loop peer dnsmark is benchmarked against separately in
> [docs/dnsmark-vs-kxdpgun.md](docs/dnsmark-vs-kxdpgun.md). The table above is versus the
> mainstream closed-loop tool.

> **Latency honesty.** A closed-loop generator's absolute latency is never "the server's
> latency" — it includes the tool's own client-side overhead. Anchor on a wire capture; see
> [Measurement accuracy](#measurement-accuracy--transport) and the
> [whitepaper](docs/WHITEPAPER.md).

---

## Install

```bash
# x86_64 static (musl — no dependencies)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-unknown-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# x86_64 glibc (servers with glibc >= 2.17)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-unknown-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 static (Graviton, Raspberry Pi 4/5 — musl)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-unknown-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 glibc
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-unknown-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/
```

> Run dnsmark on a **separate machine** from the DNS server under test.

---

## Quick start

```bash
# Find max sustainable QPS (auto-ramp — no -Q or -l needed, builtin corpus)
dnsmark -s 192.0.2.1

# Fixed load — 5 000 QPS for 60 s
dnsmark -s 192.0.2.1 -Q 5000 -l 60

# Query file (one `name [type]` per line; omit -d to use the builtin 2000-domain corpus)
dnsmark -s 192.0.2.1 -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.0.2.1 --compare 192.0.2.2 -l 30

# DNS-over-TLS
dnsmark -s 192.0.2.1 --protocol dot -l 30

# JSON output (CI/CD — the complete, stable interface)
dnsmark -s 192.0.2.1 -l 10 -q --json
```

---

## AF_XDP zero-copy — 10G line-rate generation

With `--xdp`, dnsmark builds DNS query frames straight into the NIC's UMEM and transmits
them zero-copy (no kernel, no per-packet syscall). One independent worker runs per NIC-local
physical core; each owns its own queue, UMEM and rings — no shared per-packet state. On a
10 GbE NIC this **saturates the link** and scales per core, well beyond a kernel-socket
generator (see the [benchmarking guide](docs/benchmarking.md)).

> **`--xdp` is opt-in.** The default transport is the UDP kernel socket (a standard kernel
> UDP client). Use `--xdp` only against a server that is *itself* AF_XDP (symmetric
> measurement) or for raw saturation throughput — never an XDP generator against a kernel
> server. For latency comparisons against unbound/BIND, use the default UDP path.

```bash
# grant capabilities once (or run as root)
sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip $(which dnsmark)

# zero-copy flood, all NIC-local cores
# --xdp defaults to firehose (--max-outstanding 0) and auto-confines to the NIC's NUMA
# node — no numactl, no DNSMARK_SPORT_SPREAD, no ethtool tuning needed.
dnsmark -s 10.0.0.2 -d queries.txt --xdp
```

**Requirements to reach line rate (all matter):**

1. **Physical NIC only.** XDP binds a physical interface + queue. It **cannot** bind a
   virtual interface (bond, bridge/`vmbr*`, `veth`, `macvlan`) — dnsmark detects these and
   retries the physical parent, or falls back to the kernel path.
2. **Disable flow control on the sender NIC.** Otherwise 802.3x PAUSE frames from the
   receiver throttle TX to a hard cap that vanishes once flow control is off:
   ```bash
   ethtool -A <nic> rx off tx off
   ```
3. **The server's MAC must be ARP-resolvable.** If dnsmark cannot resolve it, it logs a loud
   warning and **falls back to `sendmmsg`** (kernel path, not zero-copy). Pin it if needed:
   ```bash
   ip neigh replace 10.0.0.2 lladdr <server-mac> dev <nic> nud permanent
   ```

**CPU governor.** With `--xdp`, dnsmark pins every CPU to the `performance` governor for the
run and restores the previous governor on exit, including Ctrl-C. After a hard kill or a
crash (the binary is built with `panic = "abort"`) the restore cannot run — if a crashed run
left cores pinned, reset them:
`echo schedutil | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor`.

### Native multi-NIC — saturate 2×10G from one process

Repeat `-s`, one target per physical port (each on its own subnet routed via its own NIC). A
single dnsmark instance drives one AF_XDP stack per card; each stack pins to **its own NIC's
NUMA node**, so two cards on separate PCIe buses / sockets scale independently:

```bash
# one instance, two cards (X710 on node 1 + X520 on node 0) — no numactl, no per-NIC tuning
dnsmark -s 10.71.10.1 -s 10.51.10.1 -d queries.txt --xdp --nic-stats
```

Each target must route via a distinct NIC (dnsmark refuses two targets on the same
interface). Link **bonding** is unsupported — a bond is a virtual interface, and XDP binds a
physical NIC + queue, so use the per-port targets above.

### Tagged VLAN (experimental)

`DNSMARK_VLAN=<vid>` injects one 802.1Q tag into the generated frames (needed when the
provider delivers the private network tagged). AF_XDP zero-copy cannot bind a VLAN
sub-interface, so dnsmark binds the **physical parent** and bakes the tag into the frame
template. **Experimental**: the frame layout is unit-tested and a tagged resolver round trip
is proven end-to-end, but tagged generation has not been validated at line rate (no
zero-copy-capable tagged NIC was available).

### Spreading load across the server's cores

dnsmark fans load across the server's RX queues through **source-port diversity**: on the
default path each `-c` worker owns a socket with its own source port; with `--xdp` each worker
*cycles* its UDP source port over a wide range (`10000 + (counter mod 2048)`, with a
per-worker phase offset), producing thousands of distinct flows across the server's RSS. For
this to reach all queues, the **server's NIC must hash UDP on L4 ports** (most default to IPs
only, pinning the whole flood to one queue → one core):

```bash
# on the SERVER under test
ethtool -N <nic> rx-flow-hash udp4 sdfn   # hash src/dst IP + src/dst port
ethtool -A <nic> rx off tx off            # no PAUSE-frame throttling
```

For a single-flow / single-core test, run a single worker (`-c 1`).

> 📖 **Full 10G benchmarking methodology** — NIC tuning checklist, how to read NIC counters
> correctly (AF_XDP ZC TX bypasses standard `tx_packets`), CPU-bound vs. fill-ring diagnosis,
> and gotchas: **[docs/benchmarking.md](docs/benchmarking.md)**

---

## Ramp mode — Dichotomic Saturation Discovery

```bash
# auto-DSD (default when no -Q or -l)
dnsmark -s 192.0.2.1

# explicit ramp with a custom corpus
dnsmark -s 192.0.2.1 -d top-10000-domains.txt --xdp --ramp
```

`--ramp` doesn't just climb and stop — it runs a two-phase **Dichotomic Saturation Discovery
(DSD)** to pinpoint the *exact* saturation knee, which a plain doubling ramp cannot:

1. **Logarithmic discovery** — start at 100k QPS and double every step until the median (p50)
   round-trip latency breaks the SLO (auto-computed as `max(3 × floor, floor + 1 ms)`, floor
   = the minimum p50 observed at low load). This brackets the maximum in `log₂` steps.
2. **Dichotomic convergence** — binary-search inside that bracket (test the midpoint, move the
   edge toward it, repeat) until within 5 %, converging on the real knee instead of falling
   back to a power of two.

The **median (p50)** is the saturation signal, not p95/p99: a small fraction of forwarded
cache-misses produces large tail outliers that belong to the *workload*, not to server
saturation, and would trip a tail-based test prematurely. Each step measures its own window
(the histogram is reset per step).

At convergence it prints the idle-latency floor, the NIC-verified Capacity, the Within-SLO
rate, and the DSD knee bracket:

```
  Idle latency:  <floor> ms   (floor — minimum p50 observed)
  Capacity:      <max>        (NIC-verified — max replies/s on the wire)
  Within SLO:    <rate>       (p50 stays under the SLO at this rate)
Knee bracket (DSD bisection): [<low> ; <high>] q/s  (±%)
```

**What "Capacity" means depends on the datapath — read the label.** The `--xdp` ramp is an
**open-loop firehose with a lossless zero-copy RX**, so its Capacity genuinely *is* the max
replies/s on the wire (labelled *NIC-verified*). The kernel-UDP ramp is a **gated closed
loop** (dnsperf-comparable, latency-honest): the generator's own kernel receive path drops
replies under load, capping the *offered* rate below the server's real ceiling. So the
kernel-UDP DSD figure is the **closed-loop SLO knee, not the server's raw max**, and is
labelled accordingly, with a pointer to the open-loop command for the raw ceiling:

```
  Capacity:      <knee>  (closed-loop knee — kernel-recv bound, NOT the server's raw max)
  -> for the raw ceiling, run open-loop:  dnsmark -s <ip> -Q 0 --max-outstanding 0
     (reports "Server throughput (NIC rx)" = server_rx_qps, the authoritative reply rate)
```

See **[WHITEPAPER.md §5b](docs/WHITEPAPER.md)** for the full algorithm.

---

## Measurement accuracy & transport

dnsmark sends over a **UDP kernel socket by default** — the same datapath as a standard kernel
UDP client — so latency is directly comparable to dnsperf. `--xdp` is **opt-in** and changes
the datapath: use it only when the server under test is itself AF_XDP (a symmetric
**XDP-vs-XDP** measurement) or for raw saturation. **Never compare an XDP generator against a
kernel server.** The rule is simple: *the generator's datapath must match the server's.*

A closed-loop generator's reported RTT is **`server processing + network + the tool's own
client-side overhead`** — only the first term belongs to the server, and the third differs
between any two generators. So **absolute** latency is a property of the *(server, generator,
rig)* triple, not of the server alone: validate the server's term against the **wire** (a
`tcpdump` capture, paired by DNS transaction id) and compare servers with **one fixed
generator on one rig**. dnsmark's client path is deliberately light (batched `recvmmsg`, a
tight send/poll loop), so its figure sits close to the wire — but the **wire**, not any tool,
is the reference.

See **[docs/benchmarking.md §7](docs/benchmarking.md)** for the full three-term decomposition
and the exact commands to reproduce it.

> **Concurrency (`-c`).** Each client is a dedicated OS thread. `-c auto` (the default) uses
> one thread per physical core (HT excluded), with a **floor of 8** — so on a 2-core VM `auto`
> is still 8 workers, on purpose, to keep small hosts busy. Setting `-c` far above the
> physical core count oversubscribes the CPU and adds scheduling jitter; raise it only when the
> bottleneck is clearly elsewhere.

---

## Output

Every run produces the same underlying metrics through four sinks: a live **TUI**, **JSON**
(`--json`), **CSV** (`--csv`), and plain text. **JSON is the complete, stable interface** and
the recommended one for automation — the TUI and CSV are focused live/tabular views that
surface the headline counters and percentiles. A real `--xdp` firehose run against a warmed
resolver on a single 10 GbE X710 (latency reads `0.000 ms` — an open-loop firehose does not
sample RTT; use a fixed-load or `--ramp` run for latency):

```
DNS Performance Testing Tool — dnsmark 1.0.0
[DISCLAIMER: authorized testing only]

Parameters:

  Server:       192.0.2.1:53
  Protocol:     UDP
  Clients:      20
  QPS cap:      13000000
  Duration:     12 s
  Timeout:      3000 ms
  Mode:         fixed
  Source:       file (corpus-100k.txt)

Statistics:

  Queries sent:         145251413
  Queries completed:    118209415   (81.38%)
  Queries lost:         27041998    (18.62%)

  Response codes:
    NOERROR:            118190692   (99.98%)
    NXDOMAIN:           13055       (0.01%)
    SERVFAIL:           5668        (0.00%)
    REFUSED:            0           (0.00%)

  Send throughput (egress):  12099464 qps  ← match this to rx_packets on receiver NIC
  Wire egress (NIC PHY):     12098517 qps  (confirmed transmitted)
  Round-trip completed:      9846874 qps  (81.4% of egress, userspace)
  Server throughput (NIC rx):  9850308 qps  (authoritative — replies on the wire)
  Line rate:                 100% of 10 Gb/s wire  (103 B replies, ceiling 9.85 M/s)
    → WIRE-BOUND: the Ethernet link is saturated — this IS the max for this
      reply size on this link. Use faster/more NICs (25/40/100G) to push higher.

  Latency:
    min:       0.000 ms
    avg:       0.000 ms
    p50:       0.000 ms
    p95:       0.000 ms
    p99:       0.000 ms
    p999:      0.000 ms
    max:       0.000 ms

  Run time: 12.005 s
```

---

## Documentation

- **[Whitepaper](docs/WHITEPAPER.md)** — architecture, the worker loop, how latency is measured, and the generator effect (§7a).
- **[Benchmarking methodology](docs/benchmarking.md)** — throughput at NIC counters + the wire-validated latency decomposition; the 2026-07-03 four-server reference result.
- **[Cross-validation vs dnsperf](docs/cross-validation-dnsperf.md)** — dnsmark vs dnsperf across BIND9, Unbound, Runbound.
- **[dnsmark vs kxdpgun](docs/dnsmark-vs-kxdpgun.md)** — the two open-loop AF_XDP generators, measured at the receiver NIC.
- **[unbound vs BIND9 methodology](docs/server-comparison-methodology.md)** — the closed-loop DSD / latency comparison method.
- **[Empirical validation](docs/empirical-validation.md)** — how dnsmark's figures are cross-validated.
- **[Engineering notes](docs/FINDINGS.md)** — datapath findings and known limitations.

---

## Support the project

[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor%20on%20GitHub)](https://github.com/sponsors/redlemonbe)

**Bitcoin** — `3FP8hkkiu4kwCD1PDFgAv2oq1ZTyXwy3yy`
**Ethereum** — `0xB5eEAf89edA4204Aa9305B068b37A93439cBb680`

---

## License

AGPL v3 — see [LICENSE](LICENSE).

---

## Part of RunASM

dnsmark is part of **[RunASM](https://www.runasm.com)** — high-performance infrastructure in Rust, with benchmarks measured at the NIC hardware counters, not asserted.
