# dnsmark — Architecture & Measurement Methodology

*A technical whitepaper on how dnsmark generates load and measures latency.*
*Every mechanism below is described from the source; file references are given so
any claim can be checked against the code.*

---

## 1. What dnsmark is, and what it optimises for

dnsmark is a **closed-loop** DNS load generator: it sends a query, waits for the
response, measures the round-trip, and paces itself to a target query rate. It is
built around three goals, in order:

1. **Honest measurement** — a latency number must be reproducible and decomposable,
   never flattering. The slow tail must be counted, not dropped.
2. **Low generator overhead** — the tool should add as little of *its own* latency to
   the measurement as possible (see §7 for why this matters and how it is bounded).
3. **Headroom** — when you need to saturate a fast server, an optional AF_XDP datapath
   removes the kernel from the send/receive path entirely.

It speaks **UDP (default), TCP, DoT**, and — opt-in — **AF_XDP**. The transport is
chosen explicitly; dnsmark never silently changes datapath
(`engine/mod.rs`, `use_xdp = config.force_xdp && protocol == Udp`).

---

## 2. Worker pool

A run spawns **N worker threads**, each pinned to a CPU core
(`tokio::task::spawn_blocking` + `pin_to_cpu(worker_id)`). Workers share nothing on
the hot path except one atomic counter for the global outstanding gate (kernel paths;
the AF_XDP unified path shares nothing at all — see §5); each owns its own socket, its
own in-flight table, and its own send/receive loop.

- For UDP/TCP/DoT, N = `--clients` (`-c`).
- For AF_XDP, N is **auto-detected** from the NIC and **capped to NIC-local physical
  cores** (v2.1.0): N = min(RX queue count, per-NIC core budget). Binding one XSK per
  HW queue oversubscribes the few NIC-local cores and collapses throughput (the pre-2.1.0
  "peak then collapse"): many more workers than NIC-local cores contend and read *far
  less* than a worker count matched to the cores. The budget is topology-dependent: on a
  2-socket Intel the NIC-local physical cores plus at most 6 cross-NUMA cores (the
  inter-socket QPI saturates beyond that); on many-node AMD, 12 per port. Workers are pinned to
  **physical** cores (`autodetect.rs::physical_cores_numa_sorted`), NIC-local node
  first, never an HT sibling; a global cross-NIC cursor gives a second NIC distinct
  cores, and when the core pool is spent no further workers spawn
  (`transport/xdp/receiver.rs`).

The target rate is divided by the number of **actually spawned** workers, not by
`--clients`, so a low-queue NIC still drives the full target
(`qps_per_worker = total_qps / N_spawned`).

---

## 3. The default datapath — the unified UDP worker

The core of v2.0.0 is a **single-threaded** send-and-receive loop, one per worker
(`transport/udp.rs::unified_udp_worker`). Send and receive happen in the *same* thread
on the *same* clock, so an RTT is measured start-to-finish with no inter-thread
hand-off. The loop, each iteration:

```
1. SEND (if a slot is free)
     timestamp = clock.now()        ← taken BEFORE send(), the conventional timestamp point
     send(fd, query, MSG_DONTWAIT)
     in_flight.insert(id, timestamp)
     in_flight_count += 1           (per-worker counter — the § 5 gate)
     advance next_send by send_interval   (no burst catch-up after a stall)

2. WAIT  poll(fd, POLLIN, µs_until_next_send)
     wakes immediately on a response, or at the next send deadline — never overshoots.
     For sub-millisecond intervals it busy-spins with a non-blocking peek instead
     (poll() has only ms resolution).

3. DRAIN recvmmsg(fd, …, 64, MSG_DONTWAIT)        ← up to 64 responses per syscall
     for each response:
       timestamp = clock.now()
       rtt = timestamp − in_flight.take(id)
       histogram.record(rtt); in_flight_count −= 1

4. SWEEP (every 10 ms) expire in-flight entries older than the timeout
```

Why this shape:

- **One thread, one clock.** The previous (pre-2.0) design split sending and receiving
  across two threads; the hand-off added a context switch (~34 µs) to every measured
  RTT. Unifying the loop removes it.
- **`poll` with a deadline of "time until next send"** means the worker sleeps exactly
  as long as it should: it wakes the instant a response arrives (low latency) but also
  in time to send the next query (accurate rate). It never blocks past a send deadline.
- **`recvmmsg` batches** up to 64 datagrams per syscall — the receive path is cheap, so
  it contributes little of the generator's own overhead.
- Send and receive sockets carry **8 MB** SO_SNDBUF/SO_RCVBUF so bursts are not dropped
  by the kernel before the loop drains them.

---

## 3b. The throughput datapath (saturation mode)

The unified loop above is **latency-accurate**, but it pays for that accuracy: every
iteration is ~3 syscalls (`sendmsg` + `poll` + `recvmmsg`) plus per-query bookkeeping
(timestamp, histogram, the outstanding atomic). That is the right trade-off when you
*measure* latency — but it caps the raw send rate.

When all you need is **maximum offered load** — `--max-outstanding 0` (saturation /
flood) — dnsmark switches to a dedicated **throughput worker**
(`transport/udp.rs::throughput_udp_worker`) that strips everything not needed to
bombard:

- **`sendmmsg`, batch 64** — one syscall pushes 64 queries instead of one `send` per
  packet, amortising the syscall-entry cost (the dominant per-packet overhead in
  kernel mode).
- **bulk `recvmmsg`, drained periodically** — not after every packet; responses are
  counted for loss accounting, never timed per query.
- **no `poll`** per packet — no latency-precise wake-up is needed when flooding.
- **no shared atomic on the hot path** — the outstanding gate is off in unlimited
  mode, so each worker keeps a purely local counter and flushes in batches: zero
  cross-core cache-line traffic per packet.

