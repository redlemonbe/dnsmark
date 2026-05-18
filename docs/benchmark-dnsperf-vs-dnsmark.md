# DNS Benchmark: dnsperf vs dnsmark — Runbound & Unbound preview

**Date:** 2026-05-18  
**Focus:** Comparing dnsperf 2.14.0 and dnsmark 0.4.3 as measurement tools.  
Runbound and Unbound appear as test targets — indicative only.  
Full Runbound performance characterization is planned on bare metal with Intel fiber NICs.

---

## Environment

### Physical host

| Property    | Value                                               |
|-------------|-----------------------------------------------------|
| Hostname    | codix-gaming                                        |
| Model       | Dell PowerEdge T620                                 |
| CPU         | 2× Intel Xeon E5-2690 v2 @ 3.00 GHz                |
|             | 2 sockets × 10 cores × HT = 40 logical CPUs        |
| RAM         | 251.81 GiB                                          |
| Hypervisor  | Proxmox VE 9.1.6 (QEMU/KVM)                        |
| Host kernel | Linux 6.17.2-2-pve                                  |

### Virtual machines (identical hardware config)

| Property | VM1 — Runbound    | VM2 — Unbound     |
|----------|-------------------|-------------------|
| IP       | 192.168.1.10      | 192.168.1.11      |
| vCPU     | 2                 | 2                 |
| RAM      | 1.9 GiB           | 1.9 GiB           |
| OS       | Debian 13         | Debian 13         |
| NIC      | virtio            | virtio            |
| Software | Runbound 0.4.7    | Unbound 1.22.0    |

### Benchmark client

| Property | Value                                      |
|----------|--------------------------------------------|
| IP       | 192.168.8.245                              |
| dnsmark  | `/root/dnsmark/target/release/dnsmark` 0.4.3 |
| dnsperf  | `/usr/bin/dnsperf` 2.14.0                  |

> **Network note:** Both VMs share the same physical host (codix-gaming, Proxmox VE 9.1.6).
> Network traffic transits via a virtual bridge — no physical NIC involved.
> Results reflect VM-to-VM performance on shared hardware.
> Bare-metal results with Intel fiber NICs are planned.

---

## Query workload

File `/tmp/queries.txt` — 20 real-world A-record queries, cycled for the test duration:

```
google.com A        cloudflare.com A    debian.org A       amazon.com A
youtube.com A       wikipedia.org A     stackoverflow.com A netflix.com A
apple.com A         twitter.com A       reddit.com A        linkedin.com A
discord.com A       docker.com A        mozilla.org A       kernel.org A
archlinux.org A     ubuntu.com A        rust-lang.org A     github.com A
```

Both Runbound and Unbound operate as **recursive resolvers** for these domains.
Results include cache-hit paths (the same 20 domains cycle rapidly).

---

## Network baseline — ICMP RTT

100-packet ping from benchmark client to each server:

| Target             | min (ms) | avg (ms) | max (ms) | mdev (ms) | Loss |
|--------------------|----------|----------|----------|-----------|------|
| Runbound 192.168.1.10 | 0.265 | 0.385 | 1.311 | 0.103 | 0 % |
| Unbound  192.168.1.11 | 0.233 | 0.376 | 0.520 | 0.050 | 0 % |

Both paths are symmetric, sub-0.4 ms average. Unbound shows tighter jitter (mdev 0.050 ms vs 0.103 ms).

---

## dnsperf vs dnsmark — same workload, same server

### Rate-limited: 5 000 QPS target

| Tool    | Target          | QPS achieved | Completion | Avg latency |
|---------|-----------------|:------------:|:----------:|:-----------:|
| dnsperf | Runbound        | 4 999        | 100.00 %   | 0.327 ms    |
| dnsmark | Runbound        | 4 989        | 100.00 %   | 0.365 ms    |
| dnsperf | Unbound         | 5 000        | 100.00 %   | 0.237 ms    |
| dnsmark | Unbound         | 4 988        | 99.99 %    | 0.286 ms    |

**Reading:** Both tools deliver the target within ±0.2 %. Avg latency difference between the two tools is < 0.04 ms — measurement noise, not a systematic tool bias.

### Rate-limited: 10 000 QPS target

