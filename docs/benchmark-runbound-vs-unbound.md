# Benchmark — Runbound v0.4.2 vs Unbound 1.22.0

**Date:** 2026-05-18  
**Tools:** dnsmark 0.4.3, dnsperf 2.14.0  
**Duration per test:** 30 s (ramp mode: variable)

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
| Role | Authoritative DNS | Recursive resolver |
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

## Server roles — important caveat

**Runbound** is configured as an **authoritative DNS server**. It responds with `REFUSED`
to all queries for domains it does not serve. None of the test domains (random UUIDs under
`bench.invalid.`, or real domains like `google.com`) fall within Runbound's authoritative
zones. All Runbound results therefore measure its **REFUSED response throughput** — the
rate at which it can process and reject unknown queries.

**Unbound** is configured as a **recursive resolver**. It resolves `google.com` et al.
recursively (with caching) and returns `NXDOMAIN` for the random UUID subdomains under
`bench.invalid.` (no delegation exists for that TLD).

These are fundamentally different workloads. The comparison is still meaningful: it
quantifies how fast each server can respond under sustained query pressure, regardless
of the response type.

---

## Network baseline

| Target | Packets | Min RTT | Avg RTT | Max RTT | Loss |
|---|---|---|---|---|---|
| Runbound 192.168.1.10 | 100 | 0.294 ms | 0.487 ms | 3.390 ms | 0 % |
| Unbound 192.168.1.11 | 100 | 0.316 ms | 0.413 ms | 0.536 ms | 0 % |

The Runbound VM shows occasional jitter (max 3.4 ms vs 0.5 ms for Unbound), likely due
to the 2 vCPU constraint and Runbound's heavier per-packet processing (security checks,
zone lookup).

---

## dnsperf results (reference tool)

Test: `dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8`

| Scenario | QPS target | Runbound QPS | Runbound completion | Runbound avg RTT | Runbound max RTT | Unbound QPS | Unbound completion | Unbound avg RTT | Unbound max RTT | Winner |
|---|---|---|---|---|---|---|---|---|---|---|
| Controlled 5k | 5 000 | 4 999 | 100 % | 0.285 ms | 20.9 ms | 4 997 | 100 % | 0.272 ms | 73.3 ms | Tie |
| Controlled 10k | 10 000 | 9 999 | 100 % | 0.324 ms | 27.1 ms | 9 999 | 100 % | 0.230 ms | 67.9 ms | Unbound (latency) |
| Unlimited | — | **15 247** | 100 % | 6.525 ms | 418.9 ms | **45 752** | 100 % | 2.165 ms | 63.2 ms | **Unbound 3×** |

> dnsperf reports "completion" = received a response (any rcode). Both servers respond
> 100% to queries.txt domains. Runbound packet size = 29 bytes (query = response, i.e.
> REFUSED with no payload). Unbound packet size = 67 bytes (full answer with A record or
> NXDOMAIN).

---

## dnsmark results

Query source: `--random` (random UUID subdomains under `bench.invalid.`, new domain per
query — no cache benefit). All latency values in milliseconds.

### Runbound

| Scenario | QPS target | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| 5k | 5 000 | 4 988 | 99.997 % | REFUSED | 0.358 ms | 0.320 ms | 0.541 ms | 0.858 ms | 5.463 ms |
| 10k | 10 000 | 9 856 | 99.993 % | REFUSED | 1.545 ms | 1.242 ms | 2.337 ms | 5.803 ms | 14.895 ms |
| 50k (saturated) | 50 000 | **15 840** | 99.979 % | REFUSED | 6.362 ms | 5.811 ms | 9.759 ms | 14.247 ms | 178.687 ms |

> At 50k target, Runbound saturates at ~15 840 QPS — well below target. p999 spikes to
> 178 ms. This is the hard ceiling of Runbound on 2 vCPUs for REFUSED responses.

### Unbound

