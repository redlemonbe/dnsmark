#!/bin/bash
# ============================================================
# dnsmark reference quality — multi-server comparison
# unbound 1.22.0  vs  BIND9 9.20.23 (vm-dr, same rig)
#
# Methodology (see docs/server-comparison-methodology.md):
#   1. forwarding resolver, min-cache-ttl clamped
#   2. pre-warm: corpus × 10K at 5K q/s, 120s (all domains cached)
#   3. wait 10s (cache settled, no upstream in-flight)
#   4. DSD × 3 — reproducibility bracket
#   5. fixed-load latency curve — 9 QPS points
#   6. dnsperf concurrency sweep + 4 fixed QPS points
# NIC truth: separate nic_logger.sh runs on vm-dr (started before this script)
# ============================================================
set -e
# ── Rig parameters — override via environment before running (see methodology §7). ──
#   SERVER      data-plane IP of the DNS server under test (dnsmark/dnsperf target)
#   SERVER_SSH  SSH command to the server HOST, for reading its NIC counters (your key/host)
#   CORPUS      path to the query corpus on the generator
#   DNSMARK     path to the dnsmark binary
DNSMARK="${DNSMARK:-/usr/local/bin/dnsmark}"
CORPUS="${CORPUS:-/root/corpus-dnsperf.txt}"
SERVER="${SERVER:-10.0.0.2}"
SERVER_SSH="${SERVER_SSH:-ssh -o StrictHostKeyChecking=no -o ConnectTimeout=15 -o ServerAliveInterval=10 -o ServerAliveCountMax=3 root@10.0.0.2}"
LOG="${LOG:-/tmp/bench_compare.log}"
exec > "$LOG" 2>&1

ts() { date -u +%s; }

prewarm() {
    echo "  [pre-warm] ${1}: dnsperf 5K q/s × 120s (all 10K domains → cache)"
    dnsperf -s $SERVER -d $CORPUS -c 8 -T 8 -Q 5000 -l 120 2>&1 | \
        grep -E 'Queries per second|Response codes' || true
    echo "  [pre-warm] wait 10s for cache to settle"
    sleep 10
}

run_server() {
    local NAME=$1
    local START_CMD=$2
    local STOP_CMD=$3

    echo ""
    echo "==================================================================="
    echo "SERVER: $NAME"
    echo "==================================================================="

    # stop other server
    $SERVER_SSH "bash -c '${STOP_CMD}'" || true
    sleep 2
    # start this server
    $SERVER_SSH "bash -c '${START_CMD}'"
    sleep 5
    $SERVER_SSH "ss -ulnp | grep 53 | head -2"

    prewarm "$NAME"

    # ── [A] DSD reproducibility × 3 ──────────────────────────────────────
    echo ""
    echo "=== [A] DSD reproducibility (${NAME}, 3 runs) ==="
    for run in 1 2 3; do
        echo ""
        echo "--- DSD run $run/3 [ts=$(ts)] ---"
        $DNSMARK -s $SERVER -d $CORPUS --ramp -c 8 --no-tui 2>&1 | \
            grep -E 'Idle latency|Capacity|Within SLO|Knee bracket'
        echo "  DSD done [ts=$(ts)]"
        sleep 3
    done

    # ── [B] fixed-load latency curve (9 points) ───────────────────────────
    echo ""
    echo "=== [B] Load-latency curve (${NAME}, 9 QPS points) ==="
    for QPS in 50000 100000 150000 200000 250000 290000 310000 330000 380000; do
        echo ""
        echo "--- $QPS q/s [ts=$(ts)] ---"
        $DNSMARK -s $SERVER -d $CORPUS -c 8 -Q $QPS --max-outstanding 200 -l 20 --no-tui 2>&1 | \
            grep -E '^\s+(p50|p99|avg|min):|Send throughput|Round-trip'
        echo "  done [ts=$(ts)]"
    done

    # ── [C] dnsperf: blind concurrency sweep ──────────────────────────────
    echo ""
    echo "=== [C] dnsperf concurrency sweep (${NAME}) ==="
    echo "(shows how the reported QPS varies with -c, before knowing the ceiling)"
    for CONC in 5 20 50 200; do
        echo ""
        echo "--- dnsperf -c $CONC -T $CONC -l 20 [ts=$(ts)] ---"
        dnsperf -s $SERVER -d $CORPUS -c $CONC -T $CONC -l 20 2>&1 | \
            grep -E 'Queries per second|Average Latency|Response codes'
        echo "  done [ts=$(ts)]"
    done

    # ── [D] dnsperf fixed QPS — latency at matching points ────────────────
    echo ""
    echo "=== [D] dnsperf fixed QPS (${NAME}) ==="
    for QPS in 50000 100000 200000 280000; do
        echo ""
        echo "--- dnsperf -Q $QPS -l 20 [ts=$(ts)] ---"
        dnsperf -s $SERVER -d $CORPUS -c 8 -T 8 -Q $QPS -l 20 2>&1 | \
            grep -E 'Queries per second|Average Latency|Latency StdDev'
        echo "  done [ts=$(ts)]"
    done
}

echo "==================================================================="
echo "dnsmark multi-server comparison — $(date -u)"
echo "Rig: generator → server under test at $SERVER (NIC counters via: $SERVER_SSH)"
echo "Tools: $(${DNSMARK} --version 2>&1 | head -1), dnsperf $(dnsperf -v 2>&1 | head -1)"
echo "Corpus: $CORPUS ($(wc -l < $CORPUS) domains)"
echo "==================================================================="

# baseline ping
echo ""
echo "=== [BASELINE] ping RTT ==="
ping -c 10 -i 0.1 $SERVER | tail -2

# ── unbound ──────────────────────────────────────────────────────────────
run_server "unbound-1.22.0" \
    "systemctl restart unbound" \
    "systemctl stop named 2>/dev/null || true"

# ── BIND9 ────────────────────────────────────────────────────────────────
run_server "bind9-9.20.23" \
    "systemctl stop unbound; sleep 2; systemctl restart named" \
    "systemctl stop unbound 2>/dev/null || true"

# ── restore unbound ───────────────────────────────────────────────────────
echo ""
echo "=== [CLEANUP] restoring unbound ==="
$SERVER_SSH "bash -c 'systemctl stop named; sleep 2; systemctl restart unbound'"

echo ""
echo "==================================================================="
echo "Campaign complete — $(date -u)"
echo "==================================================================="