Measured at the CPU-cycle level (`perf`), the generator's **own** (user-space) cost
collapses — the per-packet bookkeeping is gone — leaving the loop essentially
**kernel-bound** (the irreducible UDP-stack traversal). dnsmark then sends *more* packets
at *fewer* cycles each.

In pure kernel mode the throughput path is **CPU-bound on the physical cores**, each
contributing an irreducible slice of kernel skb/UDP traversal (`sendmmsg` is non-blocking,
so the cores busy-loop at ~100 % system time). Two levers that look promising change
nothing here: the kernel already maps a TX queue per core (XPS + an `mq` qdisc by default),
and NUMA placement is within run-to-run noise (cross-socket TX costs no measurable
throughput for this workload). A third actively **lowers** the rate — spreading the workers
onto the HyperThread siblings (logical vs physical) contends the wire-build and the TX
syscall on the same physical core, which is why dnsmark pins **physical cores only**. The
single way past the per-skb cost is the AF_XDP datapath (§6): it never builds an skb, so it
scales to the wire ceiling (§7). Rule of thumb: reach for `--xdp` to saturate a fast server;
kernel mode is the portable default below the per-skb wall.

**The `--ramp` saturation search (§5b) also runs on this path**, rate-paced to the ramp's current
target QPS with per-query **RTT sampled** into the histogram. So that latency tracking never caps the
offered load, the kernel-UDP throughput path **splits TX and RX across two threads** (since 2.4.0): a
TX thread floods/paces and records each send time into a lock-free in-flight table (one `AtomicU64`
slot per 16-bit DNS id — the id is the index, so no lock and no collision), while a **dedicated RX
thread** drains responses (`poll`+`recvmmsg`), matches ids and records RTT + completions. A single
shared clock keeps `RTT = recv - send` exact. Draining RX in the TX thread (as before 2.4.0) starved
TX — the per-iteration `recvmmsg` capped the kernel-UDP ramp far below the flood rate; the split lets
the dichotomy offer up to the flood rate and find the server's real knee. The AF_XDP ramp needs
**no** split — its ring-based RX drain is cheap enough to run inside the unified worker (§6), with q0
RSS concentration keeping that one queue continuously drained. Pure flood (`--max-outstanding 0`
without `--ramp`) skips the RTT sampling for raw maximum offered load.

> **Pure flood (`--max-outstanding 0` without `--ramp`) is NOT a latency measurement.** There, send
> timestamps are per-batch, so the p50/p99 reported are throughput-mode figures, not comparable to a
> closed-loop latency figure. For an exact per-query latency comparison use `--max-outstanding > 0`
> (the closed-loop unified path of §3); `--ramp` reports a sampled latency at each offered load (§5b).

---

## 4. In-flight tracking and the latency histogram

**In-flight tracking is per-worker on the kernel path, shared on AF_XDP** — the two
datapaths reach correctness differently, so their tables differ.

- **UDP path (`transport/udp.rs`) — per-worker by construction.** Each worker binds and
  **`connect`s its own socket** to the server, so it only ever receives responses to *its
  own* queries; correctness comes from per-socket isolation, not from a globally-unique id.
  In closed-loop / latency mode (`--max-outstanding > 0`, the kernel-path default) each
  worker runs a single thread with a **thread-local**, direct-indexed `Vec<u64>` of 65 536
  slots (the 16-bit id **is** the slot index — O(1), no collisions) and a local outstanding
  counter; **no atomics, no `Arc`, nothing shared between workers**. That locality
  (`#noxdp-perf`) avoids a per-packet cache-line bounce and is what lets the kernel
  path match dnsperf's saturation throughput. In open-loop flood mode (`--max-outstanding 0`) a
  worker splits TX and RX into two threads that share a **per-worker** `SharedInFlight` (a
  65 536-slot `Box<[AtomicU64]>`) so the RX thread can complete what the TX thread sent —
  still per-worker, not shared across workers. (A kernel `--ramp` stays on the single-thread
  closed-loop path — its default gate is 32, not 0.) Every worker sweeps its own table (§3, step 4).
- **AF_XDP path (`transport/xdp/receiver.rs`, `InFlight`) — one shared table, by necessity.**
  A single `Arc`-shared 65 536-slot `Box<[AtomicU64]>` indexed directly by the **global
  16-bit DNS id**, with **no per-worker partition**: each bound RX queue has its own XSK (with
  its own UMEM and rings), and the NIC's RSS scatters replies across those queues, so a reply
  may return on *any* worker's queue and must still match its send timestamp. Only worker 0 runs the 10 ms timeout sweep
  and the end-of-run drain over the shared table (one scan, no cross-worker double-counting).
  (A legacy per-worker `unified_udp_worker` with a thread-local table also lives in `udp.rs`
  but is dead code.)

**The latency histogram.** Completed RTTs go into an HDR histogram
(`stats/mod.rs`, range **1 µs – 60 s, 3 significant figures**), from which
p50/p95/p99/p999, min, mean and max are read at the end. HDR gives constant-time
recording and bounded relative error across six orders of magnitude.

**Honest tail — slow responses count, losses are losses.** A *slow response* — one that
arrives within the timeout (default 3 s) — is recorded at its real RTT, so p99/p999
include the genuinely slow responses; the tail is never truncated by a generator that
keeps only the fast ones. A query that gets **no** response (a *timeout*) is a different
thing: it is a **loss**, not a completion and not a latency sample. The 10 ms sweep and
the end-of-run drain mark such queries as timeouts (`inc_timeout`); they move into
`queries_lost`, never into `queries_completed` or the latency histogram. This keeps three counters clean: `queries_completed` = real responses,
`queries_lost` = timeouts + send errors, and `sent == completed + lost` exactly.