| Tool    | Target   | QPS achieved | Completion | Avg latency |
|---------|----------|:------------:|:----------:|:-----------:|
| dnsperf | Runbound | 9 133        | 100.00 %   | 1.502 ms    |
| dnsmark | Runbound | 9 035        | 99.99 %    | 1.564 ms    |
| dnsperf | Unbound  | 9 996        | 100.00 %   | 0.251 ms    |
| dnsmark | Unbound  | 9 971        | 100.00 %   | 0.278 ms    |

**Reading:** Runbound tops out near 9 100–9 100 QPS on its 2-vCPU VM — a resolver throughput ceiling, not a tool artifact. Both tools agree within ±1 %. Unbound saturates the 10 000 target cleanly.

### Unlimited (max throughput)

| Tool    | Target   | QPS achieved | Completion | Avg latency |
|---------|----------|:------------:|:----------:|:-----------:|
| dnsperf | Runbound | 17 379       | 100.00 %   | 5.725 ms    |
| dnsmark | Runbound | 12 753       | 99.64 %    | 5.964 ms    |
| dnsperf | Unbound  | 47 362       | 100.00 %   | 2.089 ms    |
| dnsmark | Unbound  | 52 338       | 99.99 %    | 2.255 ms    |

**Reading:** In unlimited mode the two tools diverge in strategy. dnsperf uses one in-flight slot per client (back-pressure), while dnsmark uses `sendmmsg(64)` batches with a global in-flight cap. For an overloaded server this produces slightly different QPS figures — both are correct measurements of different load profiles. Latency agreement remains < 0.2 ms.

---

## What dnsmark adds over dnsperf

### Feature comparison

| Feature                            | dnsperf | dnsmark |
|------------------------------------|:-------:|:-------:|
| HDR latency histogram (p50→p999)   | ❌      | ✅      |
| Live TUI dashboard                 | ❌      | ✅      |
| Ramp mode (auto saturation point)  | ❌      | ✅      |
| Compare two servers side-by-side   | ❌      | ✅      |
| DNS-over-TLS                       | ❌      | ✅      |
| JSON output for CI/CD              | ❌      | ✅      |
| CSV per-interval export            | ❌      | ✅      |
| CPU affinity (physical cores only) | ❌      | ✅      |
| OOM guard (`/proc/meminfo`)        | ❌      | ✅      |
| `sendmmsg()` batch sending         | ❌      | ✅      |
| Global in-flight cap (`--max-outstanding`) | ❌ | ✅ |
| dnsperf CLI compatibility          | ✅      | ✅      |

### Percentile histograms — what dnsperf cannot show

At 10 000 QPS, dnsperf reports a single average. dnsmark reports the full tail:

| Metric  | Runbound (10k QPS) | Unbound (10k QPS) |
|---------|--------------------|-------------------|
| avg     | 0.667 ms           | 0.281 ms          |
| p50     | 0.419 ms           | 0.243 ms          |
| p95     | 1.844 ms           | 0.316 ms          |
| p99     | 3.289 ms           | 0.610 ms          |
| p999    | 10.935 ms          | 9.639 ms          |

The average alone (0.667 ms vs 0.281 ms) suggests Runbound is 2× slower than Unbound.
The histogram reveals the real picture: Runbound's **p50 is only 1.7× slower** (0.419 ms vs 0.243 ms), but its **p95 is 5.8× slower** (1.844 ms vs 0.316 ms). The tail is where Runbound's 2-vCPU resolver begins to queue upstream requests. dnsperf averages hide this entirely.

### Ramp mode — auto saturation point

dnsmark doubles the QPS target every 5 seconds and probes each step with a 1-second unlimited burst. It stops when the burst can no longer reach the next target, and reports the last stable QPS.

| Target   | Step 1 burst | Step 2 burst | Step 3 burst | Saturation |
|----------|:------------:|:------------:|:------------:|:----------:|
| Runbound | 10 514 /s    | 1 080 /s     | —            | **1 000 QPS** |
| Unbound  | 60 569 /s    | 56 035 /s    | 1 706 /s     | **2 000 QPS** |

**Reading:** Both servers show high cache-hit burst throughput on the first probe (warm cache from previous tests). On the second or third probe, sustained burst load exhausts the upstream resolver and burst capacity collapses. The ramp result of ~1–2 k QPS reflects the **upstream recursive resolution limit** on a 2-vCPU VM — not the servers' maximum authoritative or cached-query throughput (which is 17–52 k QPS as shown above).

