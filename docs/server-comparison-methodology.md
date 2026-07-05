# DNS benchmark methodology: unbound vs BIND9 (closed-loop DSD / latency)

> This document describes the **closed-loop DSD / latency** methodology for comparing two
> kernel resolvers (unbound and BIND9) configured as caching forwarders. It is a
> **methodology reference** — the rig, server configs, corpus, pre-warm and measurement
> procedures, and the conceptual observations that make the comparison valid — with no
> measured result numbers.
>
> For the current **open-loop saturation** throughput ceilings (four-server ×
> four-generator campaign, receiver-NIC-measured), see `docs/benchmarking.md` §6 and the
> cross-validation in `docs/cross-validation-dnsperf.md` §4.

---

## 1. Rig

| Role | Machine | NIC | Link |
|------|---------|-----|------|
| Generator | dragonsage — dual Intel Xeon E5-2690 v2 (40 threads) | Intel X710 (i40e), 10 GbE | direct to server |
| Server | vm-dr — KVM guest, 8 vCPUs on dragonrage (AMD Threadripper PRO 5995WX) | virtio 10 GbE (`ens19`) | — |
| Tools | dnsmark, dnsperf, BIND9, unbound | — | — |

Record the generator→server ping baseline (min/avg/max, loss) before every campaign — it
bounds the floor latency any resolver can achieve on the rig.

> **Rig caveat.** vm-dr is a KVM guest; its virtio NIC adds latency not present in a physical-to-physical
> setup. Absolute numbers are specific to this rig. The relative comparison between the two
> DNS servers is valid because both run in the same environment under identical load.

---

## 2. Server configurations

Both servers run as **caching forwarding resolvers**, forwarding to 1.1.1.1 and 8.8.8.8.
Both use 256 MB cache, the same corpus, and all 8 vCPUs.

### unbound

```
server:
  interface: 10.71.20.50
  port: 53
  num-threads: 8
  so-reuseport: yes
  so-rcvbuf: 8m
  so-sndbuf: 8m
  msg-cache-size: 256m
  rrset-cache-size: 256m
  cache-min-ttl: 300
  ratelimit: 0
  ip-ratelimit: 0
  verbosity: 0

forward-zone:
  name: "."
  forward-addr: 1.1.1.1
  forward-addr: 8.8.8.8
```

### BIND9

```
options {
  listen-on port 53 { 10.71.20.50; };
  allow-query { any; };
  recursion yes;
  dnssec-validation no;
  forwarders { 1.1.1.1; 8.8.8.8; };
  forward only;
  max-cache-size 256m;
  min-cache-ttl 90;
};
```

**min-cache-ttl difference:** BIND9 9.20 enforces an internal maximum of 90 s; unbound accepts 300 s.
This difference has a measurable effect on cache stability during the test (see §6.6) and must
be accounted for when interpreting BIND9 throughput at any point in the run.

---

## 3. Corpus

10,000 domain names from the Tranco top-1M list, in dnsperf query format (`<name> A`).
Identical file used for both tools and both servers.

---

## 4. Pre-warm procedure

```bash
dnsperf -s 10.71.20.50 -d corpus.txt -c 8 -T 8 -Q 5000 -l 120
sleep 10
```

Pre-warm coverage is server-dependent, not time-deterministic (see §6.1): the same 120 s
run resolves the same set of unique domains on both servers but by different mechanisms and
at very different total q/s. Read the achieved q/s, NOERROR/SERVFAIL split, and cached-domain
count directly from each server's pre-warm output rather than assuming equivalent coverage.

---

## 5. Measurement procedure

**[A] DSD × 3** — reproducibility bracket
```bash
dnsmark -s 10.71.20.50 -d corpus.txt --ramp -c 8 --no-tui
# × 3 runs, 3 s pause between runs
```

**[B] Fixed-load latency curve** — 9 QPS points
```bash
for QPS in 50000 100000 150000 200000 250000 290000 310000 330000 380000; do
    dnsmark -s 10.71.20.50 -d corpus.txt -c 8 -Q $QPS --max-outstanding 200 -l 20 --no-tui
done
```

**[C] dnsperf concurrency sweep**
```bash
for CONC in 5 20 50 200; do
    dnsperf -s 10.71.20.50 -d corpus.txt -c $CONC -T $CONC -l 20
done
```

**[D] dnsperf fixed QPS**
```bash
for QPS in 50000 100000 200000 280000; do
    dnsperf -s 10.71.20.50 -d corpus.txt -c 8 -T 8 -Q $QPS -l 20
done
```

For every run, record two independent quantities: the tool's reported achieved q/s, and the
**server NIC `tx_packets` egress counter** (§6.3). When the server is the bottleneck these
diverge, and only the egress counter reflects what the server actually served.

---

## 6. Observations

These conclusions are properties of the measurement method and the two resolvers'
architectures; they hold independent of any particular measured campaign.

