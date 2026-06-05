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
the hot path except one atomic counter for the global outstanding gate; each owns its
own socket, its own in-flight table, and its own send/receive loop.

- For UDP/TCP/DoT, N = `--clients` (`-c`).
- For AF_XDP, N is **auto-detected** from the NIC: one worker per RX queue
  (`get_rx_queue_count`), each pinned to a **physical** core on the NIC's NUMA node
  (`numa_node_for_iface`) — the lower half of that node's logical CPUs, never an HT
  sibling; if there are more queues than physical cores the pinning wraps.

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
     timestamp = clock.now()        ← taken BEFORE send(), the dnsperf timestamp point
     send(fd, query, MSG_DONTWAIT)
     in_flight.insert(id, timestamp)
     global_in_flight += 1
     advance next_send by send_interval   (no burst catch-up after a stall)

2. WAIT  poll(fd, POLLIN, µs_until_next_send)
     wakes immediately on a response, or at the next send deadline — never overshoots.
     For sub-millisecond intervals it busy-spins with a non-blocking peek instead
     (poll() has only ms resolution).

3. DRAIN recvmmsg(fd, …, 64, MSG_DONTWAIT)        ← up to 64 responses per syscall
     for each response:
       timestamp = clock.now()
       rtt = timestamp − in_flight.take(id)
       histogram.record(rtt); global_in_flight −= 1

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

## 4. In-flight tracking and the latency histogram

**Per-worker in-flight table.** Each worker keeps its own table mapping query id →
send timestamp — there is no shared map and no per-packet lock.

- UDP path (`transport/udp.rs`): a power-of-two `Vec<(u16, u64)>` indexed by
  `id & (len−1)`; `insert`/`take` are O(1).
- AF_XDP path (`transport/xdp/receiver.rs`): a lock-free `Box<[AtomicU64]>` of 65 536
  slots indexed directly by the 16-bit DNS id. To keep ids unique *across* workers, the
  id space is partitioned — worker *k* owns the range `[k·span, (k+1)·span)` with
  `span = 65536 / N`.

**The latency histogram.** Completed RTTs go into an HDR histogram
(`stats/mod.rs`, range **1 µs – 60 s, 3 significant figures**), from which
p50/p95/p99/p999, min, mean and max are read at the end. HDR gives constant-time
recording and bounded relative error across six orders of magnitude.

**Honest tail — the part most tools get wrong.** A query that never gets a response
must not simply vanish from the percentiles, or the tail is silently truncated and the
server looks better than it is. dnsmark records *every* outstanding query into the
histogram at its real age:

- the **10 ms timeout sweep** records each expired query at its measured age and counts
  a timeout (`record_response(0xff, age); inc_timeout()`);
- at **end of run**, any still-in-flight query is drained into the histogram the same
  way.

So p99/p999 reflect the slowest responses, including the ones that timed out.

**Outstanding depth** is tracked too (mean and max concurrent in-flight), and reported
in JSON — this is the number to align with dnsperf's `-q` when comparing the two tools.

---

## 5. Rate control and the outstanding gate

Two independent limits shape the send side:

- **Rate** — each worker holds a `send_interval = 1 / qps_per_worker`. It sends when
  `now ≥ next_send`, then advances `next_send`. After a stall it does **not** burst to
  catch up (`next_send = now + interval`), so a scheduler hiccup cannot produce a
  thundering send and a distorted tail.
- **Outstanding** — a shared atomic `global_in_flight` is gated against
  `--max-outstanding` (default 100, mirroring `dnsperf -q`). This bounds how many
  queries can be in flight at once, exactly like dnsperf's closed-loop window.

`--ramp` replaces the fixed rate with a controller that climbs QPS until the server
stops keeping up, reporting the maximum sustainable rate (`engine/ramp.rs`).

---

## 6. The AF_XDP datapath (opt-in)

With `--xdp`, dnsmark bypasses the kernel network stack on both send and receive.
Query frames are written straight into the NIC's **UMEM** and submitted to the **TX
ring**; responses are delivered to the **RX ring** by a tiny XDP/eBPF program that
redirects DNS replies (`udp src port 53`) into the per-queue `XSKS` socket map. There
is no `sendmsg`/`recvmsg`, no per-packet syscall, and no socket-buffer copy.

Design points that make this fast *and* correct:

- **One worker per NIC RX queue**, each owning its queue's socket, UMEM and rings — no
  shared per-packet state.
- **Workers pinned to NIC-local physical cores** (the lower half of the NUMA node's
  CPUs, never an HT sibling) so the DMA and the response handling stay on the memory
  controller closest to the NIC.