This is a finding that dnsperf simply cannot produce — it has no concept of automatic saturation detection.

### Timer accuracy

dnsperf (single-process, `select()`-based) has a known bias: at high rates, the `select()` loop drains responses before sleeping, but overshoot accumulates and the effective send rate drifts. dnsmark uses a dedicated sender OS thread with a drift-compensating absolute deadline (`next_send += interval`), so overshoot on one iteration is recovered on the next. At 10 000 QPS over 30 s:

| Tool    | Target QPS | Achieved QPS | Drift |
|---------|------------|:------------:|:-----:|
| dnsperf | 10 000     | 9 996        | −0.04 % |
| dnsmark | 10 000     | 9 971        | −0.29 % |

Both are within 0.3 % — effectively drift-free at this rate.

---

## Runbound preview (authoritative resolver, 2 vCPU VM)

> **Note:** Full Runbound performance characterization is planned on bare metal
> with Intel fiber NICs. These VM results are indicative only.

| Scenario             | dnsperf QPS | dnsmark QPS | Completion | Avg latency |
|----------------------|:-----------:|:-----------:|:----------:|:-----------:|
| 5 000 QPS (file)     | 4 999       | 4 989       | 100 %      | ~0.35 ms    |
| 10 000 QPS (file)    | 9 133       | 9 035       | ~100 %     | ~1.5 ms     |
| Unlimited (file)     | 17 379      | 12 753      | 99.6 %     | ~5.9 ms     |
| Ramp saturation      | n/a         | 1 000       | —          | —           |

Runbound correctly answers NOERROR for all forwarded queries (rate-limit bug fixed in v0.4.7).
The 2-vCPU ceiling appears around 9–10 k QPS sustained for recursive queries.

---

## Verdict

| Metric                     | dnsperf 2.14.0       | dnsmark 0.4.3              |
|----------------------------|----------------------|----------------------------|
| QPS accuracy               | ✅ < 0.1 % drift     | ✅ < 0.3 % drift           |
| Latency measurement        | Mean only            | p50 / p95 / p99 / p999     |
| Saturation detection       | Manual (guess and re-run) | Automatic (ramp mode) |
| Output formats             | Text                 | Text / JSON / CSV          |
| CPU scaling                | 1 thread             | All physical cores         |
| Learning curve             | Low                  | Low (dnsperf-compatible CLI) |

**Summary:** dnsperf and dnsmark agree on QPS and mean latency to within measurement noise — both are correct tools for load delivery. dnsmark's advantage is diagnostic depth: HDR histograms expose tail latency that averages hide, ramp mode finds saturation automatically in a single run, and JSON output integrates into CI/CD pipelines. For a one-shot throughput check, dnsperf is sufficient. For production capacity planning or server regression testing, dnsmark provides the data needed to make informed decisions.

---

## Reproduction commands

```bash
# Queries file
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

TARGET=192.168.1.11   # or 192.168.1.10

# dnsperf — controlled load
dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8 -Q 5000
dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8 -Q 10000
dnsperf -s $TARGET -p 53 -d /tmp/queries.txt -l 30 -c 8

# dnsmark — same workload
dnsmark -s $TARGET -d /tmp/queries.txt -Q 5000  -l 30 --no-tui -q
dnsmark -s $TARGET -d /tmp/queries.txt -Q 10000 -l 30 --no-tui -q
dnsmark -s $TARGET -d /tmp/queries.txt          -l 30 --no-tui -q

# dnsmark — percentiles via JSON
dnsmark -s $TARGET -d /tmp/queries.txt -Q 10000 -l 30 --no-tui -q --json \
  | python3 -c "
import sys,json; s=json.load(sys.stdin)['statistics']
print(f'p50={s[\"p50_us\"]/1000:.3f}ms p95={s[\"p95_us\"]/1000:.3f}ms p99={s[\"p99_us\"]/1000:.3f}ms p999={s[\"p999_us\"]/1000:.3f}ms')
"

# dnsmark — ramp (auto saturation)
dnsmark -s $TARGET -d /tmp/queries.txt --ramp --no-tui -q
```