### 6.1 Pre-warm calibration is server-dependent

The same procedure (120 s at 5K q/s) resolves the same set of unique domains on both
servers, but by different mechanisms:

- **unbound**: high total q/s dominated by fast SERVFAIL responses — upstream rate-limiting
  returns fast negative responses alongside the successful resolutions.
- **BIND9**: low total q/s, almost entirely NOERROR — forwards serially, a handful of unique
  resolutions per second, no SERVFAIL.

A pre-warm specified as "run for N seconds" does not guarantee equivalent cache coverage
across server implementations. The divergence is visible directly in the dnsperf output.

### 6.2 DSD floor latency as passive cache-state indicator

BIND9's per-run DSD floor latency drops monotonically across successive runs: each run's
traffic warms the cache further, so the floor falls toward unbound's warm-cache floor as the
test proceeds. Both then serve from a warm cache at comparable per-query speed.

This progression is readable from the DSD output alone, without any cache inspection
command or separate diagnostic probe.

### 6.3 Egress counter as server-side truth

The `egress` counter (server NIC `tx_packets`) and the generator's requested rate diverge
whenever the server is the bottleneck. Two failure modes are distinguishable:

- **Generator closed-loop ceiling**: egress plateaus below the target because the generator
  cannot send faster (its reply serialization saturates), not because the server is full.
- **Server bottleneck**: egress stays below target and can be **non-monotone** — requesting
  more does not raise the served rate, and served rate can even fall as cache state churns.

Without the egress counter, reporting a "250K q/s test" would imply the server served 250K
queries per second when the NIC counter may show far less. Always report served (egress)
rate, not requested rate.

### 6.4 dnsperf concurrency sweep: ceiling independent of `-c`

For unbound, dnsperf's achieved ceiling is essentially flat across `-c 5` through `-c 200`:
raising concurrency by 40× changes the result by a few percent. dnsmark's DSD finds a higher
ceiling for the same server because it uses an open-loop send path not constrained by the
closed-loop client/reply serialization of dnsperf.

For BIND9, the sweep declines as concurrency rises and as the test runs longer, because the
90-second cache entries expire and are re-forwarded, stalling clients (see §6.6).

### 6.5 dnsperf fixed-QPS: achieved rate vs. reported latency

dnsperf with `-Q` limits the send rate but remains closed-loop. When the server cannot
sustain the requested rate, clients block on unanswered queries. Only answered queries
count toward the reported average latency.

For BIND9 under a target it cannot meet, the reported average latency is accurate **only for
the small fraction of queries answered from cache** — the un-answered queries are excluded.
Reporting a latency figure without the achieved-vs-target ratio is misleading.

For unbound, a shortfall from a high target reflects the **generator's** own closed-loop
ceiling, not the server's — again distinguishable via the egress counter (§6.3).

### 6.6 Cache TTL stability across the test

unbound's `cache-min-ttl 300` keeps entries cached for at least 5 minutes. BIND9's maximum
`min-cache-ttl 90` expires entries every 90 seconds.

The 90-second TTL cap means BIND9's cache state resets on a ~90 s cycle throughout the test:
the measured throughput at any point depends on **when during that cycle** the measurement is
taken (freshly warmed vs. mid-expiry re-forwarding). unbound's 300 s TTL maintains a stable
cache state for the full test duration.

Consequence for scheduling: run the full BIND9 comparison within a bounded window and note
each section's elapsed time from pre-warm, or TTL expiry will confound throughput comparisons
between sections.

---

## 7. Reproduce

```bash
# Clone
git clone https://github.com/redlemonbe/dnsmark /tmp/dnsmark-bench
cd /tmp/dnsmark-bench

# Build dnsmark (requires Rust, Linux, net.admin cap or root)
cargo build --release
install -m 755 target/release/dnsmark /usr/local/bin/dnsmark

# Install dnsperf
apt install dnsperf

# Configure the server (see §2); set SERVER= in the script
# Start the NIC logger on the server before running:
ssh root@SERVER 'nohup bash -c \
  "while true; do printf \"%s %s\n\" \
  \"\$(date -u +%s)\" \"\$(cat /sys/class/net/ens19/statistics/tx_packets)\"; \
  sleep 5; done > /tmp/nic_log.txt" &'

# Run
bash benchmarks/scripts/bench_compare.sh 2>&1 | tee /tmp/results.log
```

**Reproducibility notes:**
- Pre-warm coverage is capped by upstream rate-limiting. If the corpus domains are already
  cached at the forwarders (1.1.1.1/8.8.8.8), pre-warm will be faster and more complete.
- DSD inter-run variance for unbound reflects cache prefetch cycles at high load. The floor
  latency sequence is the stable indicator of cache warmth.
- For BIND9 with `min-cache-ttl 90`, three DSD runs are the minimum to observe cache
  convergence. Schedule the full test to complete within ~30 minutes; longer gaps allow
  TTL expiry to reset cache state between sections.
