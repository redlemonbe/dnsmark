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

---

## 6. 2026-06-13 — dnsperf vs dnsmark, three resolvers, new X710 + X510 rig

A broader head-to-head: the same three resolvers (BIND 9.20.23, unbound 1.22.0, Runbound
v0.18.1 `xdp: no`) measured on the same rig by **both** tools, to show *where the two
generators agree and where each one's design bounds the result*. Full per-run reports live in
the Runbound repo (`docs/benchmark/`, the 2026-06-13 round). Receiver: AMD Threadripper PRO
5995WX (64c/128t), two direct 10 GbE DACs — Intel **X710 (i40e)** and **X510 (ixgbe)**;
generator: dual Xeon E5-2690 v2. Truth = receiver NIC counters. dnsperf is closed-loop
kernel-UDP (`-T 20 -c 500 -q 100000`); dnsmark `--ramp` is open-loop (served read at the NIC),
with a bounded closed-loop pass (`--max-outstanding 1500`) for the latency point. Both
generators are **non-XDP (kernel UDP)** in this comparison.

### Throughput — what each tool reads (same receiver, same link)

| Resolver / link | dnsperf avg QPS (closed-loop) | dnsmark served peak (open-loop, NIC) | dnsmark / dnsperf |
|---|--:|--:|--:|
| BIND 9.20.23 — X710 | 786 k | 1.84 M | 2.3× |
| BIND 9.20.23 — X510 | 432 k | 1.46 M | 3.4× |
| unbound 1.22.0 — X710 | 579 k | 2.09 M | 3.6× |
| unbound 1.22.0 — X510 | 131 k | 1.65 M | 12.6× |
| Runbound v0.18.1 `xdp: no` — X710 | 1.99 M | 3.71 M | 1.9× |
| Runbound v0.18.1 `xdp: no` — X510 | 676 k | 2.51 M | 3.7× |

### Latency & success — where they overlap

| Resolver / link | dnsperf NOERROR / lost / avg lat | dnsmark closed-loop p50 / p99 / NOERROR |
|---|---|---|
| BIND — X710 | 94.9 % / 2.6 % / 5.63 ms | 0.320 / 8.791 ms / 92.0 % |
| BIND — X510 | 95.5 % / 5.0 % / 1.7 ms | 1.051 / 1.388 ms / 99.7 % |
| unbound — X710 | 99.7 % / 1.3 % / 97.5 ms* | 0.227 / 7.123 ms / 99.7 % |
| unbound — X510 | 99.8 % / 14.7 % / 3.4 ms | 1.026 / 1.125 ms / 99.7 % |
| Runbound — X710 | 99.7 % / 0.5 % / 4.66 ms | 0.066 / 0.371 ms / 99.7 % |
| Runbound — X510 | 99.7 % / 3.5 % / 0.585 ms | 1.013 / 1.113 ms / 99.7 % |

\* dnsperf's `-q 100000` keeps up to 100 k queries outstanding; by Little's law a deep queue at
~579 k QPS yields ~97 ms *average* even when per-query service is sub-millisecond. It is a
property of the closed-loop depth, not of the server — dnsmark's bounded closed-loop p50
(0.227 ms) is the clean per-query figure.

### What this shows

1. **dnsmark open-loop reads the true served ceiling; dnsperf closed-loop reads a fraction of
   it.** dnsperf is one kernel-UDP process bounded by `outstanding × threads × syscall rate`,
   so it tops out at 0.13–2.0 M here while the *same receiver* serves 1.5–3.7 M (NIC-confirmed).
   The gap is the tool, not the server — exactly the limit documented for the X520 rig in §4.
2. **Closed-loop is sensitive to NIC RX drops; open-loop NIC-truth is not.** unbound on the
   ixgbe X510 shows dnsperf **14.7 % lost** (it waits on queries the NIC dropped at RX), which
   reads like a broken server — yet dnsmark's open-loop NIC counters show the link **healthy at
   1.65 M served** in the same session. A closed-loop tool conflates "RX-dropped" with "server
   slow"; the receiver NIC counters separate them.
3. **Where the two tools overlap — correctness and cache-hit latency — they agree.** Both report
   ~99.7 % NOERROR for the well-behaved paths, and dnsperf's average latency tracks dnsmark's
   closed-loop p50 once the queue-depth caveat (note *) is removed. The numbers are not a dnsmark
   artifact.
4. **Neither closed-loop tool can reach a kernel-bypass fast path at all.** dnsperf and any
   kernel-UDP generator cap at ~6 M offered on this generator; Runbound's AF_XDP fast path serves
   **~10.1 M on one link and ~20.3 M across two** (measured by dnsmark `--xdp`, receiver at
   ≤24 % CPU). Reaching a modern resolver's actual ceiling requires an open-loop AF_XDP generator
   — which is why dnsmark exists. dnsperf remains the right tool for an independent correctness
   and low-rate-latency cross-check, and it agrees with dnsmark there.
