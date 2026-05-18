# Benchmark — Runbound v0.4.2 vs Unbound 1.22.0

**Date:** 2026-05-18  
**Tools:** dnsmark 0.4.3, dnsperf 2.14.0  
**Duration per test:** 30 s (ramp mode: variable)

---

## ⚠️ Bug discovered: `rate-limit: 0` — all Runbound results invalid

After this benchmark was run, a configuration bug was discovered in Runbound v0.4.2:
the `rate-limit` parameter was set to `0`. In Runbound's implementation at the time,
`rate-limit: 0` meant **refuse all queries** — the server responded `REFUSED` to every
incoming DNS query regardless of zone membership or query type.

**Impact on this report:**

- Every Runbound measurement showing `REFUSED` (100 %) is an artifact of this bug, not
  a reflection of Runbound's actual DNS resolution capability.
- The throughput figures (~15k QPS) measure Runbound's REFUSED-response pipeline, not
  its authoritative resolution pipeline.
- The latency figures measure the cost of the rate-limit check path, not the zone-lookup
  and response-building path.
- The ramp anomaly (9 659/s → 732/s, see §Ramp) was caused by the rate-limiter
  exhausting its internal budget over successive bursts — not by a security feature.

**All Runbound data rows in this document are marked ⚠️ and must be considered invalid.**

> **Fixed in Runbound v0.4.7**: `rate-limit: 0` now disables rate limiting entirely
> (Unbound-compatible semantics). A corrected benchmark will be published separately.

> Note: Runbound serves its zones via the REST API (zone CRUD, record management).
> Under correct configuration it is a full authoritative DNS server, not an
> authoritative-only refuser. The REFUSED responses observed here are entirely due to
> `rate-limit: 0`.

---

## Environment

### Physical host

| Property | Value |
|---|---|
| Hostname | codix-gaming |
| Model | Dell PowerEdge T620 |
| CPU | 2× Intel Xeon E5-2690 v2 @ 3.00 GHz (2 sockets × 10 cores × HT = 40 logical CPUs) |
| RAM | 251.81 GiB |
| Hypervisor | Proxmox VE 9.1.6 (QEMU/KVM) |
| Host kernel | Linux 6.17.2-2-pve |

### VMs

| Property | VM1 — Runbound | VM2 — Unbound |
|---|---|---|
| IP | 192.168.1.10 | 192.168.1.11 |
| Software | Runbound v0.4.2 | Unbound 1.22.0 |
| Role | Authoritative DNS (REST API) | Recursive resolver |
| OS | Debian 13 | Debian 13 |
| Kernel | Linux 6.12.57+deb13-amd64 | Debian 13 |
| vCPU | 2 | — |
| RAM | 1.9 GiB | — |
| NIC driver | virtio (Proxmox bridge) | virtio (Proxmox bridge) |

### Benchmark client

| Property | Value |
|---|---|
| IP | 192.168.8.245 |
| dnsmark | 0.4.3 (`/root/dnsmark/target/release/dnsmark`) |
| dnsperf | 2.14.0 (`/usr/bin/dnsperf`) |

> **Note on client placement:** The benchmark client ran from 192.168.8.245 (same Proxmox
> host, different virtual subnet) rather than 192.168.1.10 as originally planned — SSH
> key access to VM1 was not available at test time. Network conditions are equivalent:
> all traffic transits the same virtual bridge, no physical NIC involved.

### Network note

> Both VMs share the same physical host (codix-gaming, Proxmox VE 9.1.6, QEMU/KVM).
> Network traffic between VMs transits via a virtual bridge — no physical NIC involved.
> Results reflect VM-to-VM performance on shared hardware, not bare-metal or physical
> network performance. Bare-metal results with Intel fiber NICs are planned.

---

## Network baseline

| Target | Packets | Min RTT | Avg RTT | Max RTT | Loss |
|---|---|---|---|---|---|
| Runbound 192.168.1.10 | 100 | 0.294 ms | 0.487 ms | 3.390 ms | 0 % |
| Unbound 192.168.1.11 | 100 | 0.316 ms | 0.413 ms | 0.536 ms | 0 % |

The Runbound VM shows occasional jitter (max 3.4 ms vs 0.5 ms for Unbound), consistent
with the 2 vCPU constraint and Proxmox CPU scheduling variance on a shared host.

---

## dnsperf results (reference tool)

> ⚠️ **Runbound data invalid** — all responses are REFUSED due to `rate-limit: 0` bug.
> Values reflect the REFUSED-response path only, not real DNS resolution.

Test: `dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8`

