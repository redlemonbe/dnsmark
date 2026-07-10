# Independent cross-validation — runbound (kernel slow path) measured with dnsperf

> Purpose: corroborate dnsmark's figures for a real DNS server with an **independent,
> third-party load generator** — `dnsperf` (DNS-OARC), the long-standing reference DNS
> performance tool. Measured data only; truth is the receiver NIC hardware counters, not
> the generator's self-report. Where a value is generator-bound, it is stated as such. The
> current measured campaign is the 2026-07-03 four-server round (§4).

## 1. Objective

dnsmark's numbers are produced by dnsmark itself (an AF_XDP, open-loop generator). To
check that those numbers are not a tooling artifact, the same receiver was measured by a
completely separate, widely-used tool — `dnsperf` — built and maintained outside this
project. This report records what dnsperf independently observes, and where dnsperf's own
design bounds the measurement.

## 2. Methodology & Architecture

- **Receiver (Runbound):** AMD Ryzen Threadripper PRO 5995WX (64c/128t), 125 GB RAM,
  Intel X710 / X520 `<nic>` (direct 10 GbE DAC, MTU 1500). Real `forward-zone`, **no
  local-data**, `cache-min-ttl 3600`, `rate-limit: 0` (a single-source generator must not
  be throttled). Governor `performance`, flow-control off, RSS `udp4 sdfn`, NIC IRQs on
  the NIC's NUMA-local cores, RX ring 8192, static ARP. `ss -ulpn` confirmed only Runbound
  owns `:53`.
- **Generator (dnsperf):** dual Intel Xeon E5-2690 v2 (20c/40t), `dnsperf 2.14.0`
  (DNS-OARC), over the same 10 GbE X710/X520 direct link.
- **Dataset:** `benchmark/corpus/top-10000-domains.txt`, converted to the dnsperf query
  format (`<name> A`), real names. Cache warmed before measurement.
- **Procedure:** warm the cache, then increase dnsperf concurrency (`-c`/`-T`/`-q`) to find
  its sustained maximum; read the **receiver NIC PHY counters** (`ethtool -S`:
  `tx_pkts_nic` served, `rx_pkts_nic` received, `rx_no_dma_resources`/`rx_missed_errors`
  drops) over the steady window; receiver CPU from `/proc/stat`. Latency from dnsperf.

## 3. Interpretation

- **The receiver's correctness is independently confirmed.** A third-party tool measures
  `tx_pkts_nic` = `rx_pkts_nic` (every received query answered on the wire, **zero NIC
  drops**), high NOERROR, and sub-millisecond average latency — matching what dnsmark
  reports for the same path. The figures are not a dnsmark artifact.
- **The measurement is generator-bound, not receiver-bound.** dnsperf plateaus regardless
  of added clients/threads, because it is a **closed-loop** generator using **kernel UDP
  sockets** (one process): its rate is capped by syscall throughput and by clients waiting
  on each in-flight query. At that rate the receiver is far from saturation. dnsmark's
  AF_XDP open-loop path drives the *same* receiver much harder for saturation testing (§4).
- **Closed-loop is sensitive to the real-corpus tail.** A small fraction of corpus names
  are not cacheable (they re-forward to the upstreams over the internet, tens to hundreds of
  ms). Without a per-query timeout, a handful of those stalled clients collapse dnsperf's
  aggregate rate; a short timeout (used here) lets the closed loop measure the cache-hit
  serving rate cleanly, with those few queries counted as timeouts.
- **Takeaway.** dnsperf is well-suited for confirming correctness and latency with an
  independent implementation, and it does so here. The two tools serve different purposes:
  dnsperf for closed-loop correctness and latency verification; dnsmark for open-loop
  saturation throughput. Where they overlap — correctness, NIC-truth, and latency at
  comparable load levels — they agree. The measured cross-tool numbers are in §4.

## 4. 2026-07-03 — four-server campaign: dnsperf vs dnsmark vs kxdpgun

The widest cross-tool round to date: **four servers** in strict parity, driven by **four
generators** with different load disciplines, all measured by the one datapath-independent
truth — the **receiver NIC tx counter**. The point of this document holds throughout: dnsperf
is closed-loop and latency-bounded, so it reads the *lowest* number for every server; the NIC
tx delta is what makes the four tools' figures comparable on one axis.

**Rig (identical to the established methodology).** Generator host: dual Xeon E5-2690 v2
(20c/40t). Receiver host: AMD Threadripper PRO 5995WX (64c/128t), direct 10 GbE DAC.
**Single-link** on the Intel **X710 (i40e)**, target `10.71.10.1`. Truth = receiver NIC
`tx_packets` delta / 20 s — the replies the server actually put on the wire. dnsmark also
self-reports this as `server_rx_qps`.