- **Fixed source port per worker** (`2048 + worker_id`): the receiver's RSS hashes a
  worker's responses back to a single queue, which the same worker owns — so each
  worker matches its own replies, with no cross-worker traffic.
- **`XDP_USE_NEED_WAKEUP`** kick semantics so the driver is only signalled when it needs
  to be.
- **No real-time scheduling.** Workers run `SCHED_OTHER`; the kernel can always preempt
  them, so per-core softirqs (and the host) stay healthy under load.

On an Intel X520 (82599) this saturates a 10 GbE link; see
[benchmarking.md](benchmarking.md) for the throughput methodology (measured at NIC
counters, not at the application).

**Symmetric-transport rule.** `--xdp` is for benchmarking a server that is *itself*
AF_XDP, or for raw saturation. Comparing an XDP generator against a kernel server (or
vice-versa) compares two different datapaths and is not a fair latency measurement. The
default UDP path is what you compare against dnsperf. See §7.

---

## 7. What a generator actually measures (and why the wire is the anchor)

A closed-loop generator's reported RTT is the **sum of three terms**:

```
reported RTT = server processing + network round-trip + generator client-side overhead
```

Only the first is a property of the server; the third belongs to the *tool* and differs
between any two generators. dnsmark and dnsperf are both closed-loop UDP generators and
differ in that third term, so their **absolute** numbers differ even against the same
server — which is expected of any two tools and is **not a defect in either**.

dnsmark therefore validates latency against the **wire** — a `tcpdump` capture on the
server, paired by DNS transaction id, which isolates the server's own term — rather than
against another tool. Across two rigs and both generator↔receiver directions, dnsmark and
dnsperf both report *more* than the wire (neither under-measures), dnsmark sits closer to
the wire (lower client-side overhead), and for a fixed generator the offset is stable
across servers (so server rankings are preserved). The full decomposition, numbers, and
reproduction commands are in **[benchmarking.md §7](benchmarking.md)**.

The practical rules that follow:

- never quote a generator's absolute latency as "the server's latency";
- compare servers with **one fixed generator on one rig**;
- cite the **wire** for the server's own contribution.

---

## 8. Output

Every run produces the same metrics, available live (TUI), as JSON (`--json`, for
CI/automation), as CSV (`--csv`), or as plain text: achieved QPS, sent/completed/lost,
RCODE breakdown, latency min/mean/p50/p95/p99/p999/max, and in-flight mean/max. The JSON
schema is stable and is the recommended interface for automated comparison.

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
  rate (see benchmarking.md).

### Known caveats (write them down rather than hide them)

- **In-flight table sizing and eviction accounting.** Each UDP worker's in-flight table
  is a power-of-two slot array indexed by `id & (len−1)`. With sequentially-issued ids
  and the table sized to ≥ the outstanding window (controlled-rate mode), there are zero
  collisions and `sent == completed + lost` exactly. In **flood/unlimited** mode
  (`--max-outstanding 0`) the number in flight can exceed the table length; when two ids
  hash to the same slot, `insert()` detects the collision, records the evicted query's
  real age into the latency histogram as a timeout (`record_response(0xff, age_us)`),
  and decrements `global_in_flight` — so the accounting identity `sent == completed +
  lost` holds exactly even in flood mode. The tail is never silently truncated.
  Quote latency from **controlled-rate** runs; flood-mode p99 reflects both true slow
  responses and eviction timeouts.
- **`--compare` shares one async runtime.** The two servers run as concurrent tasks in
  the same runtime, so a side-by-side compare is fair at controlled rates but not a clean
  isolation at saturation (the tasks contend for the runtime). For saturation
  comparisons, run each server separately on the same rig.
- **`--ramp` is a doubling search.** It can overshoot the true maximum by up to one step
  before the loss criterion trips; read the reported max as the top of the last
  *sustained* step, and narrow with a fixed-rate sweep if you need a tight figure.
- **IPv6 + `--xdp`.** NUMA-local pinning is derived from the IPv4 route; an IPv6 target
  skips it — workers still run, just without NUMA pinning.
- **The XDP capability probe is advisory.** A successful `AF_XDP` socket open means the
  kernel supports the family, not that attach will succeed (containers, missing BPF
  privileges, virtual interfaces). dnsmark falls back if attach fails; treat the
  capability flag as a hint, not a guarantee.

---

*This document describes the implementation as of v2.0.0. Mechanisms are referenced to
their source files so the description can be checked against the code rather than taken
on trust.*