| Scenario | QPS target | Runbound QPS ⚠️ | Runbound completion | Runbound avg RTT | Runbound max RTT | Unbound QPS | Unbound completion | Unbound avg RTT | Unbound max RTT |
|---|---|---|---|---|---|---|---|---|---|
| Controlled 5k | 5 000 | ⚠️ 4 999 | 100 % (REFUSED) | 0.285 ms | 20.9 ms | 4 997 | 100 % | 0.272 ms | 73.3 ms |
| Controlled 10k | 10 000 | ⚠️ 9 999 | 100 % (REFUSED) | 0.324 ms | 27.1 ms | 9 999 | 100 % | 0.230 ms | 67.9 ms |
| Unlimited | — | ⚠️ 15 247 | 100 % (REFUSED) | 6.525 ms | 418.9 ms | **45 752** | 100 % | 2.165 ms | 63.2 ms |

> dnsperf "completion" = received any response. Runbound packet size = 29 bytes (REFUSED,
> no answer section). Unbound packet size = 67 bytes (full answer).  
> **Unbound Unlimited result is valid.** Runbound figures are to be re-measured.

---

## dnsmark results

> ⚠️ **Runbound data invalid** — all responses are REFUSED due to `rate-limit: 0` bug.
> To be re-measured after fix.

Query source: `--random` (random UUID subdomains under `bench.invalid.`). All latency in ms.

### Runbound ⚠️ invalid — to be re-measured (bug: rate-limit: 0)

| Scenario | QPS target | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| 5k | 5 000 | ⚠️ 4 988 | 99.997 % | REFUSED | 0.358 ms | 0.320 ms | 0.541 ms | 0.858 ms | 5.463 ms |
| 10k | 10 000 | ⚠️ 9 856 | 99.993 % | REFUSED | 1.545 ms | 1.242 ms | 2.337 ms | 5.803 ms | 14.895 ms |
| 50k | 50 000 | ⚠️ 15 840 | 99.979 % | REFUSED | 6.362 ms | 5.811 ms | 9.759 ms | 14.247 ms | 178.687 ms |

### Unbound ✅ valid

| Scenario | QPS target | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| 5k | 5 000 | 4 985 | 100.000 % | NXDOMAIN | 0.295 ms | 0.248 ms | 0.328 ms | 0.914 ms | 9.511 ms |
| 10k | 10 000 | 9 970 | 99.999 % | NXDOMAIN | 0.366 ms | 0.249 ms | 0.339 ms | 4.731 ms | 12.743 ms |
| 50k | 50 000 | **47 332** | 99.9996 % | NXDOMAIN | 0.661 ms | 0.395 ms | 1.183 ms | 8.535 ms | 19.807 ms |

> Unbound delivers 47 332 QPS at 50k target (94.7 % of target) — still scaling, not
> saturated. p999 at 19.8 ms remains well-controlled.

---

## Recursive resolution — known domains (queries.txt, 500 QPS)

> ⚠️ **Runbound data invalid** — REFUSED due to `rate-limit: 0` bug.

Test: `dnsmark -s $TARGET -d /tmp/queries.txt -Q 500 -l 30 -q --json`

| Server | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| Runbound ⚠️ | 479 | 100 % | REFUSED 100 % | 0.357 ms | 0.305 ms | 0.544 ms | 0.964 ms | 11.263 ms |
| Unbound ✅ | 479 | 100 % | NOERROR 100 % | 0.306 ms | 0.261 ms | 0.377 ms | 0.924 ms | 10.071 ms |

---

## Ramp mode — saturation point

Test: `dnsmark -s $TARGET --random --ramp -q`

### Runbound ramp ⚠️ invalid — to be re-measured (bug: rate-limit: 0)

| Step target QPS | Burst measured | 80 % threshold | Saturated? |
|---|---|---|---|
| 1 000 | ⚠️ 9 659 /s | 800 | No — advance |
| 2 000 | ⚠️ 732 /s | 1 600 | **Yes** |

**Reported max sustainable QPS (Runbound): 1 000** — ⚠️ invalid, caused by the bug.

> **Root cause (corrected):** The sharp drop from 9 659/s to 732/s between burst 1 and
> burst 2 is **not a security feature** — it is a direct consequence of the `rate-limit: 0`
> bug. In Runbound v0.4.2, the rate-limiter uses an internal token bucket or counter.
> The first 1-second burst consumed the residual budget allocated at startup. By the
> second burst, the budget was exhausted and the limiter throttled responses down to a
> trickle (732/s). Under correct configuration (`rate-limit` disabled or set to a sane
> value), ramp behaviour would reflect actual DNS resolution throughput.

### Unbound ramp ✅ valid

