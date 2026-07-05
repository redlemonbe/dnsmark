# Changelog

## [2.7.7] - 2026-07-03

### Fixed — code
- **`--wire-latency` no longer hangs / never emitting a result (#18).** The reference-latency
  probe is a *serial* ping-pong (one query in flight), so a single slow reply stalls the whole
  pace — and a cache-miss that forwards upstream is 30–500 ms versus ~40 µs for a cache-hit.
  Three unbounded waits made a small request run for tens of seconds (or, if a datapath never
  emits a software TX stamp, busy-spin a core for `timeout_ms` × count ≈ minutes), printing
  nothing until the very end, so any outer `timeout` killed it before a single percentile
  appeared. Fixed with three bounds that keep it honest and always-terminating:
  - **TX timestamp:** waited via `POLLERR` in 1 ms slices for up to 5 ms (the stamp is ready in
    microseconds) instead of busy-spinning `std::hint::spin_loop()` up to `timeout_ms`; a sample
    whose TX stamp never shows is skipped, not stalled.
  - **Reply:** the per-sample wait is capped (`timeout_ms`, clamped to ≤ 250 ms) so one slow
    forward can't freeze the pace.
  - **Whole probe:** a wall-clock deadline (`4× the paced duration + 10 s`); on hit it stops and
    reports the samples collected rather than running unbounded.
  It now also prints a one-line progress counter and, on exit, notes how many sends got no reply
  (cache-miss hint). `-Q`/`-l` are honoured (`count` floor lowered 2000 → 200), so
  `--wire-latency -Q 500 -l 1` runs 500 samples in ~1 s instead of a forced 2000.
- **`--ramp` (DSD) no longer runs away without converging on a generator- / kernel-recv-bound
  path.** The step-sustained criterion was **latency-only** (`ok = latency_ok`; the computed
  `offer_ok` was explicitly discarded). On the kernel-UDP gated closed loop the shallow in-flight
  keeps p50 pinned at the floor (~40 µs) even once the generator can no longer *offer* the doubled
  target, so latency never trips — the Exp phase kept doubling to `MAX_DOUBLINGS` (100k → ~100 B),
  one wasted 5 s step each, ~100 s+ total (past any sane outer timeout → the ramp appeared to hang)
  and quoted a phantom multi-M "capacity" it never served. Fixes:
  - **Gate on offered/served throughput — kernel mode only**: a kernel-UDP step is sustained only
    when latency held **and** the datapath delivered ≥80 % of the target (`offer_ok`). Saturation
    now brackets at the real ceiling and the bisection converges in a handful of steps. This
    matches the mode's documented "SENT/offered-driven" intent, which the code did not implement.
    **XDP stays latency-only**: an AF_XDP firehose legitimately under-offers the (huge) target
    while the XSK ramps, so offer-gating it would false-saturate on the first low step and spiral
    the bisection down to noise — the SLO already trips at the wire ceiling there, as it always has.
  - **Hard bisection cap** (`MAX_BISECT = 14`): a noisy served-throughput signal (kernel-UDP
    generator jitter flipping the bracket bound) can never hunt forever — on the cap it stops and
    reports the tightest bracket found. The run always terminates.
  - Unit tests added for the generator-bound (anti-runaway), clean-knee (converges within the 5 %
    tolerance), noisy-oracle (terminates via the cap), and XDP-latency-only (no offer saturation)
    cases.
  Verified on the X710 single-link rig: kernel `--ramp` previously hit the 90 s bound (exit 124,
  same as released 2.7.5); it now converges to a ±1.6 % knee bracket in ~55 s. XDP `--ramp` still
  climbs to the real wire ceiling (~9.86 M/s, ±1.8 % bracket, ~72 s) — no regression.

## [2.7.5] - 2026-07-03

### Fixed — code
- **`Source:` label for the built-in corpus.** With no `-d` and not `--random`, the run uses
  the built-in 2000-domain corpus but the text/JSON summary mislabelled the source as
  `random`. It now reads `Source: builtin (2000 domains)` (`"type": "builtin"` in `--json`).
- **`--help` text for `--ramp`.** It claimed "auto-scale from 1000 QPS doubling every 5 s";
  the ramp actually starts at **100k** QPS (EXP doubling → bisection, DSD). Corrected.
- **Timeout sweep no longer allocates a throwaway `Vec`.** `sweep_with_ages`/`drain_all` built
  a `Vec<u64>` of per-slot ages every 10 ms, but every call site only ever used the count
  (`.len()` or `for _ in &ages`). Renamed to `sweep_expired`/`drain_all` returning `usize`; the
  doc-comment that wrongly claimed timed-out queries feed "the latency histogram (honest tail)"
  is corrected — a timeout is a **loss**, counted via `inc_timeout()`, never a latency sample
  (matches WHITEPAPER §4 / FINDINGS and all four call sites).
- **`--clients` long flag.** The concurrency argument was `-c` only (clap `short = 'c'` with no
  `long`), so `dnsmark --clients N` errored even though the docs use that name. Added `long` so
  both `-c` and `--clients` work (matches FINDINGS / WHITEPAPER usage).

### Changed — benchmarks (re-measured on 2.7.5)
- **Re-ran the four-server × four-generator campaign end to end with the 2.7.5 binary** on the
  baremetal single-link X710 rig (Runbound started via a systemd-managed unit with the documented
  `LimitMEMLOCK=infinity` + AF_XDP caps, not an ad-hoc launch). Every benchmark table/figure —
  README "server ceilings", WHITEPAPER §7a, benchmarking.md §6, cross-validation §4,
  dnsmark-vs-kxdpgun, empirical-validation — now carries 2.7.5-measured numbers. Headline
  unchanged (Runbound `xdp:yes` **12.5 M**); the AF_XDP ~1.8× over Runbound's own kernel path and
  the BIND9 receiver-livelock under the 13 M XDP firehose (872 k / 1.03 M vs 1.89 M kernel-UDP)
  both reproduce. Single 20 s runs, not averaged — cell-to-cell swings are large (several
  kernel-UDP and BIND9-livelock cells move 15–60 % vs 2.7.4). What reproduces *tightly* is the
  **AF_XDP throughput on the robust servers** — Runbound `xdp:yes` 12.5 → 12.5 M and `xdp:no`
  6.86 → 6.81 M, both < 1 % — which is the path the pure `sweep_expired` cleanup touches, so no
  throughput regression there. The larger swings track BIND9's livelock chaos, single unaveraged
  runs, and the changed (systemd) launch environment — not the code.
- **Regenerated the README output sample from a fresh 2.7.5 `--xdp` run**, arithmetic re-verified
  to the unit (`sent == completed + lost`, `Σ rcodes == completed`, `Run time` consistent with
  both derived elapseds).

### Fixed — docs (review of README ↔ code)
- README install commands used `dnsmark-<arch>-linux-<libc>` but the release assets are named
  with the full Rust triple `dnsmark-<arch>-unknown-linux-<libc>` (a 404 that `curl -Lo` would
  save as a broken "binary"). README URLs corrected to the real asset names.
- "Honest latency" bullet reversed the actual policy — timeouts/end-of-run in-flight are
  counted as **losses**, never as latency samples (matches WHITEPAPER §4 / FINDINGS and the code).
- Feature comparison table is now explicitly **vs dnsperf** (the closed-loop tool); noted that
  kxdpgun does zero-copy AF_XDP **and** JSON, so the old blanket "❌" was wrong for it.
- Purged the dead `DNSMARK_SPORT_SPREAD=4096` from all campaign commands (the variable is
  internal and not read; the source-port spread is hard-coded to 2048).
- Removed the documented `--slo-ms` override (the flag does not exist; the SLO is fully
  auto-derived). Corrected the SLO formula in the README to `max(3 × floor, floor + 1 ms)`.
- Fixed the stale source-port description (`10000 + (counter mod 2048)`, per-worker phase
  offset — not a fixed `2048 + worker_id`), the `-c auto` floor-of-8, the NIC-rx formula
  (adds `rx_fifo_errors + rx_over_errors`), and the `§` cross-references after the doc renumber.
- **WHITEPAPER §4 + §6 and FINDINGS — in-flight table unified.** All three now describe the design
  the code implements: both datapaths (kernel-UDP `SharedInFlight`, AF_XDP `InFlight`) use ONE
  shared 65536-slot lock-free table matched by the **global DNS id**, not a per-worker /
  partitioned map — a reply on any bound RX queue matches. (§4 still claimed a per-worker
  partitioned id space; §6 already agreed with the code.) §6 also documents the **cycled** source
  port (`10000 + (counter mod 2048)`, per-worker phase) and the **two RSS regimes** (`equal 1`
  closed-loop; RETA spread with per-queue counting in firehose, accurate since v2.5.0).
- **Six stale cross-references** that survived the section renumber: `benchmarking.md §6.1` → §6
  and `cross-validation-dnsperf.md §10` → §4, in empirical-validation.md and
  server-comparison-methodology.md.
- **DSD name.** empirical-validation §1 spelled DSD out as "dynamic saturation detection"; it is
  **Dichotomic Saturation Discovery** everywhere else. Fixed.
- **`benchmarks/scripts/bench_compare.sh` parameterized.** `SERVER` / `SERVER_SSH` / `CORPUS` /
  `DNSMARK` are now environment-overridable with generic defaults so the script runs per
  methodology §7; removed the hard-coded personal host and SSH key.

## [2.7.4] - 2026-07-02

### Fixed — multi-NIC `--ramp` breakdown shows the per-link knee, not the ramp-up average
In multi-NIC `--ramp` the Per-NIC breakdown printed `qps=` = each stack's whole-run average
(which includes the ramp-up and sits far below the knee, e.g. ~4.4 M/s while the link's DSD
`Capacity` was ~9.9 M/s). It now prints each link's NIC-verified `Capacity` (the DSD knee),
and the run reports an aggregate `Capacity (ramp knee)` = the sum of the per-link knees
(e.g. two 10 G NICs → 10.69 M + 9.86 M = ~20.5 M/s). New snapshot field `ramp_capacity`
(the exported peak-served) drives both the per-link line and the aggregate; non-ramp output
is unchanged.

## [2.7.3] - 2026-07-02

### Fixed — honest kernel-UDP (`--ramp`/auto-DSD) capacity label
In kernel-UDP the ramp is a **gated closed loop** (dnsperf-comparable, latency-honest).
The generator's kernel receive path drops replies under load, which clogs the outstanding
slots and caps the OFFERED rate well below the server's real capacity — so the DSD figure
is the closed-loop SLO knee, NOT the server's raw ceiling. Yet it was printed as
`Capacity: … (NIC-verified — max replies/s on the wire)`, which over-claimed "max on the
wire" (measured: kernel-UDP DSD reported ~1 M/s while the same server served ~5 M/s in an
open-loop flood). Now, in kernel-UDP mode, the ramp summary:

