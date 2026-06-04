# dnsmark — FINDINGS (mise à jour v1.7.0 — 2026-06-04)

## État global

| Item | État | Notes |
|------|------|-------|
| SIGABRT teardown (bug 0) | ✅ résolu v1.6.1 | transmute_copy Dst>Src |
| JSON émis en XDP | ✅ résolu v1.7.0 | dépendait du bug 0 |
| Teardown lien UP | ✅ résolu v1.7.0 | wait operstate + restore IP |
| ARP lookup entrées permanentes | ✅ résolu v1.7.0 | `ip neigh show` au lieu de /proc/net/arp |
| queue_count cappé à concurrent | ✅ résolu v1.7.0 | évite qps_per_worker sous-calibré |
| **XDP TX frames ne sortent pas** | ❌ **bloquant** | tx_queue_N_packets=0 malgré sendto=0 |
| Latence UDP 2× | 📋 documenté | architectural, path XDP seul est comparable dnsperf |

## Blocage actuel — XDP TX (diagnostic banc 2026-06-04)

### Symptôme
`queries_completed=0` sur dragonrage (ixgbe 82599, kernel 6.17-pve). Les sendto() AF_XDP retournent 0 (succès) mais `ethtool -S enp33s0f1 | grep tx_queue_0_packets` reste à 0. Les paquets ne sortent pas physiquement.

### Ce qui a fonctionné une fois
Un run RUST_LOG=info -c 16 a donné 652k completions et p50=122µs juste après reset modprobe ixgbe. Les runs suivants donnent systématiquement 0.

### Causes éliminées
- ZC vs COPY mode : DNSMARK_XDP_COPY=1 (copy mode) donne le même résultat
- queue_count : cappé à min(63, 16) = 16, recalibration 100000/16=6250 correcte
- ARP : ip neigh show trouve l'entrée PERMANENT
- eBPF prog : dns_xdp_client.c correct, bpf_redirect_map vers XSKS[rx_queue_index]
- XSKS map : register_socket(q, fd) appelé pour q=0..15
- Descripteurs TX : XdpDesc struct correcte, produce_tx logique correcte
- UMEM setup : XDP_UMEM_REG, fill ring initialisé avec rx_addrs

### Hypothèse non vérifiée
Le teardown ethtool -L combined (supprimé en v1.7.0) réinitialisait les channels NIC. Le run réussi avait 63 queues (état post-modprobe), pas 16. La combinaison RSS=16 + XSKS pour queues 0..15 peut avoir un problème spécifique à l'ixgbe 82599 sur kernel 6.17 où le driver ne traite pas les descripteurs TX AF_XDP après un changement de channel count.

### Prochaine étape à investiguer
1. Tester avec 63 workers sur NIC à 63 queues (état post-modprobe, sans ethtool -L)
2. Vérifier que comp.dequeue_all() retourne des completions (frames traitées par le driver)
3. Comparer l'état de la NIC entre le run réussi et les runs échoués
4. Possibilité : bug kernel 6.17 / driver ixgbe avec AF_XDP ZC sur NUMA node != 0

## Tuning pré-run requis (à faire manuellement avant chaque run XDP)
```bash
modprobe -r ixgbe && sleep 1 && modprobe ixgbe
# attendre operstate=up
ip addr add 10.10.10.1/24 dev enp33s0f1
ip link set enp33s0f1 up
ethtool -A enp33s0f1 rx off tx off
ethtool -G enp33s0f1 rx 4096 tx 4096
ethtool -L enp33s0f1 combined 16
ip neigh replace 10.10.10.2 dev enp33s0f1 lladdr <mac> nud permanent
```

## Chiffres banc v1.7.0 (dragonrage → dragonsage, X520 82599 10G)

| Test | Résultat |
|------|----------|
| dnsperf réf | 99999 qps, avg=54µs, 0 perte |
| dnsmark --no-xdp -Q 100000 | 99934 qps, p50=99µs, p99=156µs |
| dnsmark XDP -Q 100000 (1 run réussi) | 652k done, p50=122µs |
| dnsmark XDP -Q 100000 (runs suivants) | 0 completed (TX blocked) |
| NIC après run XDP | UP avec IP (teardown ok) |
| dnsperf après run XDP | 16k qps (NIC dégradée par XDP) |