**Outstanding depth** is tracked too (mean and max concurrent in-flight), and reported
in JSON — this is the closed-loop outstanding depth (the `-q`-style window).

---

## 5. Rate control and the outstanding gate

Two independent limits shape the send side:

- **Rate** — each worker holds a `send_interval = 1 / qps_per_worker`. It sends when
  `now ≥ next_send`, then advances `next_send`. After a stall it does **not** burst to
  catch up (`next_send = now + interval`), so a scheduler hiccup cannot produce a
  thundering send and a distorted tail.
- **Outstanding** — each worker bounds its **own** in-flight at `--max-outstanding`
  (kernel-UDP default 100) with a purely local counter: a per-worker closed-loop window.
  This holds on **both** datapaths (kernel-UDP and AF_XDP); there is no shared atomic on
  either hot path.

  > **`--max-outstanding` is PER WORKER, not a global cap.** dnsperf's `-q` is a **total**
  > outstanding cap; dnsmark's is multiplied by the worker count (`-c`). To reproduce a
  > dnsperf `-q N` run, set `--max-outstanding N/clients` (a swept, bounded closed-loop then
  > reads the same served rate on both tools; see `docs/cross-validation-dnsperf.md`). Setting
  > `--max-outstanding` far above the queue depth over-fills the queue and *lowers* goodput at
  > higher latency — the expected closed-loop over-pipelining, not a server change.

  > Versions up to 2.2.2 gated the kernel-UDP path on a **single shared** `global_in_flight`
  > atomic. With `--max-outstanding 100` split across *N* workers that left only ~`100/N` in
  > flight *per worker* (~5 on a 20-worker host) — a starved closed loop whose reported rate was
  > a small fraction of what the server was actually serving in the same mode: a generator
  > artefact misread as a slow server. The gate is now per-worker (2.2.3); a shared
  > `global_in_flight` is still kept, but only as a reported statistic, never as the hot-path gate.

`--ramp` replaces the fixed rate with the two-phase saturation search described in
§5b (`engine/ramp.rs`): it climbs QPS until the step saturates — the latency SLO breaks,
or (kernel-UDP) the served throughput stops tracking the target — then binary-searches
the exact knee.

---

## 5b. Dichotomic Saturation Discovery (the `--ramp` algorithm)

Finding a server's real maximum is the hard part of DNS benchmarking, and the common
approaches are coarse:

- **Fixed-load** requires you to already know the answer — you guess a rate and check
  whether it holds. Wrong guesses either under-load the server or drown it.
- **Linear step-ramp** (e.g. +100k each step) is slow over a wide range and still lands
  on whichever step boundary you happened to pick.
- **Geometric (doubling) ramp** covers the range fast but its resolution is a *whole
  octave*: if a server saturates between 8M and 16M, a doubling ramp can only tell you
  "more than 8M, less than 16M" and will report 8M. The true knee is invisible.

dnsmark's `--ramp` uses a two-phase **Dichotomic Saturation Discovery (DSD)**:

1. **Logarithmic discovery.** Start at 100k QPS and double each 5 s step (1 s paced
   warm-up **at the step's target rate** — not a flood burst, which would build an
   in-flight backlog and poison the window — followed by a 4 s paced measurement) until a
   step *breaks the latency SLO*. This brackets the maximum between the last sustained step
   (`lo`) and the first broken one (`hi`) in `log₂` steps.
2. **Dichotomic convergence.** Binary-search inside `[lo, hi]` — test the midpoint, move
   `lo` or `hi` toward it, repeat — until the bracket is within 5 %. e.g. 6.4M ok /
   12.8M broken → try 9.6M, 11.2M, 12.0M, 11.6M … converging on the real knee instead of
   falling back to a power of two.

**Saturation criterion.** A step is "sustained" when its **p50** round-trip latency is
under the SLO. The median — not p95/p99 — is the signal on purpose: a small fraction of
forwarded cache-misses produces large tail outliers that are a property of the *workload*,
not of server saturation, and would trip a tail-based test prematurely. Each step measures
its own window: the latency histogram is reset at the start of every step
(`ramp_step_latency()`), so the percentiles are the load *at that step*, never a cumulative
blur.

> **In kernel-UDP the criterion also gates on delivered throughput (since 2.7.7).** The
> kernel ramp is a gated closed loop: the shallow in-flight budget keeps p50 pinned at the
> floor even once the generator's kernel-recv can no longer *offer* the doubled target, so
> latency alone never trips and the exponential phase would climb to `MAX_DOUBLINGS` and
> quote a rate it never served. A kernel step is therefore sustained only when p50 held
> **and** the NIC-served rate reached ≥ 80 % of the target (`offer_ok`); saturation brackets
> at the real ceiling. **The `--xdp` ramp stays latency-only** — an open-loop firehose
> legitimately under-offers the (huge) target while the XSK ramps, so offer-gating it would
> false-saturate on the first step; its SLO genuinely trips at the wire ceiling. The
> bisection is additionally capped at `MAX_BISECT` iterations so a noisy served signal always
> terminates on the tightest bracket found.

> **The SLO is auto-calculated from the latency floor — never hardcoded (since 2.5.5).**
> A fixed 1 ms is wrong the moment the baseline RTT exceeds it (two switches + a router, or
> a kernel/VM resolver with a ~ms service floor) — every step would "fail" and the knee
> would read 0. The ramp records the lowest p50 it sees (the floor) and sets the SLO to
> `max(3 × floor, floor + 1 ms)`: it reduces to ~1 ms on an AF_XDP fast path (a
> sub-100-µs floor) and scales up on its own for a slower server or a real network.
> The SLO is fully auto-derived; there is no manual override flag.