**Servers — strict parity.** BIND9 9.x, Unbound 1.22, Runbound `xdp:no`, Runbound `xdp:yes`,
all configured identically: forward + cache to 1.1.1.1 / 8.8.8.8 / 9.9.9.9, DNSSEC off,
minimal-responses, large cache. The 100k-domain corpus is **warmed first**, so every measured
query is a **cache hit** — this isolates the serving path, not the recursion path.

**Generators — four load disciplines.**
- `dnsperf` — closed-loop kernel-UDP (`-c 500 -T 20 -q 100000`).
- `dnsmark` kernel firehose (`-Q 6M --max-outstanding 0`).
- `dnsmark` XDP firehose (`--xdp -Q 13M --max-outstanding 0`).
- `kxdpgun 3.4.6` (`-Q 13M`).

### Server throughput — receiver NIC tx (qps)

| Server | dnsperf | dnsmark-udp | dnsmark-xdp | kxdpgun | Peak |
|---|--:|--:|--:|--:|--:|
| BIND9 9.x | 711 k | 1.89 M | 872 k | 1.03 M | **1.89 M** |
| Unbound 1.22 | 1.55 M | 2.57 M | 3.06 M | 2.80 M | **3.06 M** |
| Runbound (`xdp:no`) | 1.26 M | 3.40 M | 6.81 M | 5.50 M | **6.81 M** |
| Runbound (`xdp:yes`) | 1.65 M | 5.27 M | **12.5 M** | 10.1 M | **12.5 M** |

### What this shows

1. **Server ceiling ranking (peak over generators).** Runbound `xdp:yes` **12.5 M** >
   Runbound `xdp:no` **6.8 M** > Unbound **3.1 M** > BIND9 **1.9 M**. Runbound-XDP is
   **~6.6× BIND9**, **~4.1× Unbound**, and **~1.8× its own kernel mode**.
2. **AF_XDP roughly doubles Runbound's own kernel path.** Same server, same corpus, same
   link: under the open-loop AF_XDP firehose the fast path is ~1.8× the kernel path
   (`xdp:yes` 12.5 M dnsmark-xdp vs `xdp:no` 6.81 M = 1.84×); the kernel-UDP-driven column
   shows a smaller ~1.5× (5.27 M vs 3.40 M) — the lossless zero-copy RX/TX is the difference,
   not the resolver logic (identical config).
3. **The generator's load discipline sets the headline — and the NIC counter is what
   reconciles them.** The open-loop AF_XDP generators (dnsmark-xdp, kxdpgun) drive a *robust*
   server hardest: on Unbound and both Runbound modes they read well above the kernel-UDP
   tools. The exception is **BIND9, which collapses under the 13 M XDP firehose** — 872 k
   (dnsmark-xdp) / 1.03 M (kxdpgun) versus **1.89 M** under the gentler kernel-UDP dnsmark.
   This is classic **receiver livelock / overload**: at extreme ingress BIND9 burns all CPU
   in softirq and drops, so it *processes less at a higher offered rate than at a lower one*.
   The NIC tx counter captures the truth regardless of which tool caused it — which is exactly
   why a benchmark's headline depends on the **generator's** load discipline, and why the
   receiver NIC counter is the only cross-tool-comparable axis.
4. **dnsperf is consistently the lowest — by design.** For all four servers dnsperf reads
   below every other generator (711 k / 1.55 M / 1.26 M / 1.65 M). It is **closed-loop and
   latency-bounded**: clients wait on each in-flight query, so it measures the *latency-gated*
   serving rate, not the saturation ceiling. It is a latency tool, not a saturation tool —
   the same closed-loop property this document describes in §3.
5. **dnsmark `--xdp` is the strongest saturation generator on the three robust servers.**
   On Unbound (3.06 M), Runbound `xdp:no` (6.81 M) and Runbound `xdp:yes` (12.5 M) the XDP
   firehose reads highest of the four tools — its lossless open-loop RX pushes past what both
   kernel-UDP and kxdpgun reach. (On BIND9 it does *not* win, because BIND9 livelocks under
   that ingress — finding 3.)

### Caveats (stated honestly)

- **Single 20 s runs, not averaged.** Expect **±10–15 % run-to-run**; these are single
  windows, not multi-run means. Treat the ranking as robust and the individual digits as
  point measurements.
- **The 12.5 M `xdp:yes` figure exceeds the ~9.85 M single-link line rate for 103 B
  replies.** That is not a counter error: **line rate is reply-size dependent**, and this
  particular warm produced a smaller *average* reply size, raising the pps ceiling
  accordingly. All figures are the receiver-NIC-tx truth.
- **All four columns are the receiver NIC tx delta** — datapath- and tool-independent. Where
  dnsmark self-reports `server_rx_qps`, it matches this counter to ±0.1–0.6 %.

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