- labels the figure `Capacity: … (closed-loop knee — kernel-recv bound, NOT the server's raw max)`,
- and points to the open-loop command for the raw server ceiling:
  `dnsmark -s <ip> -Q 0 --max-outstanding 0` (which reports `Server throughput (NIC rx)` =
  `server_rx_qps`, the authoritative reply rate).

The `--xdp` ramp is unaffected: it is an open-loop firehose with a lossless zero-copy RX,
so its `Capacity` genuinely is the max replies/s on the wire. No datapath logic changed —
this is a labelling/guidance fix so the kernel-UDP number is not mistaken for the ceiling.

## [2.7.2] - 2026-07-02

### Fixed — line-rate verdict no longer shown in `--ramp` (it was misleading there)
The v2.7.1 line-rate line is computed from `server_rx_qps`, which in `--ramp` mode spans
the entire ramp-up (its average sits far below the peak). It therefore printed a low
"% of line rate / link-headroom" that CONTRADICTED the DSD's own `Capacity: … (NIC-verified
— max replies/s on the wire)` summary. The line-rate verdict is now emitted only for
fixed/flood runs (where `server_rx_qps` reflects a single steady window); in `--ramp` the
DSD `Capacity` line remains the throughput answer. No change to the fixed-load output.

## [2.7.1] - 2026-07-02

### Added — line-rate awareness (wire-bound vs server-bound), out of the box
dnsmark now states, by itself, the question that derails every manual DNS benchmark:
**is this throughput limited by the server, or by the Ethernet link?** No receiver-side
counter reading, no methodology — it is computed from dnsmark's own hardware observations:
the authoritative reply rate (`server_rx_qps`, the egress-NIC rx counter), the average
on-wire reply size (NIC `rx_bytes`/`rx_packets`), and the egress-NIC link speed.

- **% of line rate + verdict**: `wire-bound` (≥ 90 % of line rate → the link is the limit)
  vs `link-headroom` (the server or the generator is, not the wire). Shown in the text
  report and as a `line_rate` object + note in `--json`.
- Builds on the existing `server_rx_qps` NIC-counter truth, so the verdict works in both
  the AF_XDP and kernel-UDP datapaths.
- Auto warm-up default 3 s → 5 s so the reported rate is steady-state on 10 G rigs.

Measured on an X710 + X520 dual-link rig: queries (73 B) ran at ~101 % and replies
(103 B) at ~102 % of their respective line rates — both directions pinned to the wire.
A single core saturates a 10 G NIC in both directions (TX + RX-count), so a TX/RX core
split adds no throughput; the wire is the wall (PCIe x8 Gen3 ≈ 63 Gbps has 6× the margin).

## [2.7.0] - 2026-06-24

### Builtin corpus, auto-DSD, and plain-English output
Three UX changes so dnsmark works out of the box with zero configuration:

1. **Builtin corpus** (`assets/builtin_corpus.txt`): 2000 real domains from the Tranco
   top list, embedded in the binary (`include_str!`). Used automatically when no `-d` is
   provided — no corpus file to manage for a quick check.
   `--random` (random UUID subdomain queries) remains available for cache-miss workloads.

2. **Auto-DSD**: when neither `-Q` (target rate) nor `-l` (duration) is specified,
   dnsmark automatically runs the Dichotomic Saturation Discovery (`--ramp`) instead of
   an unlimited 30 s flood. The most useful default: it discovers the server's saturation
   knee and reports it directly. Override with `-Q <qps>` or `-l <secs>` for fixed-load tests.

3. **Plain-English output** (ramp): the per-step table is replaced by per-step lines
   (`Ramp step: offered X q/s | served X q/s | rtt-samples X | p50 ... p95 ... p99 ...`)
   followed by a three-line summary at convergence:
   ```
     Idle latency:  0.032 ms   (floor — minimum p50 observed)
     Capacity:          11.3M  (NIC-verified — max replies/s on the wire)
     Within SLO:        11.3M  (p50 stays under 1.03 ms at this rate)
   Knee bracket (DSD bisection): [11 270 000 ; 11 620 000] q/s  (±1.5%)
   ```
   The SLO threshold is `max(3 × floor, floor + 1 ms)` — auto-computed, no manual threshold.

## [2.6.5] - 2026-06-24

### Ramp DSD: the bisection now actually refines the knee (was quitting with a 100k bracket)
The Dichotomic Saturation Discovery's convergence test had an absolute floor of **100 000** q/s:
`bracket ≤ max(lo/20, 100_000)`. At a ~300 k knee that floor dominated (lo/20 = 15 k), so the
bisection declared "converged" the moment the bracket was within 100 k — i.e. a ±33 % window. It
did one halving and stopped: with or without the Bisect phase you got the same coarse answer.

Floor lowered to **5 000** so the **5 % relative** tolerance (`lo/20`) drives convergence. The
bisection now narrows for real, and the per-run output prints the final bracket:

```
EXP     100k OK → 200k OK → ~308k FAIL
BISECT  299k OK → 303k FAIL → 303k → 301k
Knee bracket (DSD bisection): [300000 ; 312500] q/s  (±2.1%)
Max sustained under p50<1.07ms SLO: 299756 q/s
```

The knee is now pinned to **±2 %** (e.g. [300 k ; 312 k]) instead of a 100 k-wide guess — and it
lands exactly on dnsperf's measured plateau (~300–313 k against the same unbound).

## [2.6.4] - 2026-06-24

### kernel-UDP `--ramp` now defaults to the gated closed loop (matches dnsperf vs unbound)
Three-way ramp comparison against the **same unbound** (server on a test VM, generator on a
separate box over a 10 G X710 link — no loopback CPU contention), same 100k query set:

- **dnsperf** (reference, stepped ramp): unbound knee ≈ 300–313 k q/s, avg latency 0.09→0.82 ms.
- **dnsmark `--ramp` (kernel-UDP)** previously defaulted to the **firehose** datapath. Against a
  real resolver that floods the *kernel* RX path and inflates latency by ~20× (2.6 ms vs
  dnsperf's 0.12 ms at 200 k offered) — a generator-side artifact — so the latency-SLO DSD
  tripped early and reported a bogus **100 k** knee.

Fix: **kernel-UDP `--ramp` now defaults to the gated closed loop** (`--max-outstanding 32`),
which bounds in-flight so the RX path doesn't buffer. `--xdp --ramp` stays firehose (its
zero-copy RX drains losslessly, so latency is already honest at line rate). Re-measured:

| tool / mode | max sustained | peak | p50 @100k | p50 @200k |
|---|---|---|---|---|
| dnsperf (reference) | ~300–313 k | — | 0.092 ms | ~0.24 ms |
| dnsmark kernel-UDP (now closed-loop) | **300 k** | 327 k | 0.070 ms | 0.129 ms |
| dnsmark `--xdp` | 296 k | 296 k | 0.068 ms | 0.104 ms |

All three now agree on unbound's ceiling (~296–327 k) with step-by-step matching latencies (the
kernel-UDP ramp went from a bogus 100 k to 300 k). Override with an explicit `--max-outstanding`
(e.g. `0` for the old firehose behaviour against an XDP/line-rate server that never queues).

## [2.6.3] - 2026-06-24

### kernel-UDP closed loop rebuilt single-threaded — now matches/beats dnsperf throughput
Cross-validated against **dnsperf 2.14** on the *same* unbound (redirect responder, 8 threads),
same query file, side by side, the v2.6.1 two-thread closed loop topped out **~30 % below
dnsperf** at saturation (≈350 k vs dnsperf ≈500–520 k q/s). Root cause: a SEND thread + a RECV
thread per worker sharing an `outstanding` atomic and an in-flight table → a cache-line bounce
on every packet, plus 2 threads/worker oversubscribing the cores (and a busy-spin pacer).

That was exactly backwards from how dnsperf works. The closed loop is now a **single thread per
worker** (dnsperf's actual model): one loop that FILLS the pipe up to `--max-outstanding`
(rate-limited to the per-worker target when `-Q` is set) and then DRAINS replies with one
batched `recvmmsg`, matching each by DNS id in a **local** id-indexed slot table. Send and recv
share the same thread, so the in-flight table and the outstanding counter are plain locals — no
atomics, no second thread. Rate pacing allows bounded catch-up (like dnsperf) so offered ≈ `-Q`.

Result on the same unbound (8 vCPU VM), `-c8 -T8`, outstanding 256:
- **Saturation throughput: dnsmark 560–604 k q/s vs dnsperf 500–520 k** (was 350 k) — the gap is
  gone (dnsmark now drives the resolver slightly harder), average latency comparable
  (dnsmark ~0.41 ms vs dnsperf ~0.37 ms).
- Rate-limited steps track dnsperf (e.g. 100 k offered → 99 965 vs 99 887 q/s).

The firehose path (`--max-outstanding 0`, the ramp default) and the v2.6.2 ramp fixes are
unchanged. The old two-thread closed-loop functions were removed.

## [2.6.2] - 2026-06-24

### Ramp: honest p50/p95/p99 in BOTH --xdp and kernel-UDP, with an external query set
The `--ramp` Dichotomic Saturation Discovery (DSD: exponential bracket → bisection) now produces
clean per-step **p50 / p95 / p99** in kernel-UDP (noxdp) as well as `--xdp`, and was validated
driving the **`-d` external query file** (the Runbound `top-100000-resolving` corpus, one
name/line → A). Three fixes, all measured against Runbound (XDP server) over a 10 G X710 link:

- **Pre-ramp prime** (`DNSMARK_RAMP_PRIME`, default 5 s; 0 = off). Warms ARP/switch-FDB **and the
  server cache** over the whole query set before step 1. Without it the low-rate EXP steps run
  first while the cache is cold — every name a miss → slow/dropped — so they showed seconds-scale
  tails and heavy loss that vanished only once the cache filled at the later, higher steps. With
  a warm server the whole curve is honest (e.g. noxdp sustained jumped from an apparent ~0.77 M
  cache-cold to **2.85 M q/s @ p50 0.98 ms**).
- **Warm-up at the step's target rate**, not a `qps=0` flood. The old flood built a deep in-flight
  backlog whose late replies landed in the measured window and poisoned its tail percentiles.
- **In-flight slot hygiene in the kernel-UDP RX** (sweep + reject). At low *per-worker* rates the
  16-bit DNS-id space wraps only every several seconds, so the slot of a lost / pre-window query
  lingers and a late or duplicate reply aliases it — recording a fictitious multi-hundred-ms RTT.
  The RX now sweeps stale slots (ramp horizon capped at 200 ms — nothing legitimate is in flight
  that long against a primed server, and the DSD stops well before) and rejects over-horizon
  matches as losses, not latency. Loss accounting (sent − completed) is unchanged. The same
  reject-past-timeout guard was added to the steady `-Q` closed-loop RX.

Validated: noxdp ramp p50 0.30–0.45 ms / p95 0.5–0.6 ms / p99 0.9–1.4 ms across the sustained
steps; `--xdp` ramp reaches the server's **9.97 M q/s** ceiling with p50 0.3 ms / p95 0.4 ms /
p99 0.5 ms. DSD converges (no oscillation) in both modes.

### Notes
- Ramp defaults to the **firehose** datapath (its p50 rises cleanly at the knee, so the DSD finds
  it, and it reaches the generator's real packet ceiling). An explicit `--max-outstanding N` opts
  the ramp into the gated closed loop (added a ramp-aware, batched, sleep-paced sender), which
  gives a tighter tail but is generator-bound and does not surface a latency knee for the DSD.

## [2.6.1] - 2026-06-24

### Changed — kernel-UDP closed-loop rebuilt on a dnsperf-modelled two-thread design
The closed-loop (latency) kernel-UDP path (`--max-outstanding > 0`) is now a **SEND thread +
RECV thread per worker**, sharing a qid-indexed in-flight table (the DNS id is the slot index
→ O(1) match) and an `outstanding` atomic gate. This is an independent, clean-room
reimplementation of the architecture DNS-OARC **dnsperf** uses (NOT a copy of its code): the
SEND thread paces to `qps_per_worker` (q_step = 1/qps) and blocks when `outstanding == max`;
the RECV thread frees slots and timestamps replies promptly on its own core (`recvmmsg`
batch), so per-query RTT stays accurate under load. `--max-outstanding`/`-Q` keep their
per-worker semantics. The old single-thread `unified_udp_worker` is retained (dead-code) for
reference.

Cross-validated against dnsperf on unbound 1.22.0 (matched closed-loop, same corpus):
throughput is within ~1–5% at every offered rate, and **latency is lower** (dedicated RX
thread + recvmmsg): at 300k offered, dnsmark p50 0.31 ms / avg 1.10 ms vs dnsperf avg 6.1 ms
(its single per-thread recv saturates). The firehose/ramp path is unchanged.

## [2.6.0] - 2026-06-24

### Changed — `--ramp` throughput is now read at the NIC hardware counter (concords with dnsperf)
The 2.5.x ramp rework measured throughput from the userspace counters, and neither was
correct on both datapaths: `sent`/offered runs away on a soft server (it reports the
generator's TX ceiling, not the server — measured 2.2M offered while the server served
~300k), and the XSK `completed` count under-drains under XDP load (under-reports). The
ramp now measures **served throughput at the NIC hardware counter** (`ethtool -S
rx_unicast` — the replies physically counted *in the card*, including XDP-redirected
frames that the netdev `/sys` counter misses on a bridged path). This is datapath-
independent: no open-loop runaway, no software under-count.

- **Dichotomic Saturation Discovery is retained** (exponential doubling + bisection), and
  the **SLO is auto-calculated from the measured latency floor** (`max(3×floor, floor+1ms)`)
  — never the hardcoded 1ms (which read 0 on any server/path whose floor exceeds 1ms).
- Each ramp step prints **offered + served + p50/p95/p99**; the summary reports **Peak
  server throughput (served, NIC HW)** = the server's ceiling, plus the **latency-bounded**
  sustained rate under the auto SLO.
- Validated vs unbound 1.22.0: XDP ramp **Peak served 330k** = dnsperf max **321k** (~3%,
  run variance) — concordant; the per-step `served` column tracks the server's saturation
  (offered 400k → served 316k) while `offered` keeps climbing. The kernel-UDP (`noxdp`)
  path benefits from the same NIC-HW source; further noxdp validation is pending.
- Removed the experimental served-plateau/`--slo-ms`/peak-from-burst code paths from 2.5.x.

## [2.5.8] - 2026-06-23

### Added
- **`--wire-latency` — reference latency via kernel SO_TIMESTAMPING.** A new mode that
  measures the **wire** round-trip (server + network) with the generator's userspace/socket
  overhead **excluded**, by reading kernel TX+RX timestamps (raw-hardware when the NIC
  provides them for the flow, else software/driver-level — i40e only HW-stamps PTP, so DNS
  uses the software driver stamp). It is a serial ping-pong at `-Q` rate, so it also avoids
  the open-loop / deep-outstanding queuing that inflates throughput-mode latency. This is
  the honest "what is the server+network latency" figure (whitepaper §7's wire anchor),
  now built in — no external tcpdump needed. Validated: vs Runbound on a direct 10 GbE link
  it reads **p50 27 µs** (vs 31 µs for the userspace serial RTT — the ~4 µs is the emitter
  userspace path it removes); the absolute server-only term still needs a capture on the
  server (e.g. unbound measured 22 µs server-side). New module `transport/wire_latency.rs`,
  isolated — it does not touch the throughput/ramp datapaths.

## [2.5.7] - 2026-06-23

### Changed
- **Ramp EXP phase now finds the throughput ceiling (served plateau), then Dichotomic
  Saturation Discovery bisects the latency knee — both kept.** Before, the EXP phase
  stopped at the first step that broke the latency SLO; on a low-latency datapath (XDP) the
  tight auto-SLO tripped well below the server's real ceiling, so the reported Peak
  under-read it (XDP 310 k vs the 330–343 k the server actually served). EXP now doubles
  until **served** stops climbing (the true ceiling = Peak), and the latency knee it
  bracketed along the way is refined by the existing **binary search** (the Dichotomic
  Saturation Discovery is unchanged in spirit — only its bracket source moved from
  "first SLO breach" to "throughput plateau"). Re-validated on unbound 1.22.0: noxdp
  Peak/knee **321 k**, xdp Peak **333 k** / knee 324 k — both matching dnsperf (~330 k);
  Runbound `xdp:yes` unchanged at 11.2 M.

## [2.5.6] - 2026-06-23

### Fixed
- **`--ramp` "Peak server throughput" is now read from the paced steps, not the firehose
  burst.** The per-step 1 s burst (qps=0) over-floods a soft server (a kernel/VM resolver
  livelocks; an AF_XDP-redirected reply path isn't counted by netdev `rx_packets` on every
  NIC) — so the burst-based peak read garbage against unbound (4 k while it served ~310 k).
  The peak is now the **max served (completed) over the paced ramp steps**, correct in both
  datapaths (XSK count in XDP, recvmmsg in kernel). Re-validated on unbound 1.22.0: noxdp
  ramp Peak/knee **343 k** (SLO auto 6.8 ms); xdp ramp Peak **310 k** / knee 299 k (SLO auto
  1.09 ms) — both matching dnsperf (~330 k); Runbound `xdp:yes` knee unchanged at 11.2 M.

## [2.5.5] - 2026-06-23

### Changed (the `--ramp` SLO is now auto-calculated — never hardcoded)
- **The ramp's saturation SLO is derived from the measured latency floor, not a fixed
  1 ms.** A hardcoded 1 ms is wrong the moment a real path (a couple of switches + a router,
  or a kernel/VM resolver) puts the baseline RTT above 1 ms — every step "fails" and the
  knee reads 0 (as it did against unbound, floor ~2.5 ms). The ramp now records the lowest
  p50 it sees (the floor) and sets the SLO to `max(3 × floor, floor + 1 ms)` — relative,
  auto-adapting to any server/network. It reduces to ~1 ms on an AF_XDP fast path (floor
  ~0.03 ms, the proven value) and scales up on its own elsewhere. `--slo-ms` is now an
  optional **absolute override** (default = auto).
- **The knee is reported as SERVED throughput, not offered.** The generator paces TX
  open-loop, so `offered` outruns a soft server (measured: 2.2 M offered at sub-SLO p50
  while the server served 318 k). The ramp now reports the max **served** (completed) at
  the steps that held the SLO, which caps at the server's real ceiling.

Validated on the same rig: unbound 1.22.0 (floor 2.5 ms → SLO 7.4 ms auto) → knee **334 k
served** (= dnsperf ~310–340 k, = Peak NIC-rx 336 k); Runbound `xdp: yes` (floor 0.03 ms →
SLO 1.03 ms auto) → knee **11.19 M** (unchanged from the hardcoded-1 ms behaviour).

## [2.5.4] - 2026-06-23

### Fixed (docs/accuracy)
- **Corrected `--max-outstanding` semantics in help + whitepaper.** It is **per worker**,
  not a global cap; total in flight ≈ value × clients (`-c`). The previous text claimed it
  matched dnsperf's `-q` "exactly" — but `-q` is a **total** cap. To reproduce `dnsperf -q N`
  use `--max-outstanding N/clients`.

### Validated
- **Cross-validated against dnsperf on unbound 1.22.0** (swept, bounded closed-loop, same
  server/corpus): dnsperf `-c8 -q{1000,4000}` and dnsmark noxdp `-c8 --max-outstanding
  {100,500}` **both read ~308–323 k**, and bounded dnsmark `--xdp` reads the same 308 k
  (p50 0.91 ms) — all three converge on unbound's VM ceiling. dnsmark is not slower than
  dnsperf (a first single-run 382 k/349 k pair was VM variance). Documented in
  `docs/cross-validation-dnsperf.md` §7, with the real trap: open-loop **firehose** collapses
  a soft/kernel/VM server (livelock or virtio/bridge can't absorb line rate) — bound the load
  (closed-loop) for such a server; firehose is for an AF_XDP server that can take line rate.

## [2.5.3] - 2026-06-23

### Fixed
- **Multi-NIC `--ramp` now ramps every link and reports an aggregate.** The shared
  shutdown let the first link to saturate kill the others before they finished, so only
  one link's summary printed and there was no total. Each NIC now owns its shutdown and
  ramps to its own saturation independently; `run_multi_nic` then prints per-link Peak +
  SLO lines and a final **`Aggregate across N NICs`** summing the per-link maxima and
  knees. Dual-NIC XDP 100k: per-link ~8.7M + ~6.4M peak → **aggregate ~15.1M served peak**,
  SLO knee sum ~18.3M offered. (`StatsSnapshot` gains `ramp_peak_served_qps` /
  `ramp_slo_knee_qps`, summed in the multi-NIC merge.)

## [2.5.2] - 2026-06-23

### Added
- **`--ramp` now reports the raw maximum speed, not just the SLO knee.** Each ramp step
  already floods for 1 s (qps=0) to probe the ceiling, but that rate was discarded — the
  run only printed "Max offered under p50<1ms SLO". The ramp now reads the return NIC's
  `rx_packets + rx_missed_errors` across every burst and prints **`Peak server throughput
  (NIC rx)`** = the max replies/s the server actually emitted at saturation (per NIC in
  multi-NIC). So one `--ramp` run gives both the raw max and the latency-bounded knee.
  Single-NIC XDP 100k: peak ~10.2M served, p50<1ms knee ~11.2M offered.

## [2.5.1] - 2026-06-23

### Fixed
- **`--ramp` lost its latency in XDP (regression from 2.5.0).** The count-only firehose RX
  recorded no RTT, but `--ramp` also runs at `max_outstanding==0`, so the p50 SLO gate saw
  zero samples (p50 0.000) and never found the saturation knee. The unified worker now
  records a **1/64 RTT sample** in ramp mode (shared in-flight table, `record_latency_us`) —
  enough for p50/p95/p99 at Mpps, negligible cost; throughput is still counted per-queue.
  Single-NIC XDP 100k ramp now finds a **p50<1ms knee at ~11.2M qps**.
- **Multi-NIC `--ramp` collapsed at ~1.25M.** The unified datapath stored one global config,
  so both NICs' workers read the first NIC's `qps_per_worker` and the two per-NIC ramp
  controllers fought. Configs are now keyed by interface, so each stack paces from its own
  ramp controller. Dual-NIC XDP 100k ramp: each link holds sub-ms p50 to ~6.4M (p50 0.30 /
  p95 0.43 ms) and reports its own max-offered-under-SLO.

## [2.5.0] - 2026-06-23

### Fixed
- **Round-trip under-counted a fast server by ~14× (#15-P1).** In `--xdp` the unified
  workers steered all RSS responses onto q0 (`ethtool -X equal 1`) and matched per-worker,
  so one TX-busy worker drained ~350k resp/s while millions arrived — every healthy XDP
  server looked broken (~3% success; round-trip 335k vs 11.24M served). The firehose RX now
  spreads the RETA across **all** worker queues and counts responses per queue by rcode (no
  cross-core match, no shared per-packet state), so the TX hot path stays at line rate and
  round-trip tracks the server's real reply rate. Closed-loop now shares one lock-free
  in-flight table + a global backpressure counter (warmup 950k → 2.77M). Validated at the
  NIC: **335k → 11.19M** (single-NIC) vs 11.24M truth (0.5%).
- **Multi-NIC NUMA pinning (#15-P2).** Worker cores were assigned through one shared global
  cursor that assumed a single-node generator, so the 2nd NIC's stack landed on the remote
  node (QPI-bound) — dual-fibre capped ~17.7M. Each NIC now pins strictly to **its own**
  NUMA-local cores via a per-node cursor (disjoint pools across nodes; shared within a node).
  Dual-NIC XDP: **17.7M → 21.7M** wire (X710 ~10.7M + X520 ~10M).

### Added
- **`Server throughput (NIC rx)` — authoritative reply rate.** dnsmark reads the return
  NIC(s)' `rx_packets + rx_missed_errors` over the measurement window and reports it as the
  truth, in **both** kernel-UDP and XDP. In kernel mode the NIC ring overflows at multi-Mpps
  and the socket drops replies, so the userspace round-trip under-counts; adding the
  ring-overflow counters recovers the server's true reply rate (= its tx counter) without
  reading the remote host. When round-trip < NIC-rx, a NOTE explains the loss is
  generator-side (kernel socket / NIC ring / non-NUMA-local stack), not the server.
- **Auto-NUMA (single-NIC).** The process is confined to the NIC's NUMA node — CPUs
  (`sched_setaffinity`) and memory (`set_mempolicy` MPOL_BIND) — at startup, the equivalent
  of `numactl --cpunodebind=N --membind=N`, automatically. Kernel-UDP single-NIC egress
  4.82M → 5.05M with no numactl (the rig's kernel ceiling).

### Changed
- **#16 auto-config.** `--max-outstanding` now defaults by mode: `--xdp` ⇒ `0`
  (firehose/throughput), kernel-UDP ⇒ `100` (closed-loop, dnsperf-like), `--ramp` ⇒ `0`.
  `dnsmark --xdp -s <ip> -d <corpus>` floods out-of-the-box (was silently throttled to 100
  outstanding = closed-loop). `DNSMARK_SPORT_SPREAD` is internal and no longer read.

## [2.4.0] - 2026-06-18

### Fixed
- **`--ramp` self-cap on kernel-UDP — the offered load now reaches the server's real knee (#14).**
  The 2.3.0 ramp work let the throughput worker rate-pace and sample RTT, but the worker still
  drained RX **in the TX thread**: at a high offered rate the per-iteration `recvmmsg` + in-flight
  bookkeeping capped TX at ~440k, so the dichotomy concluded "max sustainable ~440k" and never
  reached a fast server's knee (the same generator floods 5.9M kernel-UDP).

  Fix: the kernel-UDP throughput path (flood **and** ramp) is now split into a **TX thread** that
  floods/paces and records send times into a lock-free `SharedInFlight` (one `AtomicU64` slot per
  16-bit DNS id — the id is the index, so no lock and no collision), and a **dedicated RX thread**
  that drains via `poll()`+`recvmmsg`, matches ids and records RTT + completions. A single shared
  clock keeps `RTT = recv - send` exact.

  Verified on X710 / i40e vs a kernel-slow-path server: `--ramp` NIC tx peaked at **2.37M** (was
  442k) and found a real **p50<1ms knee at 1.3M**; p50 rises correctly with load and the SLO breaks
  at saturation. Flood egress unchanged at 5.82M; the closed-loop latency path
  (`--max-outstanding > 0`) is bit-for-bit unchanged.

### Notes
- The AF_XDP `--ramp` was already correct and is unchanged — verified end-to-end to a **9.36M
  sub-ms knee** against a fast XDP server. Its ring-based RX drain is cheap enough to run in the
  unified worker, so the TX/RX split is not needed there.
- The AF_XDP **flood** self-report under-count is **by design** (#5): the unified TX+RX-per-queue
  worker with single-queue (q0) RSS concentration is exactly what lets the XDP `--ramp` reach the
  real knee at sub-ms p50. In flood, read served throughput from the receiver NIC counters (the
  methodology already does); `--ramp` self-report is accurate.

## [2.3.0] - 2026-06-13

### Fixed
- **`--ramp` now works reliably in BOTH transports.** The saturation ramp (Dichotomic
  Saturation Discovery) was effectively unusable:
  - **kernel-UDP**: `--ramp` ran on the throughput worker, which has no per-query latency,
    so the p50 SLO never tripped and the ramp just flooded (no knee).
  - **AF_XDP**: the RSS RETA was steered across all bound queues (`equal queue_count`), but the
    unified RX+TX workers — busy on TX — polled each thinly-filled RX queue rarely, so matched
    replies sat ~10 ms (a measurement artefact) and the dichotomy never found the real knee.

  Fixes:
  - kernel-UDP: the throughput worker now rate-paces to the ramp target **and** samples RTT
    into the histogram (per-worker InFlight keyed by DNS id) — it drives load on the fast batched
    path while measuring latency, so the dichotomy finds the server's real knee.
  - AF_XDP: RSS is steered to a **single** RX queue (`ethtool -X equal 1`) so it is drained
    continuously; TX still spreads across all bound queues.

  Verified end-to-end on a dual-Xeon-v2 / X710 bench: kernel-UDP ramp ~3.8 M qps, AF_XDP ramp
  ~11.0 M qps (the server's real saturation knee), vs 0 / a 10 ms artefact before.

### Notes
- `--ramp` stays an opt-in flag; the default remains the closed-loop latency probe
  (`--max-outstanding 100`). Throughput (flood) and closed-loop latency are unchanged.

## [2.2.3] - 2026-06-13

### Fixed
- **No-xdp latency mode — the default invocation — was ~512× too slow.** The closed-loop
  outstanding gate on the kernel-UDP datapath checked a single **shared** `global_in_flight`
  atomic, so `--max-outstanding 100` (the default) was split across all *N* workers: only
  ~`100/N` queries in flight per worker (~5 on a 20-worker host). `dnsmark -s <server> -d
  <queries>` with no flags therefore read **1 845 qps** against a server serving ~940 k in the
  same mode — a starved generator misreported as a slow server. The gate is now **per-worker**
  (a local counter, matching dnsperf's per-client `-q` and the AF_XDP path); the shared atomic
  is kept only as a reported statistic, never as the hot-path gate. Measured: **1 845 → 944 k
  qps**. Flood mode (`--max-outstanding 0`) and `--xdp` are unaffected.

### Changed
- WHITEPAPER §5 / §3b updated to match reality: the outstanding gate is per-worker on **both**
  datapaths; documented the kernel-UDP throughput ceiling (~5 M qps, CPU-bound on the physical
  cores — XPS/`mq` and NUMA are already optimal, and HyperThreading *lowers* the rate) and that
  AF_XDP (~13 M, near 10 GbE line rate) is the only path past the per-skb cost.
- Removed dead `InFlight::sweep` (superseded by `sweep_with_ages`).

## [2.2.2] - 2026-06-13

### Fixed
- **--xdp emitted only ~nworkers (~12) distinct flows, capping the server under test.**
  v2.2.1 routed each worker's responses correctly but still stamped one fixed UDP source port
  per worker, so query traffic carried only ~12 distinct 5-tuples - the server's RSS collapsed
  onto ~6 receive queues and the measured throughput was a generator artefact, not the server's
  ceiling. Each worker now cycles its source port over a wide range (SPORT_SPREAD=2048), fanning
  queries across the receiver's full RSS. Response matching stays per-worker (lock-free, no
  cross-core contention); in --xdp flood mode the per-query latency stats become approximate
  (responses scatter across the bound RX queues) while served throughput stays exact. Measured:
  a kernel-UDP server that read 2.4 Mqps under the 12-flow generator reads 6.5 Mqps with the
  spread; an AF_XDP server reaches the generator's ~11 Mqps single-fibre ceiling.

## [2.2.1] - 2026-06-10

### Fixed
- **`--xdp` reported ~100% loss against a healthy AF_XDP server (#8).** Query frames use a fixed UDP source port (12345), so every response shares one 5-tuple and hashes to a single RSS queue. The generator binds its AF_XDP RX on a capped subset of queues (q0..N-1), but the NIC's default RSS indirection spans all HW queues, so the single response queue was frequently outside the bound set: responses landed on an unbound queue and were dropped before the XSK (false 100% loss), which also stalled the closed-loop sender. The generator now steers the RSS indirection table to span exactly the bound queues (`ethtool -X <if> equal <queue_count>` - RETA only, no channel reconfig, safe around an active zero-copy bind). Verified: round-trip completion 99.7-99.9% with the NIC's default multi-queue RSS. Above ~9 M qps the single active RX queue (one core) saturates and depresses the reported completion rate - a generator-side limit; read served throughput from the receiver NIC counters.

## [2.2.0] - 2026-06-10

### Added
- **802.1Q VLAN support for AF_XDP (`DNSMARK_VLAN=<vid>`)** *(experimental — see
  Validation)*. With `--xdp`, one 802.1Q tag is baked into the frame template (no
  per-frame shift; the hot path stays copy+patch), an optional tag is skipped on
  RX, and the AF_XDP socket **binds the physical parent** of a VLAN sub-interface
  while reading src IP/MAC from the sub-interface. Rationale: AF_XDP zero-copy is
  unsupported on a VLAN sub-interface (`bind … : errno 95`), so generation must
  use the physical NIC and inject the tag itself. Needed for providers that
  deliver the private network tagged (e.g. Latitude). Companion to Runbound #188.
- Wire-truth PHY tx counter resolves to the physical parent when a VLAN is used.

### Validation
- Frame layout **unit-tested against the 802.1Q wire spec** (TPID/VID, inner
  EtherType, L3 shifted +4, IPv4 checksum — the offset guard). Physical-parent
  bind confirmed (XSK `ifindex` = physical NIC). The
  resolver-side round trip is proven end-to-end (`dig` over a tagged VLAN →
  `NOERROR`). **The dnsmark tagged `--xdp` data path is not yet validated at line
  rate**: the only available 100G NIC (Broadcom BCM57508, `bnxt_en`) has **no
  AF_XDP zero-copy in any Linux kernel** (verified against mainline `bnxt_xdp.c` —
  no `xsk_pool`/`XDP_ZEROCOPY`), so generation fell back to copy mode and could not
  be rate-tested. Full validation pends a zero-copy-capable NIC (Intel
  `ice`/`i40e`/`ixgbe`, Mellanox `mlx5`). I cannot confirm tagged generation at rate.

## [2.1.0] - 2026-06-07

### Added
- **Reliable throughput counter**: report SUBMITTED TX descriptors (what actually
  reaches the wire), not completion-ring entries which under-report at multi-Mpps.
- **Auto warm-up** (`DNSMARK_WARMUP`, default 3 s): reset the measurement window
  after XSK bind / ring fill / NIC ramp so reported rates are steady-state.
- **GovernorGuard**: pin every CPU to `performance` for the run, restore on exit
  (DVFS is the #1 benchmark confounder).
- **Huge-page (2 MiB) UMEM** with 4 KiB fallback — fewer dTLB misses at multi-Mpps.

### Changed / Fixed
- **AF_XDP generator no longer self-throttles** (the non-reproducible "peak then
  collapse" behaviour): cap unified workers to NIC-local PHYSICAL cores (never HT)
  instead of one-per-HW-queue, which oversubscribed the few NIC-local cores and
  overdrove the ixgbe ZC datapath. Core budget by topology: 2-socket Intel = 10
  local + 6 cross-NUMA (QPI-bound); many-node AMD = 12 per port (Infinity Fabric).
- Global cross-NIC core cursor so dual-fibre spreads across distinct cores.
- Incremental IPv4 header checksum (RFC 1071) on the hot path.

### Notes
- Frame size dominates pps: realistic short query names (corpus) reach far higher
  rates than 32-hex `--random` names (~104 B). Bench with a realistic dataset.


All notable changes to this project will be documented in this file.  
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) — [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [2.0.6] — 2026-06-05

### Fixed

- **`physical_cores()` CPU enumeration.** It now walks the real
  `/sys/devices/system/cpu/cpuN` entries — correct for any SMT width (2/4/8) and for
  sparse/high CPU ids (e.g. a cgroup cpuset) — and dedups by **(package, core)** rather
  than `core_id` alone, so on a multi-socket host the second socket's cores are no longer
  collapsed (a dual-socket box previously reported ~half its physical cores). Still
  returns exactly **one logical CPU per physical core — HT siblings are excluded** (we pin
  to real cores, never HyperThreads). Falls back to `num_cpus::get_physical()` if `/sys`
  is absent.

---

## [2.0.5] — 2026-06-05

### Added

- **Host environment capture (#6).** `--json` output now includes a `host` object — CPU
  model, physical cores / logical threads, NUMA nodes, total memory, and the egress NIC
  toward the target (interface, driver, link speed, NUMA node) — and a one-line host
  banner is printed at startup. A `notes` field flags a result that may be bounded by the
  **receiver's** NIC/bus rather than the server software (high loss → read the receiver's
  NIC counters; see docs/benchmarking.md §3). Generator-side capture; receiver-side is out
  of scope (no remote hook).

---

## [2.0.4] — 2026-06-05

Hardening and measurement-correctness follow-ups to 2.0.0, each backed by a bench or a
microbench and cross-checked against a `tcpdump` wire capture (see docs/WHITEPAPER.md).

### Fixed

- **A timeout is a loss, not a completion** (2.0.3). Timed-out / evicted / end-of-run
  in-flight queries count toward `queries_lost`, never `queries_completed` or the latency
  histogram. So `queries_completed` = real responses, `queries_lost` = timeouts + send
  errors, `sent == completed + lost`, and the latency tail is pure response latencies.
- **Explicit in-flight eviction accounting** (2.0.1). In flood mode, a hash collision in
  a per-worker in-flight table is detected and counted as a loss, so the accounting
  identity holds exactly even when the table overflows.

### Changed

- **Hot-path query copy uses `copy_from_slice`** (2.0.2). The hand-rolled AVX2/SSE2
  memcpy was measured no faster than the standard copy at 30–60 B and was removed —
  simpler, `unsafe`-free. No SIMD speedup is claimed (see WHITEPAPER §10).
- **Multi-NIC aggregate percentiles report the worst NIC's value**, not a weighted
  average — averaging percentiles is invalid and could hide a slow NIC; a max surfaces
  it. `--nic-stats` gives per-NIC percentiles. Mean / min / max / throughput are exact.

### Added

- IPv6 targets log a one-time warning that NUMA-local pinning is skipped (the route
  lookup is IPv4-only) instead of degrading silently.
- **Technical whitepaper** (docs/WHITEPAPER.md) and a **wire-validated latency methodology**
  (docs/benchmarking.md §7): a generator's reported RTT = server + network + the tool's own
  client-side overhead, so absolute latency is anchored on a `tcpdump` capture, not on
  another tool. Default transport is UDP (comparable to dnsperf); `--xdp` is opt-in and
  symmetric (XDP-vs-XDP only).

---

## [2.0.0] — 2026-06-05

### Changed

- **UDP kernel socket is now the default transport; AF_XDP is opt-in via `--xdp`.** dnsmark no longer auto-enables XDP. The generator's datapath must match the server's: use the default UDP path against kernel servers (unbound, BIND, Runbound kernel path) for latency comparable to dnsperf, and `--xdp` only against AF_XDP servers (symmetric XDP-vs-XDP) or for saturation throughput. Mixing transports across a comparison is rejected as non-publishable.
- **Unified UDP worker** — send and receive now run in the *same* OS thread per worker (loop modelled on dnsperf: send → `poll` until next-send-or-response → `recvmmsg` drain → timeout sweep). Removes the previous sender/receiver thread split that added ~34 µs of context-switch latency to every measured RTT.

### Fixed

- **Honest latency tail** — queries expired by the timeout sweep, and queries still in flight at end-of-run, are now recorded in the latency histogram (at their real age) instead of being silently dropped. p99/p999 no longer hide the slowest responses.
- **Per-worker rate calibration** — target QPS is divided by the number of *actually spawned* workers (one per NIC RX queue), not by `--clients`. Previously a low-queue NIC under-drove the target (e.g. 50k → 6.25k on a single-queue interface).
- Removed `SCHED_FIFO` real-time scheduling on worker threads — on bare metal it starved per-core kernel softirqs and could take down host networking. Workers run `SCHED_OTHER`, pinned to NIC-local physical cores.

### Validation

- Latency cross-checked against `tcpdump` wire capture on two servers (Unbound, BIND): dnsmark's generator overhead is ~constant (~45 µs) and ~25 µs lower than dnsperf's, i.e. closer to the wire truth. Both tools rank servers identically.

---

## [1.3.0] — 2026-06-03

### Added

- **Multi-NIC AF_XDP flood** — generate across N independent NICs in parallel to aggregate beyond a single 10 GbE link. AF_XDP does not support bonding, so each NIC is an **independent XSK interface** with its own rings, workers, and target. Specify multiple targets with repeated `-s` (e.g. `-s 10.10.10.2 -s 10.10.20.2`), each on a distinct subnet routed via a distinct NIC; workers are split across NICs. `--nic-stats` prints a per-NIC throughput breakdown alongside the aggregate. A single `-s` is unchanged (mono-NIC). A NIC that fails XDP attach warns and is skipped without taking down the others.
  - Measured: **19.4M qps aggregate** across two X520 10 GbE fibre links (8.6M + 10.8M) — past the single-link ~11.3M line-rate.
- **Benchmarking methodology guide** (`docs/benchmarking.md`) — how to measure a receiver's true throughput at its NIC counters (not the generator's round-trip, which under-counts when the receiver out-paces the generator's RX), NIC/host tuning (flow control, RSS, governor), the 10 GbE line-rate ceiling, and gotchas (silent TX fallback < 1.2.1, corrupted-NIC reset, the poll-model myth).

---

## [1.0.0] — 2026-05-26

First stable release.

### Added
- **SIMD memcpy dispatch** — `AVX2` (32 B/iter) on Haswell+ / Threadripper, `SSE2` (16 B/iter) on Xeon E5 v2 baseline. Detected once at boot via CPUID, cached in `OnceLock`. Logged at startup: `[dnsmark] CPU SIMD: SSE4.2 | sse4.2=true avx2=false avx512f=false`.
- **Zero-allocation hot path** — pre-built wire-format query pool (`WireQueryPool`), stack-allocated `iovecs[256]` + `mmsghdr[256]` in `sendmmsg_pre_alloc()`, stack response buffer in receiver thread. No heap allocation on the send/receive hot path.
- **Static binaries** — `x86_64-linux-musl` and `aarch64-linux-musl` in GitHub releases. Drop-anywhere, no runtime dependencies.

### Changed
- `BATCH_SIZE` 64 → 256 — 4× fewer `sendmmsg(2)` syscalls at peak QPS.
- `RECV_BATCH` 16 → 64 — matches larger send batch size.
- `SO_SNDBUF` / `SO_RCVBUF` tuned to 8 MB per socket (requires `net.core.wmem_max` / `rmem_max` ≥ 8 MB on the OS).

---

## [0.4.5] — 2026-05-19

### Fixed
- XDP interface selected via `getifaddrs()` on server subnet — eliminates wrong-interface selection on Proxmox hosts where a bridge and a veth share the same `/24`.
- Virtual interface detection with automatic parent resolution — if the selected interface is a bridge / veth / ipvlan / macvlan, XDP retries on the physical parent; falls back to recvmmsg if no parent is found.

---

## [0.4.4] — 2026-05-18

### Added
- AF/XDP receive path (`--features xdp`, enabled by default) — DNS responses captured at NIC driver level via eBPF, bypassing the kernel network stack. Automatic fallback to recvmmsg on unsupported hardware.
- `--no-xdp` — disable AF/XDP at runtime.
- `--xdp` — force AF/XDP, error if unavailable.

---

## [0.4.3] — 2026-05-18

### Fixed
- `--max-outstanding` no longer stalls the sender: skips the slot instead of sleeping 500 µs when the global in-flight cap is reached.

---

## [0.4.2] — 2026-05-18

### Changed
- `--max-outstanding` is now a global limit across all workers (matches `dnsperf -q` semantics).

---

## [0.4.1] — 2026-05-18

### Added
- `--max-outstanding <N>` (default 100) — caps total in-flight queries across all workers.

---

## [0.4.0] — 2026-05-18

### Added
- Dedicated sender + receiver OS threads per UDP worker — sender and receiver are fully decoupled, eliminating cross-thread contention on the hot path.

---

## [0.3.x] — 2026-05-17

- `-c auto` minimum 8 workers, drift-compensating rate limiter.
- Global shared in-flight counter (`Arc<AtomicUsize>`).
- `--max-outstanding` initial implementation (per-worker).
- Non-blocking sender — removes 500 µs sleep on cap hit.