> **The knee is reported as SERVED throughput, not offered.** The ramp reports the max
> **served** (completed at the NIC) over the SLO-holding steps — served caps at the server's
> real ceiling on its own, so the reported knee is the server's. In `--xdp` the open-loop
> offer climbs past the ceiling (unanswered queries never come back, so their latency is
> never sampled); in kernel-UDP the offer gate (above) stops the climb at the ceiling. Either
> way the reported figure is the served peak, not the offered target.

> **Two Capacity meanings, one per datapath — read the label (since v2.7.3).** What
> "Capacity" measures depends on the datapath, because the two ramps are not the same shape:
> - In **kernel-UDP** the ramp is a **gated closed loop** (dnsperf-comparable, latency-honest).
>   The generator's own kernel receive path drops replies under load, and those drops clog the
>   outstanding slots, so the *offered* rate is capped well below the server's real capacity.
>   The kernel-UDP DSD figure is therefore the **closed-loop SLO knee, generator-recv bound —
>   NOT the server's raw ceiling**, and it is labelled as such:
>   `Capacity: … (closed-loop knee — kernel-recv bound, NOT the server's raw max)`. To read the
>   server's raw ceiling from kernel-UDP, the ramp prints a pointer to the open-loop command —
>   `dnsmark -s <ip> -Q 0 --max-outstanding 0` — which reports `Server throughput (NIC rx)` =
>   `server_rx_qps` (§7), the authoritative reply rate. The kernel-UDP closed-loop knee sits
>   well below the same server's open-loop-flooded `server_rx_qps`, because the generator's own
>   kernel recv drops a large fraction of the replies under load. Kernel-UDP is generator-recv
>   bound; the AF_XDP datapath (§6) exists to remove exactly that bound with a lossless
>   zero-copy RX.
> - The **`--xdp` ramp is unaffected**: it is an open-loop firehose with a lossless zero-copy
>   RX, so its `Capacity` genuinely *is* the max replies/s on the wire.

**The SLO test cannot tell *whose* saturation it is.** A closed-loop generator at its
own limit inflates the measured RTT exactly like a saturating server, so a `--ramp`
knee is the knee of the *whole path*, generator included. Attribute it before quoting
it: if the breaking step's achieved q/s falls well short of its target (the per-step
`offered q/s` column stops tracking the doubling/bisection targets), or the reported
"Send throughput (egress)" plateaus below target while the receiver's NIC counters
confirm everything offered is being served, the knee is the **generator's**, not the
server's. Re-run on a faster rig (or split across NICs) before publishing the number
as a server maximum.

**What the two phases yield.** Both phases print the **same** one-line-per-step format —
`Ramp step: offered <N> q/s | served <N> q/s | rtt-samples <N> | p50 <ms> p95 <ms> p99 <ms>`
— for every doubling step and every bisection step (there are no per-step status labels; the
SLO decision is internal). At convergence the run prints a four-line summary:
`Idle latency: <ms> (floor — minimum p50 observed)`, `Capacity: <N> (NIC-verified — max
replies/s on the wire)` (the label differs in kernel-UDP — see above), `Within SLO: <N> (p50
stays under <ms>)`, and `Knee bracket (DSD bisection): [<lo> ; <hi>] q/s (±<pct>)`. A pure doubling ramp could only report the last clean
octave; DSD narrows that octave to a few-percent bracket and shows *exactly* where p50
turns, so one `--ramp` command answers "what is the real maximum, and what is the latency
right at it?".

Two properties of the output that are algorithm-, not rig-, specific: the very low-QPS
steps can show inflated p95/p99 when a small fraction of forwarded cache-misses dominates
a thin sample (which is why the **median** is the saturation signal, not the tail); and
`rtt-samples` is the *round-trip* count the generator could drain back — a lower bound on
what the server actually answered, so DSD characterises the **latency envelope** while the
**receiver's NIC counters** give the authoritative **throughput**. See
[benchmarking.md](benchmarking.md).

---

## 6. The AF_XDP datapath (opt-in)

With `--xdp`, dnsmark bypasses the kernel network stack on both send and receive.
Query frames are written straight into the NIC's **UMEM** and submitted to the **TX
ring**; responses are delivered to the **RX ring** by a tiny XDP/eBPF program that
redirects DNS replies (`udp src port 53`) into the per-queue `XSKS` socket map.
(`XDP_REDIRECT` into an AF_XDP socket is the kernel's native mechanism for handing a
frame straight from the NIC driver to a userspace socket — no network-stack
traversal, no copy; `XSKS` is the BPF map that points the redirect at the right
per-queue socket.) There is no `sendmsg`/`recvmsg`, no per-packet syscall, and no
socket-buffer copy.

Design points that make this fast *and* correct:

- **One worker per bound RX queue, capped to NIC-local physical cores** (see §2): one
  busy-poll worker per physical core is the stable point — more than that overdrives
  the ixgbe zero-copy datapath and collapses throughput. Each worker owns its queue's
  socket, UMEM and rings — no shared per-packet state.
- **Workers pinned to NIC-local physical cores** (never an HT sibling) so the DMA and
  the response handling stay on the memory controller closest to the NIC; at most a
  small cross-NUMA budget is used after the local cores.
