# Empirical validation — dnsmark reference quality

This document describes **how** dnsmark's reference quality is established: the
cross-validation methodology used to confirm that its throughput and latency figures
agree with independent references (NIC hardware counters and dnsperf), and what each
dimension of the tool is checked against.

> **Latest measured results:** the numbers from any given campaign live with that
> campaign. The current reference dataset is the 2026-07-03 dnsmark v2.7.5 four-server ×
> four-generator open-loop campaign — see `docs/benchmarking.md` §6 and the
> cross-validation write-up in `docs/cross-validation-dnsperf.md` §4 (kxdpgun 3.4.6
> cross-validated on all four servers at the receiver NIC). This document intentionally
> carries no measured numbers; it documents the validation procedure only.

## What is validated, and against what

| Dimension | What "reference quality" means | Independent reference |
|-----------|-------------------------------|-----------------------|
| Knee detection accuracy | The auto-detected knee is a fixed, small fraction below the flood ceiling, reproducibly | Flood ceiling from dnsperf / NIC counters |
| Absolute p50 latency | dnsmark closed-loop p50 matches an independent latency tool at the same offered load | dnsperf mean latency |
| Hockey-stick shape | p50 stays flat below the knee and inflects sharply just above it | The knee the tool itself reports |
| Reproducibility | Repeated runs against a stable server agree within a tight band | Prior runs (same server state) |
| p99 accuracy | Tail latency tracks real server behavior, distinguishing tool noise from server artifacts | dnsperf tail, cross-run |

**Prerequisite for reproducible results**: pre-warm the server cache before each run
(one dnsmark or dnsperf flood pass, ≥30 s). The server's cache state is the dominant
source of inter-run variation — cold→warm swings dwarf warm→warm swings — so all
comparison runs must start from a warm, steady server state.

---

## 1. DSD reproducibility — method

Run three or more consecutive DSD (Dichotomic Saturation Discovery) passes on the same
corpus against the same server, with a short gap between runs, and compare the detected
knee, peak served, and the latency-pass p50 across runs.

What to look for:
- Warm→warm runs should agree within a tight band (single-digit permille to low percent).
- A monotonic drift across the first runs indicates **server-side cache warming**
  (each ramp step touches more of the corpus), not tool instability — hold this constant
  by pre-warming.
- Each DSD run should converge in a bounded number of EXP+BISECT steps to a narrow
  bracket. The auto-SLO floor adapts to the measured ambient latency: it is
  self-calibrating, with no manual threshold to set.

Reproducibility is established when repeated measurements on a stable, warm server land
in the same bracket.

---

## 2. Throughput ceiling cross-validation — method

Point every tool at the **same server in the same measurement window** and compare:

- **NIC hardware counters** on the receiver over the flood window — the ground-truth
  packet rate the server actually processed.
- **dnsperf self-report** flooding closed-loop with enough concurrency/threads to
  saturate — the flood ceiling as the generator sees it.
- **kxdpgun** at the receiver NIC — an independent AF_XDP flood generator, read at the
  same hardware counters.
- **dnsmark peak served** (the DSD overshoot) and **dnsmark knee** (the sustainable
  SLO-bounded rate).

Expected relationships (the invariants the cross-validation confirms):
- dnsperf flood ceiling and NIC counters must agree to within hardware-counter noise —
  this is the hardware cross-check that anchors everything else.
- The dnsmark knee sits a fixed, small fraction **below** the flood ceiling. This is by
  design: the knee is the maximum *sustainable* throughput under a p50 SLO, whereas the
  flood ceiling is reached only by dropping the SLO entirely (latency is allowed to blow
  up at the ceiling).
- dnsmark peak served sits just below the flood ceiling (the final bisection step).

**dnsmark reports the sustainable knee**; dnsperf and kxdpgun report the flood ceiling.
Each answers a different question: the knee is the maximum rate under a latency SLO; the
flood ceiling is the raw upper bound regardless of latency. A valid cross-validation
confirms both figures against the NIC, not that they are equal.

> Note on kxdpgun: an early attempt to cross-validate kxdpgun failed due to a script
> timing bug (measurement window DT=0) that prevented NIC-counter extraction. It was
> subsequently cross-validated on all four servers in the v2.7.5 campaign
> (`docs/cross-validation-dnsperf.md` §4).

---

## 3. Latency accuracy vs dnsperf — method

Run dnsmark in **fixed-QPS closed-loop** (bounded outstanding requests) and dnsperf at
the **same offered load** against the same server, then compare their latency reports.

Read the comparison correctly — the two tools report different statistics:
- dnsmark reports **p50 (median)**; dnsperf reports the **arithmetic mean**. For a
  symmetric distribution, mean > median *by construction*, so a gap at low load is a
  statistical artifact, not a measurement error.
- Near the knee the distribution becomes **bimodal** (most replies fast, a slow tail).
  dnsmark's p50 tracks the fast mode; dnsperf's mean is pulled up by the tail. Both are
  correct — they describe different aspects of the same distribution.

Latency accuracy is established when, at a load where the distribution is tight around the
mode, dnsmark p50 and dnsperf mean converge within measurement noise, and their
divergence elsewhere is fully explained by the median-vs-mean distinction above.

---

## 4. Load-latency curve — hockey-stick shape — method

Sweep fixed QPS in closed-loop from well below to above the detected knee and plot p50 and
p99 versus offered load. The validation checks that the curve has the expected shape:

- **p50 stays flat** below the knee, then **inflects sharply** just above it (a small
  percentage over the knee should already multiply p50). Far above the knee, p50 explodes.
  The knee the tool reports should coincide with this inflection point.
- **Closed-loop backpressure** is expected once the server saturates: when RTT rises, the
  in-flight window fills and the sender throttles, so actual egress falls below the target.
  This self-regulation is a feature of closed-loop generation, not a measurement fault.
- **p99 bimodal spikes** at specific load levels — present at one load, absent at adjacent
  ones, while p50 stays stable through them — are **server artifacts** (e.g. resolver
  prefetch/refresh cycles), visible to any measurement tool. For p99 reporting, average
  across multiple runs; do not attribute these to the generator.

---

## 5. What dnsmark measures that dnsperf does not

| Feature | dnsmark | dnsperf |
|---------|---------|---------|
| Sustainable throughput (knee) | **Yes** | No (flood only) |
| Auto-SLO (no threshold to configure) | **Yes** | No |
| Full percentiles (p50/p99/p999) | **Yes** | Mean only |
| Hockey-stick detection | **Yes** | No |
| Latency pass at knee | **Yes** | No |
| NIC-speed AF_XDP path | **Yes** | No |

---

## Reproducing a validation run

General procedure (server-, corpus-, and rig-independent):
1. Pin the generator and server on a direct link; record ping RTT as the latency floor.
2. Load a corpus of real domains in dnsperf format (shared by dnsmark and dnsperf so both
   tools drive identical queries).
3. Pre-warm the server cache (≥30 s flood pass) before every measured run.
4. Run the four checks above (§1–§4), each against its independent reference.
5. Read NIC hardware counters on the receiver for every throughput figure.

For the current measured dataset produced with this procedure, see the v2.7.5 campaign in
`docs/benchmarking.md` §6 and `docs/cross-validation-dnsperf.md` §4.