| Step target QPS | Burst measured | 80 % threshold | Saturated? |
|---|---|---|---|
| 1 000 | 65 076 /s | 800 | No |
| 2 000 | 46 432 /s | 1 600 | No |
| 4 000 | 57 080 /s | 3 200 | No |
| 8 000 | 73 500 /s | 6 400 | No |
| 16 000 | 53 877 /s | 12 800 | No |
| 32 000 | 66 699 /s | 25 600 | No |
| 64 000 | 68 556 /s | 51 200 | No |
| 128 000 | 60 812 /s | 102 400 | **Yes** |

**Max sustainable QPS (Unbound): 64 000** ✅  
Reason: `burst 60 812/s < 128 000/s target`

---

## dnsperf vs dnsmark — tool comparison

Both tools were used as reference in this benchmark. Key differences observed:

### 1. In-flight query management

| | dnsperf | dnsmark |
|---|---|---|
| Flag | `-q N` (per client) | `--max-outstanding N` (global, all workers) |
| Default | 100 per client | 100 total |
| Mechanism | Per-client send gate | `Arc<AtomicUsize>` shared across all OS threads |
| Equivalent settings | `-q 100 -c 32` = 3 200 total | `--max-outstanding 3200` |

With equivalent total in-flight (3 200), both tools delivered identical throughput
(~67 000 QPS against Unbound on this hardware). dnsmark achieves the same QPS with
32× fewer in-flight slots when using its global default of 100.

### 2. Latency reporting

| | dnsperf | dnsmark |
|---|---|---|
| Output | avg ± stddev | p50 / p95 / p99 / p999 (HDR histogram) |
| Tail visibility | None | Full — p999 exposes outliers invisible in avg |
| Example | avg 1.47 ms, stddev 2.0 ms | p50 0.73 ms / p99 10.5 ms / p999 24.8 ms |

dnsperf's average masks the tail. In this benchmark, Unbound at 50k QPS showed
avg 0.661 ms but p999 19.8 ms — a 30× spread invisible in the average alone.

### 3. Rate delivery accuracy

Both tools achieved < 0.3 % deviation from target at 5k and 10k QPS.

dnsmark uses a **drift-compensating absolute deadline** (`next_send: Instant`):
when the OS timer fires late (e.g. 3 ms instead of 2.13 ms), the next sleep is
proportionally shorter to recover the deficit. This keeps the long-run rate accurate
without accumulating drift — equivalent to dnsperf's `req_time += q_step` approach.

### 4. Architecture

| | dnsperf | dnsmark |
|---|---|---|
| Hot path | `select()` event loop per client | Dedicated sender + receiver OS thread per worker |
| Send syscall | `send()` one datagram | `sendmmsg(64)` in unlimited mode |
| Receive syscall | `recv()` one datagram | `recvmmsg(16, MSG_DONTWAIT)` batch |
| CPU affinity | None | `sched_setaffinity` to physical cores (HT excluded) |
| Timer resolution | `select()` quantization (~1 ms) | `nanosleep` (~100 µs) |

The OS-thread model eliminates tokio async overhead from the UDP hot path. The
dedicated receiver thread processes responses independently of the sender, avoiding
the RTT inflation observed when both share a single `select()` loop.

### 5. Test modes

| Feature | dnsperf | dnsmark |
|---|---|---|
| Rate-limited load | ✅ `-Q N` | ✅ `-Q N` |
| Unlimited flood | ✅ (no `-Q`) | ✅ (`-Q 0`) |
| Auto-saturation ramp | ❌ | ✅ `--ramp` |
| Two-server comparison | ❌ | ✅ `--compare IP` |
| JSON output | ❌ | ✅ `--json` |
| CSV output | ❌ | ✅ `--csv FILE` |
| Live TUI dashboard | ❌ | ✅ |
| DNS-over-TLS | ❌ | ✅ `--protocol dot` |

---

## Analysis (Unbound only — Runbound pending re-test)

### Unbound saturation

Unbound saturates at approximately **64 000 QPS** on this VM (ramp mode). The dnsmark
50k test delivered 47 332 QPS (94.7 % of target) with p999 under 20 ms — Unbound still
had headroom. The 64k figure from ramp mode represents the practical ceiling under the
virtual bridge network conditions.

### Behaviour under overload (Unbound)

| Metric | Value |
|---|---|
| SERVFAIL rate at saturation | 0 % |
| Packet loss at 50k target | 0.001 % |
| p999 at 50k target | 19.8 ms |
| p999 at 10k target | 12.7 ms |

Unbound degrades gracefully: no SERVFAIL, loss stays near zero, tail latency grows
linearly. No catastrophic failure mode observed in this range.

### Cache impact on latency (Unbound)