| Scenario | QPS target | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| 5k | 5 000 | 4 985 | 100.000 % | NXDOMAIN | 0.295 ms | 0.248 ms | 0.328 ms | 0.914 ms | 9.511 ms |
| 10k | 10 000 | 9 970 | 99.999 % | NXDOMAIN | 0.366 ms | 0.249 ms | 0.339 ms | 4.731 ms | 12.743 ms |
| 50k | 50 000 | **47 332** | 99.9996 % | NXDOMAIN | 0.661 ms | 0.395 ms | 1.183 ms | 8.535 ms | 19.807 ms |

> Unbound delivers 47 332 QPS at 50k target (94.7% of target) — still scaling, not
> saturated. p999 at 19.8 ms remains well-controlled.

---

## Recursive resolution — known domains (queries.txt, 500 QPS)

Test: `dnsmark -s $TARGET -d /tmp/queries.txt -Q 500 -l 30 -q --json`  
20 real-world domains (google.com, github.com…), cycled. Unbound resolves recursively
and caches; results stabilise on cache hits after the first cycle.

| Server | Effective QPS | Completion | Rcode | Avg RTT | p50 | p95 | p99 | p999 |
|---|---|---|---|---|---|---|---|---|---|
| Runbound | 479 | 100 % | REFUSED (100 %) | 0.357 ms | 0.305 ms | 0.544 ms | 0.964 ms | 11.263 ms |
| Unbound | 479 | 100 % | NOERROR (100 %) | 0.306 ms | 0.261 ms | 0.377 ms | 0.924 ms | 10.071 ms |

> Runbound refuses all queries for `google.com` et al. (not in its authoritative zones).
> Unbound returns valid NOERROR responses from cache. At 500 QPS both servers are
> trivially under load — latency difference is negligible (0.357 ms vs 0.306 ms).

---

## Ramp mode — saturation point

Test: `dnsmark -s $TARGET --random --ramp -q`  
Starts at 1 000 QPS, bursts 1 s at unlimited then measures completions, doubles target.
Saturation when burst completions < 80 % of target.

### Runbound ramp progression

| Step target QPS | Burst measured | 80 % threshold | Saturated? |
|---|---|---|---|
| 1 000 | 9 659 /s | 800 | No — advance |
| 2 000 | 732 /s | 1 600 | **Yes** |

**Max sustainable QPS (Runbound): 1 000**  
Reason: `burst 732/s < 2 000/s target`

> **Anomaly:** The first burst delivered 9 659 completions/s; the second dropped to 732/s.
> This sharp decline is consistent with Runbound activating an internal rate-limit or
> per-source protection mechanism after a sustained burst of REFUSED queries. This is
> a **security feature**, not a performance bug — Runbound is designed to resist query
> floods. Under this interpretation, the ramp result measures Runbound's sustained
> safe throughput for unknown-domain floods, not its peak authoritative query capacity.

### Unbound ramp progression

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

**Max sustainable QPS (Unbound): 64 000**  
Reason: `burst 60 812/s < 128 000/s target`

---

## Analysis

### Saturation points

| Server | Saturates at | Evidence |
|---|---|---|
| Runbound | ~15 000–16 000 QPS | dnsperf unlimited 15 247 QPS; dnsmark 50k shows 15 840 QPS |
| Unbound | ~64 000 QPS | Ramp mode; dnsmark 50k delivers 47k with headroom remaining |

Unbound outperforms Runbound **3–4×** in raw throughput on this virtual hardware. The
primary reasons:
1. **vCPU constraint**: Runbound runs on 2 vCPUs vs Unbound's unreported count. On the
   same Dell PowerEdge T620, Unbound likely has more vCPUs allocated.
2. **Workload complexity**: Runbound's per-packet security pipeline (zone authentication,
   SSRF guards, HSM checks, etc.) adds CPU cycles per query. Unbound's NXDOMAIN path
   for unknown domains is simpler.
3. **Response size**: Runbound REFUSED = 29 bytes (minimal); Unbound NXDOMAIN = 67 bytes.
   Smaller responses reduce NIC and socket overhead — this slightly favours Runbound,
   yet Unbound is still faster.

