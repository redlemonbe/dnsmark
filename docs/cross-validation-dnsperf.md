# Independent cross-validation — runbound (kernel slow path) measured with dnsperf

> Purpose: corroborate dnsmark's figures for a real DNS server with an **independent,
> third-party load generator** — `dnsperf` (DNS-OARC), the long-standing reference DNS
> performance tool. Measured data only; truth is the receiver NIC hardware counters, not
> the generator's self-report. Where a value is generator-bound, it is stated as such.

## 1. Objective

dnsmark's numbers are produced by dnsmark itself (an AF_XDP, open-loop generator). To
check that those numbers are not a tooling artifact, the same receiver was measured by a
completely separate, widely-used tool — `dnsperf` — built and maintained outside this
project. This report records what dnsperf independently observes, and where dnsperf's own
design bounds the measurement.

## 2. Methodology & Architecture

- **Receiver (Runbound):** AMD Ryzen Threadripper PRO 5995WX (64c/128t), 125 GB RAM,
  Intel X520 / 82599 `<nic>` (`ixgbe`, PCIe 2.0 x8, MTU 1500), kernel 7.0.6-2-pve,
  Runbound v0.16.6, **`xdp: no`** (kernel slow path). Real `forward-zone`, **no
  local-data**, `cache-min-ttl 3600`, `rate-limit: 0` (a single-source generator must not
  be throttled). Governor `performance`, flow-control off, RSS `udp4 sdfn`, NIC IRQs on
  the NIC's NUMA-local cores, RX ring 8192, static ARP. `ss -ulpn` confirmed only Runbound
  owns `:53`.
- **Generator (dnsperf):** dual Intel Xeon E5-2690 v2 (20c/40t), `dnsperf 2.14.0`
  (DNS-OARC), over the same 10 GbE X520 ↔ X520 direct fibre.
- **Dataset:** `benchmark/corpus/top-10000-domains.txt`, converted to the dnsperf query
  format (`<name> A`), 10 000 real names. Cache warmed before measurement.
- **Procedure:** warm the cache, then increase dnsperf concurrency (`-c`/`-T`/`-q`) to find
  its sustained maximum; read the **receiver NIC PHY counters** (`ethtool -S`:
  `tx_pkts_nic` served, `rx_pkts_nic` received, `rx_no_dma_resources`/`rx_missed_errors`
  drops) over the steady window; receiver CPU from `/proc/stat`. Latency from dnsperf.

## 3. Raw Results

| Metric | Value | Source |
|--------|-------|--------|
| Sustained throughput (dnsperf max) | **~238 k QPS** | dnsperf, NIC-confirmed |
| Served on the wire (NIC truth) | **238 386 QPS** | receiver `tx_pkts_nic` |
| Received on the wire (NIC truth) | 238 380 QPS | receiver `rx_pkts_nic` |
| NIC drops | **0** (`rx_no_dma_resources` = `rx_missed_errors` = 0) | receiver `ethtool -S` |
| Average latency (at max) | **0.118 ms** (min 0.027 ms) | dnsperf |
| Average latency (controlled 43 k) | **0.091 ms** (min 0.034 ms) | dnsperf |
| Success rate | **99.85 % NOERROR** | dnsperf rcodes |
| Lost (timed-out forwards) | 0.12 % | dnsperf |
| **Receiver CPU at 238 k** | **3.4 %** | `/proc/stat` |

Pushing dnsperf from `-c 60 -T 30` to `-c 200 -T 40` did **not** raise throughput
(238 k → 237 k): dnsperf had reached its own ceiling, while the receiver stayed at 3.4 %
CPU.

## 4. Interpretation

- **The receiver's correctness is independently confirmed.** A third-party tool measures
  `tx_pkts_nic` = `rx_pkts_nic` (every received query answered on the wire, **zero NIC
  drops**), 99.85 % NOERROR, and sub-150 µs average latency — matching what dnsmark
  reports for the same path. The figures are not a dnsmark artifact.
- **The measurement is generator-bound, not receiver-bound.** dnsperf plateaus at
  ~238 k QPS regardless of added clients/threads, because it is a **closed-loop**
  generator using **kernel UDP sockets** (one process): its rate is capped by syscall
  throughput and by clients waiting on each in-flight query. At that rate the receiver is
  **3.4 % busy** — it is nowhere near saturation. dnsmark's AF_XDP open-loop path drives
  the *same* receiver to roughly **7 M QPS** on this rig; dnsperf exercises ~3–4 % of that.
- **Closed-loop is sensitive to the real-corpus tail.** ~0.3 % of the corpus names are not
  cacheable (they re-forward to the upstreams over the internet, tens to hundreds of ms).
  Without a per-query timeout, a handful of those stalled clients collapse dnsperf's
  aggregate rate to ~25 k; a 0.2 s timeout (used here) lets the closed loop measure the
  cache-hit serving rate cleanly, with those few queries counted as timeouts.
- **Takeaway.** dnsperf is excellent for confirming correctness and low-rate latency with
  an independent implementation, and it does so here. It is not built to saturate a
  kernel-bypass resolver: reaching a modern resolver's actual ceiling needs an open-loop,
  AF_XDP generator — which is the reason dnsmark exists. The two tools agree where they
  overlap (correctness, latency, NIC-truth), and dnsperf's own limit is the expected one.

## 5. Appendix — exact commands

```bash
# Receiver (the receiver host) — runbound xdp:no, methodology host setup
cpupower frequency-set -g performance
ethtool -A <nic> rx off tx off
ethtool -N <nic> rx-flow-hash udp4 sdfn
ethtool -G <nic> rx 8192
ss -ulpn | grep 10.0.0.1:53            # rule 5: only runbound owns :53
runbound -c rb-single-noxdp.conf          # xdp:no, no local-data, cache-min-ttl 3600, rate-limit 0

# Generator (the generator host) — dnsperf 2.14.0 (DNS-OARC)
awk '{print $1" A"}' top-10000-domains.txt > queries.txt   # dnsperf query format
ethtool -A nic2 rx off tx off
dnsperf -s 10.0.0.1 -p 53 -d queries.txt -c 60  -T 30 -q 100 -t 0.2 -l 25   # sustained max
dnsperf -s 10.0.0.1 -p 53 -d queries.txt -c 200 -T 40 -q 100 -t 0.2 -l 22   # confirm ceiling
dnsperf -s 10.0.0.1 -p 53 -d queries.txt -c 10  -T 10 -Q 50000 -q 100 -t 1 -l 10  # clean latency

# Throughput truth = receiver NIC PHY counters
ethtool -S <nic> | grep -wE 'tx_pkts_nic|rx_pkts_nic|rx_no_dma_resources|rx_missed_errors'
```