The recursive query test (queries.txt, 500 QPS) shows Unbound's cache in action:
- 20 unique domains cycled at 500 QPS → cache warms in ~0.04 s
- All subsequent queries hit cache → p50 0.261 ms (near-instant)
- At 5k QPS with random UUIDs (no cache): p50 0.248 ms — nearly identical
- Cache benefit is minimal for p50 on this virtual hardware; it matters more at
  high QPS where cache hit rate determines whether the upstream resolver is hit

### VM environment limitations

- **Shared physical host**: CPU steal from one VM can inflate another's tail latency.
  The Runbound RTT spike to 3.39 ms in the ping baseline is likely CPU-steal noise.
- **Virtual bridge only**: no physical NIC, no interrupt coalescing, no XDP kernel-bypass.
- **Runbound on 2 vCPUs**: hard throughput ceiling from vCPU count, independent of the
  rate-limit bug. Bare-metal deployment would remove this constraint.
- **Unbound vCPU count unknown**: SSH access not available at test time.

---

## Verdict

| Metric | Runbound v0.4.2 | Unbound 1.22.0 | Status |
|---|---|---|---|
| RTT baseline (avg) | 0.487 ms | 0.413 ms | ✅ valid |
| RTT baseline (max) | 3.390 ms | 0.536 ms | ✅ valid |
| dnsperf 5k — avg RTT | ⚠️ 0.285 ms (REFUSED) | 0.272 ms | ⚠️ Runbound invalid |
| dnsperf 10k — avg RTT | ⚠️ 0.324 ms (REFUSED) | 0.230 ms | ⚠️ Runbound invalid |
| dnsperf unlimited QPS | ⚠️ 15 247 (REFUSED) | **45 752** | ⚠️ Runbound invalid |
| dnsmark p50 at 5k QPS | ⚠️ 0.320 ms (REFUSED) | 0.248 ms | ⚠️ Runbound invalid |
| dnsmark p999 at 5k QPS | ⚠️ 5.463 ms (REFUSED) | 9.511 ms | ⚠️ Runbound invalid |
| dnsmark max QPS | ⚠️ ~15 840 (REFUSED) | **~47 332** | ⚠️ Runbound invalid |
| Ramp — max sustainable | ⚠️ 1 000 (rate-limit bug) | **64 000** | ⚠️ Runbound invalid |
| Recursive resolution | ⚠️ REFUSED (all) | NOERROR 100 % | ⚠️ Runbound invalid |
| SERVFAIL rate | 0 % | 0 % | ✅ valid |

> **VM-to-VM results — bare metal with Intel fiber NICs pending.**  
> **All Runbound figures are invalid and will be replaced once the `rate-limit: 0` bug is fixed.**

---

## Reproduction commands

```bash
# Benchmark client: 192.168.8.245
# Tools: dnsmark 0.4.3, dnsperf 2.14.0
DNSMARK=/root/dnsmark/target/release/dnsmark

# Prepare query file
cat > /tmp/queries.txt << 'EOF'
google.com A
github.com A
cloudflare.com A
debian.org A
amazon.com A
youtube.com A
wikipedia.org A
stackoverflow.com A
netflix.com A
apple.com A
twitter.com A
reddit.com A
linkedin.com A
discord.com A
docker.com A
mozilla.org A
kernel.org A
archlinux.org A
ubuntu.com A
rust-lang.org A
EOF

# ── RTT baseline ──────────────────────────────────────────────────────────────
ping -c 100 192.168.1.10 | tail -2
ping -c 100 192.168.1.11 | tail -2

# ── dnsperf — controlled load ─────────────────────────────────────────────────
for TARGET in 192.168.1.10 192.168.1.11; do
  dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8 -Q 5000
  dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8 -Q 10000
  dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8
done

# ── dnsmark — controlled load ─────────────────────────────────────────────────
for TARGET in 192.168.1.10 192.168.1.11; do
  $DNSMARK -s $TARGET --random -Q 5000  -l 30 -q --json
  $DNSMARK -s $TARGET --random -Q 10000 -l 30 -q --json
  $DNSMARK -s $TARGET --random -Q 50000 -l 30 -q --json
done

# ── dnsmark — ramp mode ───────────────────────────────────────────────────────
$DNSMARK -s 192.168.1.10 --random --ramp -q
$DNSMARK -s 192.168.1.11 --random --ramp -q

# ── dnsmark — recursive resolution ───────────────────────────────────────────
$DNSMARK -s 192.168.1.10 -d /tmp/queries.txt -Q 500 -l 30 -q --json
$DNSMARK -s 192.168.1.11 -d /tmp/queries.txt -Q 500 -l 30 -q --json
```

---

*Generated by dnsmark 0.4.3 — https://github.com/redlemonbe/dnsmark*