- **Cycled source port + global reply matching.** Each worker *cycles* its UDP source
  port over a wide range (`10000 + (counter mod 2048)`, with a per-worker phase offset so
  workers do not emit the same port in lockstep). A fixed one-port-per-worker scheme
  emitted only `nworkers` distinct flows and collapsed the receiver's RSS onto a few
  queues, so it was dropped. Reply matching is by **global DNS transaction id** into a
  shared 65536-slot lock-free in-flight table — so a reply may return on *any* of the
  bound RX queues (wherever the generator's RSS scattered it) and still match. There is no
  per-worker id partition and no shared-state-free per-worker matching; a reply on
  "another worker's" queue is **not** lost.
- **Generator-side RSS steering — two regimes, because one RETA cannot serve both.** The
  NIC's default RSS spans **all** HW queues, so replies frequently hashed to an unbound
  queue and were dropped before the socket (a false ~100 % loss). The RETA is steered to
  span the bound queues (`ethtool -X`), and *how* depends on the mode:
  - **Closed-loop / latency** (`max_outstanding > 0`, bounded offered rate): funnel all
    replies to a single always-hot queue (`equal 1`) so it is drained continuously and the
    p50 SLO is not polluted by a ~10 ms "thinly-spread, rarely-polled" artefact.
  - **Firehose / throughput** (`max_outstanding == 0`): spread the RETA across **all** bound
    queues (`equal queue_count`) with per-queue counting. A single q0 worker (busy on the TX
    batch) drains only ~350 k resp/s while millions arrive, which produced the #15-P1 14×
    round-trip under-count; the spread + per-queue count makes round-trip track the server's
    real reply rate (**accurate since v2.5.0**, matching the README).

  TX always spreads across all bound queues regardless. (RETA only, no channel reconfig,
  safe around a live zero-copy bind; best-effort, warns on failure.)
- **`XDP_USE_NEED_WAKEUP`** kick semantics so the driver is only signalled when it needs
  to be.
- **No real-time scheduling.** Workers run `SCHED_OTHER`; the kernel can always preempt
  them, so per-core softirqs (and the host) stay healthy under load.

Operational hardening (so a benchmark is repeatable without a setup ritual):

- **Stale-program auto-detach.** On startup the loader force-detaches any XDP program
  already bound to the interface — a previous run killed with `SIGKILL` never runs its
  `Drop`, and the leftover program otherwise wedges the next attach and silently breaks
  TX. Pure netlink, no dependency; the user never has to `ip link set <if> xdp off`.
- **Per-socket AF_XDP statistics.** `getsockopt(SOL_XDP, XDP_STATISTICS)` (kernel UAPI
  only — no bpftool/libbpf code) exposes `tx_invalid_descs`, fill/completion-ring state,
  etc., live per queue. These are valid in zero-copy, where `ethtool -S` *netdev*
  counters read a flat zero.
- **Wire-truth guard.** dnsmark reads the NIC PHY tx counter (`*_nic`) around the run and
  prints the PHY-confirmed egress next to the submitted-descriptor egress; if they
  diverge it shouts and refuses to present a fictional rate. It never falls back to the
  netdev `tx_packets` counter, which counts *submissions*, not transmissions, under
  zero-copy — exactly the fiction the guard exists to catch. The sent counter itself is
  the **submitted**-descriptor count (v2.1.0): the completion ring under-reports at
  multi-Mpps, so it is not used for throughput.
- **Auto warm-up** (v2.1.0). The first seconds of a run (default 3 s, `DNSMARK_WARMUP`)
  are excluded from the measurement window, so XSK bind, ring fill and NIC ramp do not
  pollute the reported steady-state rate (`engine/mod.rs`).
- **CPU governor guard** (v2.1.0). With `--xdp`, every CPU is pinned to the
  `performance` governor for the run and restored on exit (`governor.rs`) — DVFS is the
  #1 benchmark confounder.
- **Huge-page UMEM** (v2.1.0). The UMEM is backed by 2 MiB huge pages when available,
  with a 4 KiB fallback (`transport/xdp/umem.rs`) — fewer dTLB misses at multi-Mpps.
- **802.1Q VLAN — experimental** (v2.2.0). `DNSMARK_VLAN=<vid>` bakes one 802.1Q tag
  into the frame template (the hot path stays copy+patch), an optional tag is skipped
  on RX, and the AF_XDP socket binds the **physical parent** of a VLAN sub-interface
  (AF_XDP zero-copy cannot bind a sub-interface) while reading src IP/MAC from the
  sub-interface; the wire-truth PHY counter also resolves to the parent
  (`transport/xdp/frame.rs`). The frame layout is unit-tested against the 802.1Q wire
  spec and a resolver round trip over a tagged VLAN is proven end-to-end, but tagged
  generation has not been rate-tested (no zero-copy-capable NIC was available). I
  cannot confirm tagged generation at line rate.

On an Intel X520 (82599) this saturates a 10 GbE link; see
[benchmarking.md](benchmarking.md) for the throughput methodology (measured at NIC
counters, not at the application).

**Symmetric-transport rule.** `--xdp` is for benchmarking a server that is *itself*
AF_XDP, or for raw saturation. Comparing an XDP generator against a kernel server (or
vice-versa) compares two different datapaths and is not a fair latency measurement. The
default UDP path is the one to use for a fair kernel-vs-kernel comparison. See §7.

---

## 7. What a generator actually measures (and why the wire is the anchor)

A closed-loop generator's reported RTT is the **sum of three terms**:

```
reported RTT = server processing + network round-trip + generator client-side overhead
```

Only the first is a property of the server; the third belongs to the *tool* and differs
between any two generators, so two tools' **absolute** numbers differ even against the
same server — which is expected and is **not a defect in either**.

dnsmark therefore validates latency against the **wire** — a `tcpdump` capture on the
server, paired by DNS transaction id, which isolates the server's own term — rather than
against another tool. Across two rigs and both generator↔receiver directions the generator
always reports *more* than the wire (it never under-measures), dnsmark's light client path
sits close to the wire, and for a fixed generator the offset is stable across servers (so
server rankings are preserved). The full decomposition, numbers, and
reproduction commands are in **[benchmarking.md §7](benchmarking.md)**.

**Throughput has the same anchor: `server_rx_qps`.** The authoritative served rate is not
the generator's send counter but the egress NIC's hardware **rx** counter on the receiver
(`rx_packets` + ring-overflow drops) — `server_rx_qps`. That NIC-counter truth is the
reference for every throughput figure in this document; the generator's own RX drain (§5b)
is a lower bound, never the quoted number. This is why the two ramps report different
Capacity meanings (§5b, since v2.7.3): the kernel-UDP closed-loop knee is generator-recv
bound and is *not* `server_rx_qps`, so its label points to the open-loop
`dnsmark -s <ip> -Q 0 --max-outstanding 0` run — whose `Server throughput (NIC rx)` *is*
`server_rx_qps` — for the raw ceiling; the `--xdp` ramp, being a lossless open-loop firehose,
already reports the on-wire max directly.

### 7a. The generator effect — why one server yields four headline numbers (2026-07-03, dnsmark 2.7.5)

The point above is not academic. Run the **same server** under four different generators and
it reports four different "throughputs" — not because the server changed, but because each
generator imposes a different **load discipline** on it. The receiver's NIC tx counter
(`server_rx_qps`) is what makes those four numbers commensurable, and it is the only thing that
does.

The measurement below drives four servers — BIND9 9.x, Unbound 1.22, Runbound `xdp:no`, Runbound
`xdp:yes` — in **strict parity** (all four forward+cache to 1.1.1.1/8.8.8.8/9.9.9.9, DNSSEC off,
minimal-responses, a large cache pre-**warmed** with the 100k-domain corpus so every measured
query is a cache hit) across four generators of increasing load aggression, on a single-link Intel
X710 (i40e), 10 GbE DAC, generator = dual Xeon E5-2690 v2, receiver = Threadripper PRO 5995WX. Each
figure is the **receiver NIC tx_packets delta / 20 s** — the replies the server actually put on the
wire, the datapath- and tool-independent truth (dnsmark self-reports the same value as
`server_rx_qps`):

| Server              | dnsperf | dnsmark-udp | dnsmark-xdp | kxdpgun |
|---------------------|--------:|------------:|------------:|--------:|
| BIND9 9.x           |   711 k |     1.89 M  |      872 k  |  1.03 M |
| Unbound 1.22        |  1.55 M |     2.57 M  |     3.06 M  |  2.80 M |
| Runbound (xdp:no)   |  1.26 M |     3.40 M  |     6.81 M  |  5.50 M |
| Runbound (xdp:yes)  |  1.65 M |     5.27 M  |    12.5 M   | 10.1 M  |

Generators, in load-discipline order: **dnsperf** (`-c 500 -T 20 -q 100000`, closed-loop
kernel-UDP), **dnsmark-udp** (kernel firehose, `-Q 6M --max-outstanding 0`), **dnsmark-xdp**
(AF_XDP firehose, `--xdp -Q 13M --max-outstanding 0`), **kxdpgun 3.4.6**
(`-Q 13M`, AF_XDP). Read the table across a row and three things fall out:

- **The server ceiling ranking is unambiguous** (peak over generators): Runbound `xdp:yes`
  **12.5 M** > Runbound `xdp:no` **6.8 M** > Unbound **3.1 M** > BIND9 **1.9 M**. Runbound-xdp is
  ~6.6× BIND9, ~4.1× Unbound, and ~1.8× its own kernel mode — the AF_XDP fast path roughly
  **doubles** Runbound's own kernel-path throughput.
- **The open-loop AF_XDP generators drive a *robust* server hardest.** On the three servers that
  hold up under pressure (Unbound, both Runbounds), dnsmark-xdp is the strongest saturation
  generator and kxdpgun is close behind; both leave the closed-loop dnsperf and the gentler
  kernel-UDP firehose well below the server's real ceiling. dnsperf (closed-loop, latency-bounded)
  is *consistently the lowest* on every row — it is a latency tool, not a saturation tool, so it
  paces itself off the RTT and never offers enough load to find the wall.
- **BIND9 is the exception, and it is the whole point.** BIND9 does *not* peak under the 13 M XDP
  firehose — it **collapses**: 872 k (dnsmark-xdp) and 1.03 M (kxdpgun) versus **1.89 M** under the
  gentler kernel-UDP dnsmark. This is textbook **receiver livelock / overload**: at extreme
  ingress BIND9 burns all its CPU in softirq servicing the interrupt/receive storm and dropping
  frames, and ends up processing *less* than it does at a lower offered rate. Handed a firehose it
  cannot drink from, its served rate goes *down*. A benchmark that quoted BIND9's number under the
  hardest generator would report **872 k**; one that used the kernel-UDP firehose would report
  **1.89 M** — a ~2× swing on the identical server and cache, decided entirely by how hard the
  generator pushed.

That last row is exactly why **the generator's headline number is not the server's throughput** —
it is a joint property of the server *and* the generator's load discipline. The only figure that
survives the swap between all four tools is the one taken off the wire at the receiver:
`server_rx_qps` = receiver NIC tx / interval. It counts the replies that physically left the
server regardless of which datapath (kernel or XDP) or which tool produced them, so it is the sole
cross-generator/cross-tool comparable truth — and the reason every throughput figure in this
document is anchored to it rather than to any generator's send counter.

**Caveats, stated honestly.** These are **single 20 s runs**, not averaged — expect ±10–15 %
run-to-run. Line rate is **reply-size dependent** (§7): the single-link packet ceiling for a
given NIC scales with the average on-wire reply size, so a warm that produces a *smaller*
average reply raises the packet ceiling accordingly — the Runbound `xdp:yes` **12.5 M** is still
the honest receiver-NIC-tx count, just at a smaller average reply size than the ~100 B figure
§7's ceiling assumes. All four columns are the receiver-NIC-tx truth; nothing here is a generator
self-report.

**Line-rate awareness (`--` / `--json`, since v2.7.1) — is the run wire-bound or is there
headroom?** At multi-Mpps the honest next question is *what* is the wall. dnsmark now answers
it from its **own** hardware observations, building directly on `server_rx_qps`: it divides
the authoritative served rate by the line-rate ceiling implied by the average on-wire reply
size (the egress NIC's `rx_bytes/rx_packets`) and the egress-NIC link speed, and reports the
result as a **% of line rate** with a verdict:

- **wire-bound** (`server_rx_qps` ≥ 90 % of line rate) — the Ethernet link is the limit;
- **link-headroom** (< 90 %) — the wall is the server or the generator, *not* the wire.

This needs no receiver-side reading beyond the NIC counter already at the heart of
`server_rx_qps`, and it works identically in AF_XDP and kernel-UDP mode.

> **The line-rate verdict is emitted for fixed/flood runs only, never in `--ramp` (since
> v2.7.2).** In a fixed or flood run `server_rx_qps` reflects one steady window, so the
> % of line rate is meaningful. In `--ramp` the same counter spans the whole ramp-up and
> its average sits far below the peak — a line-rate % there would contradict the DSD's own
> "Capacity" summary. In `--ramp` the DSD Capacity / Within SLO / Knee bracket (§5b) *is*
> the throughput answer; the line-rate line is suppressed.

The finding it surfaces is structural: on a 10 GbE link, DNS at multi-Mpps is **line-rate
bound**. For ~100 B replies the packet ceiling is reached well before the CPU or the PCIe
bus is — an x8 Gen3 slot carries several times the 10 G link's bandwidth, and a single
physical core building minimal DNS frames already emits fast enough to fill the wire. The
consequence for the datapath discussion above: a **single core can saturate a 10 G NIC in
both directions** (TX *and* RX-count), so a 1-core unified worker and a 2-core TX/RX split
reach the **same** wire-bound throughput. The wire is the wall, not the CPU and not PCIe.
This is also why the auto warm-up default is now **5 s** (was 3 s): the reported rate is read
at steady state, after the link has settled at its ceiling.

**`--wire-latency` (built-in wire anchor, since 2.5.8).** Rather than only validating against
an external `tcpdump`, dnsmark can read the wire stamps itself: a serial-ping-pong mode that
takes kernel **SO_TIMESTAMPING** TX+RX timestamps (raw-hardware when the NIC stamps the flow,
else software/driver-level) and reports the round-trip with the **generator's userspace/socket
overhead excluded** — and, being serial, free of the open-loop queuing that inflates
throughput-mode latency. It reports server + network (the generator cannot isolate the
server's own term — that still needs a capture *on the server*), but it removes the tool's
third term, so the reported round-trip sits below the userspace serial RTT by the amount of
that excluded socket/userspace overhead.

The practical rules that follow:

- never quote a generator's absolute latency as "the server's latency";
- compare servers with **one fixed generator on one rig**;
- cite the **wire** for the server's own contribution.

---

## 8. Output

Every run produces the same metrics, available live (TUI), as JSON (`--json`, for
CI/automation), as CSV (`--csv`), or as plain text. The plain-text report (header
`DNS Performance Testing Tool — dnsmark 2.7.5`) prints the parameters, then the
statistics (queries sent/completed/lost + response-code breakdown), then the throughput
block in this fixed order:

```
Send throughput (egress):  <N> qps        ← matches rx_packets on the receiver NIC
Wire egress (NIC PHY):     <N> qps  (confirmed transmitted)      [XDP only]
Round-trip completed:      <N> qps  (<P>% of egress, userspace)
Server throughput (NIC rx):  <N> qps  (authoritative — replies on the wire)
Line rate:                 <P>% of <G> Gb/s wire  (<B> B replies, ceiling <N> M/s)
  → WIRE-BOUND: …    or    → link has headroom: …               [fixed/flood only]
```

followed by latency min/avg/p50/p95/p99/p999/max and the run time. (In an XDP firehose,
`-Q 0 --max-outstanding 0`, latency is not sampled and those lines read `0.000 ms`; the
`Line rate` line and its verdict appear in fixed/flood runs only, never in `--ramp`.) The
JSON schema is stable and is the recommended interface for automated comparison.

The JSON also carries a **`host`** object — the generator's CPU model, physical/logical
core counts, NUMA nodes, memory, and the egress NIC (driver, link speed, NUMA node) — so a
result records the rig it was produced on, and a **`notes`** array that flags conditions
worth knowing (e.g. high loss → the result may be bounded by the *receiver's* NIC/bus, not
the server; read the receiver's NIC counters). A one-line host banner is printed at
startup.

Since v2.7.1 the report also carries the **line-rate verdict** (§7). The text report prints
a `Line rate: X% of Y Gb/s wire (Z B replies, ceiling N M/s)` line followed by a
`-> WIRE-BOUND: …` or `-> link has headroom: …` verdict, and `--json` adds a **`line_rate`**
object — `{rate_qps, avg_reply_bytes, link_mbps, line_rate_pps, percent_of_line, verdict}` —
plus an explanatory entry in `notes`. It is computed entirely from `server_rx_qps` and the
egress NIC counters, so it appears in both AF_XDP and kernel-UDP runs with no receiver-side
reading. Since v2.7.2 it is emitted for **fixed/flood runs only** — in `--ramp` the
`server_rx_qps` average spans the whole ramp-up and would contradict the DSD Capacity
summary, so the line-rate line is suppressed and the JSON `line_rate` is `null`, with the DSD Capacity /
Within SLO / Knee bracket (§5b) is the throughput answer instead.

---

## 9. Reproducibility and limitations

- **Reproduce, don't quote.** Absolute latencies are rig-dependent. The commands in
  benchmarking.md let you re-derive every number on your own hardware.
- **One static binary.** dnsmark builds as a static musl binary with no runtime
  dependencies, so the *generator* is the same artefact across machines.
- **Synthetic workload.** A single static record with recursion off isolates a server's
  data plane; it is not a recursive-resolver or cache-miss workload.
- **txid pairing.** The wire-capture method pairs by the 16-bit DNS id, which recycles at
  high QPS; anchor the wire on **p50** (robust) — each tool's own tail is matched by
  internal per-query state and is reliable.
- **AF_XDP needs a physical NIC.** It cannot bind a bond/bridge/veth; it requires
  `CAP_NET_RAW`/`CAP_BPF` (or root) and flow control disabled on the sender to reach line
  rate (see benchmarking.md). A VLAN sub-interface is handled by binding its physical
  parent and injecting the tag (`DNSMARK_VLAN`, experimental — see §6).

### Known caveats (write them down rather than hide them)

- **In-flight table sizing and eviction accounting.** Each UDP worker's in-flight table
  is a power-of-two slot array indexed by `id & (len−1)`. With sequentially-issued ids
  and the table sized to ≥ the outstanding window (controlled-rate mode), there are zero
  collisions. In **flood/unlimited** mode (`--max-outstanding 0`) the number in flight
  can exceed the table length; when two ids hash to the same slot, `insert()` detects the
  collision and the evicted query is counted as a **timeout** (a loss) and removed from
  `global_in_flight` — so `sent == completed + lost` holds exactly even in flood mode,
  with no query silently disappearing. Eviction-timeouts, like all timeouts, count toward
  `queries_lost`, not the latency histogram. Quote latency from **controlled-rate** runs.
- **Flood mode bounds the latency window (read p99 with the loss rate).** A consequence of
  the above: at offered rate *R*, the in-flight table holds at most `table_len` queries,
  so a response slower than ≈ `table_len / R` is evicted (counted as a *loss*) before it
  can return (e.g. a 65 536-slot table at R = 10 M qps evicts anything slower than
  ≈ 6.5 ms). In **flood** the latency histogram therefore holds only the responses that
  came back *within* that window — its p99 is the latency of the queries that **made it
  back**, and is optimistic if read without the loss rate. This is not hidden data (the
  slow ones are losses, reported in `queries_lost`), but it means **p99 in flood must be
  read together with loss%** — and for any latency figure you should use a controlled rate,
  where there are no evictions and loss ≈ 0.
- **`--compare` shares one async runtime.** The two servers run as concurrent tasks in
  the same runtime, so a side-by-side compare is fair at controlled rates but not a clean
  isolation at saturation (the tasks contend for the runtime). For saturation
  comparisons, run each server separately on the same rig.
- **`--ramp` steps are short windows.** The dichotomic phase (§5b) narrows the bracket
  to within 5 %, so the coarse-resolution caveat of a pure doubling ramp no longer
  applies — but each step's verdict is the p50-vs-SLO test over that step's own 4 s
  paced window, so an effect slower than a step (cache pollution, thermal throttling)
  can pass a step it would fail at steady state. Confirm a published figure with a
  fixed-rate run at the reported maximum.
- **IPv6 + `--xdp`.** NUMA-local pinning is derived from the IPv4 route; an IPv6 target
  skips it — workers still run, just without NUMA pinning.
- **The XDP capability probe is advisory.** A successful `AF_XDP` socket open means the
  kernel supports the family, not that attach will succeed (containers, missing BPF
  privileges, virtual interfaces). dnsmark falls back if attach fails; treat the
  capability flag as a hint, not a guarantee.
- **Multi-NIC aggregate percentiles.** With several NICs (`-s` repeated), the aggregate
  latency percentiles are the **worst NIC's** value, not a true cross-NIC percentile —
  percentiles cannot be averaged. This is conservative and *surfaces* a slow NIC rather
  than hiding it behind a weighted average; use `--nic-stats` for per-NIC percentiles.
  (Aggregate throughput, mean, min and max are exact.)

---

## 10. The hot-path copy — why it is plain `copy_from_slice`

`write_with_index()` (`query/wire.rs`) assembles each DNS query frame by copying a
pre-built wire template (30–60 bytes) into the send buffer and patching 2 bytes (the
transaction ID). A hand-written, runtime-dispatched SIMD copy (AVX2 32 B/iter → SSE2
16 B/iter) was evaluated against the standard library's `copy_from_slice`, both in an
isolated criterion microbench and in an end-to-end paired A/B against the same receiver.

### Why `copy_from_slice`

At these template sizes (< 64 bytes) the compiler-generated `copy_from_slice` under `-O3`
— which emits `vmovdqu` + `vmovq` automatically, without a function-call boundary — is as
fast as or **faster than** the hand-written AVX2 loop. The loop's overhead comes from the
call boundary, the branch on `len >= 32`, and the loop counter, all absent in the inlined
scalar path. And even that copy-level difference is sub-nanosecond per op — completely
masked end-to-end by the `send()` syscall (single-digit µs) and the network RTT, so the
choice of copy is invisible in qps and p50.

### Decision

Because `copy_from_slice` is **as fast as or faster than** the hand-written SIMD at these
sizes — and is simpler, `unsafe`-free, and one fewer thing to audit — the hot-path copy
uses `copy_from_slice`, and the hand-rolled AVX2/SSE2 memcpy has been **removed**
(v2.0.2). CPU-tier detection is kept only for the startup banner. dnsmark claims **no**
SIMD-driven speedup anywhere — the copy is an implementation detail, not a feature. If
profiling on a 25/100 G NIC ever puts the copy on the critical path, the correct fix is to
eliminate the copy entirely (zero-copy UMEM frames reuse the template in-place), not to
re-introduce a hand-rolled loop.

---

*This document describes the implementation as of v2.7.5 (2026-07-03). Mechanisms are
referenced to their source files so the description can be checked against the code
rather than taken on trust.*
