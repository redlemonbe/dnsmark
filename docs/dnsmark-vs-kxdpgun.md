# dnsmark vs kxdpgun — two AF_XDP DNS generators, one server, measured at the NIC

Two open-loop AF_XDP DNS traffic generators — **kxdpgun** (CZ.NIC, shipped with Knot DNS)
and **dnsmark** (RunASM) — drive the same server over the same link, under the
[Runbound benchmark methodology](https://github.com/redlemonbe/Runbound/blob/main/docs/benchmark/README.md).
The point is not which floods faster (at 10 GbE both saturate the wire); it is what each
tool *reports*, and how far that is from what the receiver's NIC actually counted.

## Setup (identical for both, per methodology)

| | |
|---|---|
| Server | Runbound, `xdp: yes` (AF_XDP fast path), Intel X710 / i40e, single 10 GbE port, 64 combined RX queues (= cores; the i40e accepts up to 128 on this port), warm cache |
| Server host | AMD Threadripper PRO 5995WX (64c/128t), governor `performance`, other VMs stopped |
| Generator host | dual Intel Xeon E5-2690 v2 (20c/40t) |
| Link | direct DAC 10 GbE, flow-control off both ends, RSS `udp4 sdfn`, 10.71.10.2 → 10.71.10.1 |
| Workload | `top-10000-domains.txt`, 10 000 real names, random order, cache warmed (forward-zone `.` → 1.1.1.1 / 8.8.8.8 / 9.9.9.9), no local data |
| Truth | receiver NIC counters (`/sys/class/net/<nic>/statistics/{tx,rx}_packets`, `rx_missed_errors`), 1 s steady windows — not the generator's self-report |

Latency is deliberately out of scope here: this note is about throughput and counters, and a
defensible p50/p95/p99 must be anchored to a tcpdump wire capture (methodology rule 7) — a
separate measurement, a separate document. The current head-to-head result is the
[v2.7.5 four-server campaign](#v275-kxdpgun-vs-dnsmark-xdp-across-4-servers--both-read-the-truth-at-the-nic-2026-07-03).

## What the two tools are for

**kxdpgun — the stress weapon.** *"How many packets can I throw, how does the server hold
up?"* Open-loop, instantaneous, minimal output (sent, received, reply size, reply bitrate).
It reports what it sent and what it received back — right for finding a breaking point,
though its received count can sit below what a fast server actually served under certain
return-path RSS conditions, and it leaves its XDP program attached on exit (the operator
detaches it afterward). A focused, low-overhead instrument for capacity and resilience
testing, and good at exactly that.

**dnsmark — the measured instrument.** *"What went on the wire, and can I trust the number?"*
AF_XDP datapath with a NIC-PHY egress check (it refuses to print a throughput the hardware
did not transmit) and a ramp to find saturation. It does not flood faster — at line rate the
two tie — it validates its egress against NIC PHY counters. (If a run hits a NIC left wedged
by a previous tool, dnsmark detects the gap between intended and PHY-transmitted packets and
flags the figure as fictional rather than print it.)

dnsmark also names *what* the ceiling is, from its own hardware observations. Building on the
authoritative `server_rx_qps` (the receiver NIC's rx counter, the reference throughout this
comparison), it divides that rate by the line-rate ceiling computed from the average on-wire
reply size (`rx_bytes/rx_packets`) and the egress-NIC link speed, and reports
**"% of line rate"** plus a verdict: **wire-bound** (the link is the wall) or
**link-headroom** (the server or the generator is the limit, not the wire). In fixed/flood
mode the text report prints, after the throughput block, a
`Line rate: X% of Y Gb/s wire (Z B replies, ceiling N M/s)` line and a `-> WIRE-BOUND` /
`-> link has headroom` verdict; `--json` adds a `line_rate` object
(`avg_reply_bytes`, `line_rate_pps`, `link_mbps`, `percent_of_line`, `rate_qps`,
`verdict`: `"wire-bound"` | `"link-headroom"`) plus a note. It is self-contained — no
receiver-side reading required — and works in both AF_XDP and kernel-UDP.

The line-rate verdict is emitted **only for fixed/flood runs, not in `--ramp`**. In `--ramp`
the `server_rx_qps` spans the whole ramp-up, so its average sits far below the peak and a
line-rate % there would contradict the DSD's own Capacity summary; the DSD Capacity /
Within SLO / Knee bracket is the throughput answer for a ramp, and the line-rate line appears
only in fixed/flood mode, where `server_rx_qps` reflects one steady window. In `--ramp` the
`--json` `line_rate` object is `null` accordingly.

## The rule both demonstrate

A traffic generator measures *itself*: what it sent, and what it managed to receive back.
Neither is the server's throughput. To benchmark a DNS server you read **three** hardware
counters — generator egress, server ingress, server egress — and the server's egress is the
only one that says what it served. Read the receiver.

## Reproduce

```bash
# Server (Runbound, xdp: yes), per methodology
ethtool -L <nic> combined 64                 # queues = cores; covers the NIC's NUMA node
ethtool -A <nic> rx off tx off
ethtool -N <nic> rx-flow-hash udp4 sdfn
cpupower frequency-set -g performance         # + IRQs one-per-core, ulimit -l unlimited
runbound -c receiver-bench.conf               # forward-zone cache; warm the 10k corpus first

# Truth = receiver NIC, 1 s windows:
cat /sys/class/net/<nic>/statistics/tx_packets   # served
cat /sys/class/net/<nic>/statistics/rx_packets   # received
ethtool -S <nic> | grep rx_missed_errors

# dnsmark — firehose
dnsmark -s <ip> -p 53 -d top-10000-domains.txt --xdp -Q 13000000 --max-outstanding 0 -l 22

# kxdpgun — firehose (query file needs "name type" per line)
kxdpgun -t 15 -Q 20000000 -i queries-with-types.txt <ip>
ip link set <gen_nic> xdp off                # detach its program afterwards
```

In an XDP firehose run latency reads `0.000 ms` because firehose does not sample RTT
(`-Q 0 --max-outstanding 0`), and the `Line rate` line and verdict appear only in
fixed/flood, never in `--ramp`.

## Which to use

- Stress / capacity / breaking point: **kxdpgun**.
- A served-throughput figure you can defend, egress confirmed at the PHY: **dnsmark** — and
  read the receiver NIC either way.

---

## v2.7.5: kxdpgun vs dnsmark-xdp across 4 servers — both read the truth at the NIC (2026-07-03)

The primary, current result. Same single-link X710 rig and methodology as above, but now the
two open-loop AF_XDP generators — **kxdpgun 3.4.6** and **dnsmark 2.7.5 `--xdp`** — are pointed
at **four servers in strict parity** (all forward+cache to 1.1.1.1 / 8.8.8.8 / 9.9.9.9, DNSSEC
off, minimal-responses, large cache; the 100k-domain corpus warmed first so every measured
query is a cache hit). This is the head-to-head the whole document has been building toward:
two AF_XDP flooders, four servers, one truth counter.

**Rig:** generator host = dual Xeon E5-2690 v2 (20c/40t); receiver host = AMD Threadripper PRO
5995WX; direct 10 GbE DAC, single-link on the Intel X710 (i40e), target 10.71.10.1.
**Truth** = receiver NIC `tx_packets` delta / 20 s — the replies the server actually put on the
wire, the same datapath- and tool-independent measure used throughout. dnsmark also self-reports
it as `server_rx_qps`.

**Generator commands:** dnsmark 2.7.5 XDP firehose `--xdp -Q 13M
--max-outstanding 0`; kxdpgun 3.4.6 `-Q 13M`.

Server throughput (qps) measured at the receiver NIC tx, the two AF_XDP columns side by side:

| Server | dnsmark-xdp (2.7.5) | kxdpgun (3.4.6) |
|--------|--------------------:|----------------:|
| BIND9 9.x | **872 k** | **1.03 M** |
| Unbound 1.22 | 3.06 M | 2.80 M |
| Runbound (xdp:no) | 6.81 M | 5.50 M |
| Runbound (xdp:yes) | **12.5 M** | 10.1 M |

Two things fall out of this pair of open-loop AF_XDP columns:

1. **On every robust server dnsmark `--xdp` edges kxdpgun.** Runbound xdp:yes reads 12.5 M vs
   10.1 M, Runbound xdp:no 6.81 M vs 5.50 M, Unbound 3.06 M vs 2.80 M. Both flood at 13 M/s
   offered; dnsmark cycles the UDP source port over an internal spread (2048 ports) so the
   receiver's RSS fans queries across its RX queues and stays balanced, and more replies come
   back on the wire.
   The gap is real but modest — same order, same ranking — because both are lossless zero-copy
   AF_XDP RX reading the same NIC-tx truth.

2. **Both AF_XDP tools drive BIND9 below its kernel-UDP rate — 872 k and 1.03 M.** Under the 13 M/s XDP
   firehose BIND9 goes into classic receiver livelock: it burns all CPU in softirq handling the
   ingress storm and *processes fewer queries than at a gentler offered rate* (a gentler
   kernel-UDP dnsmark run pulled 1.89 M out of the same BIND9). The two AF_XDP generators
   disagree by only ~160 k here because there is almost
   nothing left to measure — the server has fallen over, and both counters catch it at the NIC
   regardless of which tool drove the flood.

The lesson is the one this whole document keeps making: **because the truth is read at the
receiver's NIC-tx counter, kxdpgun and dnsmark-xdp are directly comparable across all four
servers** — the small dnsmark edge on robust servers and the shared BIND9 collapse are both
properties of the *servers under an open-loop AF_XDP flood*, not artifacts of one tool's
self-accounting. Read the receiver, and any two lossless-RX generators agree on what happened.

**Caveats (stated honestly):** single 20 s runs, not averaged (±10–15 % run-to-run). The
Runbound xdp:yes 12.5 M exceeds the single-link line rate for 103 B replies because this
particular warm produced smaller average replies, and line rate is reply-size dependent — the
figure is still the receiver-NIC-tx truth, just at a smaller average frame. All numbers above are
that receiver-NIC-tx truth.

---

*Current version: **v2.7.5**.*
