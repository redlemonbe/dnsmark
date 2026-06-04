# dnsmark — Findings & Bug Analysis (banc X520 10G)

> Branche de fix : `fix/reliability-5bugs` — commit `a4814c8` — v1.4.0

## Bug 1 — Latence 2× sur-évaluée (XDP unified path)

**Symptôme** : À -Q 100000 contre unbound — dnsperf p50=50µs/p99=66µs, dnsmark p50=94µs/p99=148µs. Écart ~44µs.

**Cause** : Dans `xdp_unified_worker`, `in_flight.insert(id)` était appelé dans la boucle de construction des `descs`, AVANT `sock.tx.produce_tx()` + `sendto()` kick. Le timestamp incluait :
- Le temps de remplissage du batch complet (jusqu'à TX_BATCH=64 frames)
- Le `produce_tx()` (mmap ring write)
- Le `sendto()` kick

**Fix** : `ids_batch: Vec<u16>` parallèle à `descs[]` stocke les IDs. `in_flight.insert()` est appelé APRÈS le kick `sendto()`, uniquement pour les `enq` frames qui ont réellement atteint le TX ring (pas les débordements retournés au pool). Sémantique identique à dnsperf ("timestamp just before sendmsg").

**Critère de validation** : p50/p99 à ±5% de dnsperf à 50k/100k/200k QPS.

---

## Bug 2 — -Q rate-limit cassé sur XDP (queries_sent=0)

**Symptôme** : `dnsmark -Q 100000` avec XDP → `queries_sent=0`, rien ne part sur le fil.

**Causes combinées** :

### 2a — max_outstanding défaut trop bas (100 → 1000)
L'ancien défaut de 100 causait un spin-deadlock dans le chemin XDP :
1. Worker envoie jusqu'à `global_in_flight = 100`
2. Gate `max_outstanding` atteinte → `continue` sans `yield_now()`
3. Spin loop CPU 100% → le receiver du même worker ne peut pas drainer RX
4. `global_in_flight` ne décroît jamais → deadlock
5. `local_sent < FLUSH_N=1024` → `stats.sent` reste 0 → "queries_sent=0"

Fix : défaut porté à 1000 (= dnsperf default `-q`), `yield_now()` ajouté sur tous les chemins de back-pressure.

### 2b — Token bucket last_refill non mis à jour en mode flood
Si `qps == 0` (flood), `last_refill` n'était pas mis à jour. Au passage flood→rate-limited, `duration_since(last_refill)` = durée du run flood → burst massif de tokens.

Fix : `last_refill` toujours mis à jour en début d'itération (avant le `if qps > 0`).

### 2c — Back-pressure gate sans yield
Dans `xdp_tx_sender_thread` et `xdp_sender_thread`, la gate `global_in_flight >= max_outstanding` faisait `continue` sans `yield_now()` → spin pur.

---

## Bug 3 — Contention histogram Mutex / sous-comptage

**Symptôme** : Débit `queries_completed` s'effondre à ~1-2M même quand le serveur répond à 8M+.

**Cause** : `Mutex<Histogram<u64>>` unique dans `StatsCollector`, tapée par TOUS les workers pour chaque réponse. À 8-14 Mpps → 8-14M lock/unlock/sec sur une seule Mutex → contention quadratique en nombre de workers.

**Fix** : `ShardedHistogram` — 64 shards `Mutex<Histogram>` indépendants, indexés par `worker_id % 64`. Zéro contention cross-shard. Nouveau point d'entrée `record_response_sharded(rcode, rtt, worker_id)` utilisé dans tous les chemins XDP. `record_response()` maintenu pour rétrocompat (UDP/TCP/DoT → shard 0).

**Merge** : `snapshot()` fusionne les 64 shards via `hdrhistogram::Histogram::add()` — exact, pas d'approximation.

---

## Bug 4 — Sous-comptage RX 6× en saturation

**Symptôme** : `queries_completed ≈ 1M` alors que `ethtool -S nic tx_packets = 6.8M`.

**Cause** : Conséquence directe des bugs 2 (deadlock gate → peu de paquets envoyés) et 3 (Mutex histogram → completions non comptées). Résolu par les fixes des bugs 2 et 3.

**Note** : Vérifier le débit RÉEL toujours via les compteurs NIC du récepteur (`ethtool -S <nic>` delta `tx_bytes_nic`), pas uniquement via `queries_completed` de dnsmark.

---

## Bug 5 — Teardown XDP wedge la NIC (dnsperf suivant → 16k/s)

**Symptôme** : Après un run XDP, la NIC ixgbe (enp33s0f1) reste dans un état dégradé. dnsperf suivant : ~16k/s au lieu de line-rate.

**Cause** : Le drop de `XdpHandle` (aya) détache le prog XDP, mais l'état AF_XDP résiduel dans le driver ixgbe/i40e (descripteurs, RSS, queues DMA) persiste. La NIC n'est plus utilisable normalement jusqu'à un bounce du lien.

**Fix** : `XdpHandle` stocke maintenant le nom d'iface. Son `impl Drop` effectue un bounce `SIOCSIFFLAGS` (down 50ms + up) via ioctl direct APRÈS que aya détache le prog XDP. Purge l'état XDP résiduel du driver. Best-effort : `WARN` si ioctl échoue (permissions), jamais de panic.

```rust
// Dans XdpHandle::drop() :
// 1. _bpf dropped → aya détache XDP
// 2. SIOCSIFFLAGS down (50ms)
// 3. SIOCSIFFLAGS up
// → NIC restaurée pour le run suivant
```

---

## Résultats attendus après fix

| Métrique | Avant (v1.3.0) | Après (v1.4.0) | Critère |
|----------|----------------|----------------|---------|
| Latence p50 @ 100k QPS | ~94µs | ~50µs (±5%) | ±5% vs dnsperf |
| Latence p99 @ 100k QPS | ~148µs | ~66µs (±5%) | ±5% vs dnsperf |
| -Q en mode XDP | queries_sent=0 | OK | Débit cible atteint |
| queries_completed @ 8M QPS | ~1M (6× sous-comp.) | ~8M | ±qq% NIC tx_packets |
| NIC post-run XDP | Wedgée (16k) | Propre (line-rate) | dnsperf immédiat OK |

---

## Validation finale (à effectuer sur banc)

```bash
# Côte-à-côte dnsmark vs dnsperf, 3 paliers
for qps in 50000 100000 200000; do
  dnsperf -s $SERVER -d queries.txt -Q $qps -l 30 -c 1
  dnsmark -s $SERVER -f queries.txt -Q $qps -l 30 -c 16
done

# Ramp sur serveur rapide + vérification NIC
dnsmark -s $SERVER -f queries.txt --ramp -c 16 -l 60
ethtool -S enp33s0f1 | grep tx_bytes_nic  # compteur NIC récepteur

# Vérifier NIC propre après run
dnsperf -s $SERVER -d queries.txt -Q 100000 -l 5  # doit donner ~100k, pas 16k
```