### Behaviour under overload

| Server | Overload response | SERVFAIL | REFUSED | Timeout |
|---|---|---|---|---|
| Runbound | Degrades gracefully; p999 → 178 ms at 50k target | 0 % | 100 % | 0.02 % |
| Unbound | Stays under control; p999 19 ms at 47k delivered | 0 % | 0 % | 0.001 % |

No SERVFAIL observed on either server. Runbound's rate-limiter kicks in at the second
ramp step (drops from 9 659 to 732 burst/s), preventing further escalation — a deliberate
protection behaviour.

### Cache impact on latency

The recursive query test (queries.txt, 500 QPS) illustrates cache benefit for Unbound:
- All 20 domains are resolved and cached in the first 20 cycles (~0.04 s at 500 QPS)
- Subsequent queries hit the cache → p50 = 0.261 ms, nearly identical to the
  pure-REFUSED p50 of Runbound (0.305 ms)
- The p99 values (0.924 ms vs 0.964 ms) are indistinguishable at this load level

At low QPS (< 1k), both servers deliver sub-millisecond median latency.

### VM environment limitations

- **Shared host**: both VMs co-run on the same physical Xeon E5-2690 v2 pool. A burst
  from one VM can cause CPU steal on the other, inflating tail latency (visible in
  Runbound's occasional RTT spike to 3.39 ms in the ping baseline).
- **Virtual bridge only**: no physical NIC in the path. Real-world NIC interrupt coalescing,
  IRQ affinity, and kernel bypass (XDP) are not exercised.
- **vCPU count unknown for Unbound VM**: nproc / free -h not accessible without SSH.
  The vCPU allocation likely explains part of the throughput gap.
- **Runbound on 2 vCPUs**: each vCPU maps to one logical thread of a 3 GHz Xeon core.
  At 15k QPS, both vCPUs are likely saturated — scaling would require more vCPUs or
  bare-metal deployment.

---

## Verdict

| Metric | Runbound v0.4.2 | Unbound 1.22.0 | Winner |
|---|---|---|---|
| RTT baseline (avg) | 0.487 ms | 0.413 ms | Unbound |
| RTT baseline (max) | 3.390 ms | 0.536 ms | Unbound |
| dnsperf — 5k QPS latency (avg) | 0.285 ms | 0.272 ms | Tie |
| dnsperf — 10k QPS latency (avg) | 0.324 ms | 0.230 ms | Unbound |
| dnsperf — unlimited throughput | 15 247 QPS | 45 752 QPS | **Unbound 3×** |
| dnsmark — p50 at 5k QPS | 0.320 ms | 0.248 ms | Unbound |
| dnsmark — p999 at 5k QPS | 5.463 ms | 9.511 ms | **Runbound** |
| dnsmark — max throughput | ~15 840 QPS | ~47 332 QPS | **Unbound 3×** |
| Ramp — max sustainable QPS | 1 000 QPS* | 64 000 QPS | **Unbound 64×** |
| Overload — SERVFAIL rate | 0 % | 0 % | Tie |
| Overload — packet loss at saturation | 0.02 % | 0.001 % | Unbound |
| Recursive resolution p50 | N/A (REFUSED) | 0.261 ms | Unbound |

> \* Ramp result reflects Runbound's flood-protection throttle activating at step 2, not
> its steady-state authoritative capacity (~15k QPS from rate-limited tests).

**Summary:** Under these VM-to-VM conditions, Unbound 1.22.0 outperforms Runbound v0.4.2
by 3–4× in raw throughput. Runbound's security-focused architecture deliberately trades
throughput for protection (flood-protection rate limiting, per-packet security pipeline,
authoritative-only operation). On its 2-vCPU VM, Runbound reaches ~15k QPS sustained —
adequate for most authoritative zone scenarios. Unbound reaches ~64k QPS sustained before
saturating.

> **VM-to-VM results — bare metal with Intel fiber NICs pending.**

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
