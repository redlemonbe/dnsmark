// XDP receive path for dnsmark.
//
// Architecture:
//   - N sender OS threads: each owns a regular UDP socket, sends DNS queries,
//     records timestamps in a SHARED in_flight map keyed by a global DNS ID.
//   - 1 XDP receiver OS thread: reads raw Ethernet frames from AF_XDP rings,
//     parses Eth/IP/UDP/DNS headers, matches DNS IDs in the shared in_flight
//     map, and records RTTs in the shared StatsCollector.
//
// The XDP eBPF program intercepts UDP packets with src_port=53 (DNS responses)
// at the NIC driver level and redirects them into the AF_XDP ring buffer,
// bypassing the kernel network stack entirely.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
    OnceLock, Arc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::dns::{build_query, parse_response};
use crate::query::QuerySource;
use crate::stats::StatsCollector;

use super::frame::{self, FrameHeader};
use super::loader::XdpHandle;
use super::socket::{
    XskSocket, create_xsk_socket, get_rx_queue_count, iface_index,
    iface_for_server, default_interface,
    is_virtual_interface, parent_interface,
};
use super::umem::{XdpDesc, FRAME_SIZE, SockaddrXdp};
use super::socket::AF_XDP;

// Ethernet/IP/UDP header sizes (IPv4 with IHL=5 only; packets with options
// are XDP_PASS'd by the eBPF program and never reach this path).
const ETH_HDR:  usize = 14;
const IPV4_HDR: usize = 20;
const IPV6_HDR: usize = 40;
const UDP_HDR:  usize = 8;

const ETH_P_IP:   u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86DD;
const PROTO_UDP:   u8 = 17;

// ── XDP TX state ─────────────────────────────────────────────────────────

/// Per-NIC-queue TX state shared by sender workers assigned to that queue.
/// Sender workers write Ethernet frames into UMEM and submit to the TX ring.
pub struct XdpTxState {
    pub fd:   i32,
    pub tx:   Mutex<super::umem::DescRing>,
    pub comp: Mutex<super::umem::AddrRing>,
    pub pool: Mutex<Vec<u64>>,
    pub hdr:  FrameHeader,
    pub sa:   SockaddrXdp,
    pub area: *mut u8,
}
unsafe impl Send for XdpTxState {}
unsafe impl Sync for XdpTxState {}

/// Set once during `start_xdp_receive_path` for all queues.
/// Sender workers index into this by `worker_id % len`.
static XDP_TX_STATES: OnceLock<Vec<Arc<XdpTxState>>> = OnceLock::new();

const TX_BATCH: usize = 64;

// ── Frame parser ──────────────────────────────────────────────────────────

/// Parse a raw Ethernet frame and return the DNS response ID + rcode,
/// or None if the frame is not a valid DNS response.
fn parse_dns_from_frame(frame: &[u8]) -> Option<(u16, u8)> {
    if frame.len() < ETH_HDR + 2 { return None; }

    let eth_type = u16::from_be_bytes([frame[12], frame[13]]);
    let ip_hdr_len = match eth_type {
        ETH_P_IP => {
            if frame.len() < ETH_HDR + IPV4_HDR { return None; }
            if frame[ETH_HDR + 9] != PROTO_UDP { return None; }
            let ihl = ((frame[ETH_HDR] & 0xF) as usize) * 4;
            if ihl < IPV4_HDR { return None; }
            ihl
        }
        ETH_P_IPV6 => {
            if frame.len() < ETH_HDR + IPV6_HDR { return None; }
            if frame[ETH_HDR + 6] != PROTO_UDP { return None; }
            IPV6_HDR
        }
        _ => return None,
    };

    let dns_off = ETH_HDR + ip_hdr_len + UDP_HDR;
    if frame.len() < dns_off + 12 { return None; }

    parse_response(&frame[dns_off..]).map(|r| (r.id, r.rcode))
}

// ── XDP receiver OS thread ─────────────────────────────────────────────────

fn xdp_receiver_thread(
    sock:            XskSocket,
    in_flight:       Arc<Mutex<HashMap<u16, Instant>>>,
    global_in_flight: Arc<AtomicUsize>,
    stats:           Arc<StatsCollector>,
    shutdown:        Arc<AtomicBool>,
    timeout_dur:     Duration,
) {
    let mut last_timeout_check = Instant::now();

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // poll() with 100 ms timeout so we check shutdown and timeouts regularly.
        let mut pfd = libc::pollfd {
            fd:      sock.fd,
            events:  libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };

        if ready < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted { continue; }
            break; // socket closed or fatal error
        }

        // Drain RX ring.
        let descs = sock.rx.consume_rx();
        if !descs.is_empty() {
            let now = Instant::now();
            let mut completed = 0usize;

            {
                let mut map = in_flight.lock();
                let mut recycle: Vec<u64> = Vec::with_capacity(descs.len());

                for desc in &descs {
                    let frame = unsafe { sock.umem.frame(desc.addr, desc.len as usize) };
                    if let Some((id, rcode)) = parse_dns_from_frame(frame) {
                        if let Some(sent_at) = map.remove(&id) {
                            let rtt_us = now.duration_since(sent_at).as_micros() as u64;
                            stats.record_response(rcode, rtt_us);
                            completed += 1;
                        }
                    }
                    recycle.push(desc.addr);
                }

                // Return frames to fill ring for re-use.
                sock.umem.fill.enqueue_batch(&recycle);
            }

            if completed > 0 {
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                    Some(x.saturating_sub(completed))
                }).ok();
            }
        }

        // Completion ring is drained by XDP TX sender threads.

        // Expire timed-out queries every 10 ms.
        let now = Instant::now();
        if now.duration_since(last_timeout_check) >= Duration::from_millis(10) {
            let mut expired = 0usize;
            let mut map = in_flight.lock();
            map.retain(|_, sent_at| {
                if now.duration_since(*sent_at) > timeout_dur {
                    stats.inc_timeout();
                    expired += 1;
                    false
                } else {
                    true
                }
            });
            if expired > 0 {
                global_in_flight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                    Some(x.saturating_sub(expired))
                }).ok();
            }
            last_timeout_check = now;
        }
    }
}

// ── XDP sender OS thread ───────────────────────────────────────────────────
//
// Identical to udp.rs sender_thread except:
//   - Uses a SHARED in_flight map and GLOBAL DNS ID counter (AtomicU16).
//   - Does not start a receiver — the shared XDP receiver handles all responses.

#[allow(clippy::too_many_arguments)]
fn xdp_sender_thread(
    fd:               i32,
    in_flight:        Arc<Mutex<HashMap<u16, Instant>>>,
    global_in_flight: Arc<AtomicUsize>,
    global_id:        Arc<AtomicU16>,
    query_source:     Arc<dyn QuerySource>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    max_outstanding:  usize,
) {
    let mut next_send  = Instant::now();
    let mut last_qps:   u64 = 0;
    let mut send_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        let qps = qps_per_worker.load(Ordering::Relaxed);

        if qps > 0 {
            if qps != last_qps {
                send_interval = Duration::from_secs_f64(1.0 / qps as f64);
                next_send = Instant::now();
                last_qps = qps;
            }

            if max_outstanding > 0
                && global_in_flight.load(Ordering::Relaxed) >= max_outstanding
            {
                next_send = Instant::now() + send_interval;
                continue;
            }

            let now = Instant::now();
            if now < next_send {
                std::thread::sleep(next_send - now);
                if shutdown.load(Ordering::Relaxed) { break; }
            }
            next_send += send_interval;
            if next_send < Instant::now() { next_send = Instant::now(); }

            let id = global_id.fetch_add(1, Ordering::Relaxed);
            let entry = query_source.next();
            let qbytes = build_query(id, &entry.name, entry.qtype);
            let ret = unsafe {
                libc::send(fd, qbytes.as_ptr() as *const libc::c_void, qbytes.len(), 0)
            };
            if ret >= 0 {
                in_flight.lock().insert(id, Instant::now());
                global_in_flight.fetch_add(1, Ordering::Relaxed);
                stats.inc_sent();
                if verbose {
                    tracing::debug!(id, name = %entry.name, "XDP sent query");
                }
            } else {
                tracing::debug!("XDP UDP send error: {}", std::io::Error::last_os_error());
                stats.inc_error();
            }
        } else {
            // Unlimited mode: burst-send up to headroom in global cap.
            let headroom = if max_outstanding > 0 {
                let cur = global_in_flight.load(Ordering::Relaxed);
                if cur >= max_outstanding {
                    std::thread::yield_now();
                    last_qps = 0;
                    continue;
                }
                (max_outstanding - cur).min(64)
            } else {
                64
            };

            let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(headroom);
            let mut ids:  Vec<u16>     = Vec::with_capacity(headroom);
            for _ in 0..headroom {
                let id = global_id.fetch_add(1, Ordering::Relaxed);
                let entry = query_source.next();
                bufs.push(build_query(id, &entry.name, entry.qtype));
                ids.push(id);
            }

            let mut iovecs: Vec<libc::iovec> = bufs.iter()
                .map(|b| libc::iovec { iov_base: b.as_ptr() as *mut _, iov_len: b.len() })
                .collect();
            let mut msgs: Vec<libc::mmsghdr> = iovecs.iter_mut()
                .map(|iov| {
                    let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
                    m.msg_hdr.msg_iov = iov as *mut _;
                    m.msg_hdr.msg_iovlen = 1;
                    m
                })
                .collect();

            let sent = unsafe {
                libc::sendmmsg(
                    fd, msgs.as_mut_ptr(), msgs.len() as libc::c_uint,
                    libc::MSG_DONTWAIT as _,
                )
            };
            let sent = if sent < 0 { 0usize } else { sent as usize };

            if sent > 0 {
                let send_time = Instant::now();
                let mut map = in_flight.lock();
                for &id in ids.iter().take(sent) {
                    map.insert(id, send_time);
                }
                global_in_flight.fetch_add(sent, Ordering::Relaxed);
                stats.inc_sent_n(sent);
            }

            last_qps = 0;
        }
    }
}

// ── XDP TX sender ─────────────────────────────────────────────────────────

/// Write one DNS query as a complete Ethernet frame to the XDP TX ring.
/// Returns false if the TX pool is empty (caller should yield and retry).
fn xdp_tx_one(state: &XdpTxState, dns: &[u8]) -> bool {
    let frame_addr = match state.pool.lock().pop() {
        Some(a) => a,
        None    => return false,
    };

    let frame_len = unsafe {
        let buf = std::slice::from_raw_parts_mut(
            state.area.add(frame_addr as usize),
            FRAME_SIZE as usize,
        );
        state.hdr.write_frame(buf, dns)
    };

    let desc = XdpDesc { addr: frame_addr, len: frame_len as u32, options: 0 };
    let (enqueued, kick) = {
        let tx = state.tx.lock();
        let n = tx.produce_tx(&[desc]);
        (n, tx.needs_wakeup())
    };
    if enqueued == 0 {
        state.pool.lock().push(frame_addr);
        return false;
    }
    if kick {
        unsafe {
            libc::sendto(
                state.fd,
                std::ptr::null(),
                0,
                libc::MSG_DONTWAIT,
                &state.sa as *const SockaddrXdp as *const libc::sockaddr,
                std::mem::size_of::<SockaddrXdp>() as libc::socklen_t,
            );
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn xdp_tx_sender_thread(
    state:            Arc<XdpTxState>,
    in_flight:        Arc<Mutex<HashMap<u16, Instant>>>,
    global_in_flight: Arc<AtomicUsize>,
    global_id:        Arc<AtomicU16>,
    query_source:     Arc<dyn QuerySource>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    max_outstanding:  usize,
) {
    let mut next_send    = Instant::now();
    let mut last_qps: u64 = 0;
    let mut send_interval = Duration::ZERO;

    loop {
        if shutdown.load(Ordering::Relaxed) { break; }

        // Recycle completed TX frames back to pool
        {
            let done = state.comp.lock().dequeue_all();
            if !done.is_empty() { state.pool.lock().extend_from_slice(&done); }
        }

        let qps = qps_per_worker.load(Ordering::Relaxed);

        if qps > 0 {
            if qps != last_qps {
                send_interval = Duration::from_secs_f64(1.0 / qps as f64);
                next_send = Instant::now();
                last_qps = qps;
            }
            if max_outstanding > 0
                && global_in_flight.load(Ordering::Relaxed) >= max_outstanding
            {
                next_send = Instant::now() + send_interval;
                continue;
            }
            let now = Instant::now();
            if now < next_send {
                std::thread::sleep(next_send - now);
                if shutdown.load(Ordering::Relaxed) { break; }
            }
            next_send += send_interval;
            if next_send < Instant::now() { next_send = Instant::now(); }

            let id    = global_id.fetch_add(1, Ordering::Relaxed);
            let entry = query_source.next();
            let dns   = build_query(id, &entry.name, entry.qtype);
            if xdp_tx_one(&state, &dns) {
                in_flight.lock().insert(id, Instant::now());
                global_in_flight.fetch_add(1, Ordering::Relaxed);
                stats.inc_sent();
                if verbose { tracing::debug!(id, "XDP TX sent query"); }
            }
        } else {
            // Unlimited: burst up to headroom
            let headroom = if max_outstanding > 0 {
                let cur = global_in_flight.load(Ordering::Relaxed);
                if cur >= max_outstanding {
                    std::thread::yield_now();
                    last_qps = 0;
                    continue;
                }
                (max_outstanding - cur).min(TX_BATCH)
            } else {
                TX_BATCH
            };

            let send_time = Instant::now();
            let mut sent  = 0usize;
            {
                let mut map = in_flight.lock();
                for _ in 0..headroom {
                    let id    = global_id.fetch_add(1, Ordering::Relaxed);
                    let entry = query_source.next();
                    let dns   = build_query(id, &entry.name, entry.qtype);
                    if xdp_tx_one(&state, &dns) {
                        map.insert(id, send_time);
                        sent += 1;
                    } else {
                        break; // pool empty
                    }
                }
            }
            if sent > 0 {
                global_in_flight.fetch_add(sent, Ordering::Relaxed);
                stats.inc_sent_n(sent);
            } else {
                std::thread::yield_now();
            }
            last_qps = 0;
        }
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Load the XDP program, create AF_XDP sockets (one per NIC queue),
/// spawn the XDP receiver thread, and return the XdpHandle (RAII).
///
/// If `iface` is a virtual interface (Proxmox bridge, vmbr*, veth, ipvlan,
/// macvlan), the function automatically retries on the physical parent. If no
/// parent is found, XDP is disabled and `Err` is returned so the caller can
/// fall back to the standard UDP receive path.
///
/// The receiver thread exits when `shutdown` is set to true.
/// The XdpHandle must stay alive for the duration of the test.
pub fn start_xdp_receive_path(
    iface:            &str,
    server:           IpAddr,
    server_port:      u16,
    in_flight:        Arc<Mutex<HashMap<u16, Instant>>>,
    global_in_flight: Arc<AtomicUsize>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_dur:      Duration,
) -> Result<XdpHandle, String> {
    if is_virtual_interface(iface) {
        match parent_interface(iface) {
            Some(ref parent) => {
                tracing::warn!(
                    virt = %iface, parent = %parent,
                    "XDP: '{}' is a virtual interface — retrying on parent '{}'",
                    iface, parent
                );
                return do_start_xdp_receive_path(
                    parent, server, server_port,
                    in_flight, global_in_flight, stats, shutdown, timeout_dur,
                );
            }
            None => {
                let msg = format!(
                    "XDP: '{}' is a virtual interface (Proxmox bridge / vmbr / veth) \
                     — XDP disabled, falling back to UDP receive. \
                     For native XDP use the physical NIC directly.",
                    iface
                );
                tracing::warn!("{msg}");
                return Err(msg);
            }
        }
    }
    do_start_xdp_receive_path(iface, server, server_port, in_flight, global_in_flight, stats, shutdown, timeout_dur)
}

fn do_start_xdp_receive_path(
    iface:            &str,
    server:           IpAddr,
    server_port:      u16,
    in_flight:        Arc<Mutex<HashMap<u16, Instant>>>,
    global_in_flight: Arc<AtomicUsize>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_dur:      Duration,
) -> Result<XdpHandle, String> {
    let ifidx = iface_index(iface)
        .ok_or_else(|| format!("interface {iface} not found"))?;

    let mut handle = XdpHandle::load(iface)?;

    // Build Ethernet frame header for XDP TX (IPv4 only; IPv6 falls back to sendmmsg).
    let frame_hdr_opt: Option<FrameHeader> = (|| {
        let IpAddr::V4(dst_ip) = server else { return None; };
        let src_ip  = frame::local_ipv4(iface)?;
        let src_mac = frame::local_mac(iface)?;
        let dst_mac = frame::resolve_server_mac(dst_ip)?;
        tracing::info!(
            "[XDP TX] frame header: src={} dst={} sport=12345 dport={}",
            src_ip, dst_ip, server_port
        );
        Some(FrameHeader::new(src_mac, dst_mac, src_ip, dst_ip, 12345, server_port))
    })();

    let queue_count = get_rx_queue_count(iface);
    let mut tx_states: Vec<Arc<XdpTxState>> = Vec::new();

    for q in 0..queue_count {
        let mut sock = unsafe { create_xsk_socket(ifidx, q, true) }
            .or_else(|_| unsafe { create_xsk_socket(ifidx, q, false) })
            .map_err(|e| format!("AF_XDP socket q={q}: {e}"))?;

        handle.register_socket(q, sock.fd)?;

        // Extract TX ring, completion ring, and frame pool before moving
        // the socket into the receiver thread (which only uses rx + fill).
        if let Some(ref hdr) = frame_hdr_opt {
            let area = sock.umem.ptr_at(0);
            let fd   = sock.fd;
            let sa   = SockaddrXdp {
                sxdp_family:         AF_XDP as u16,
                sxdp_flags:          0,
                sxdp_ifindex:        ifidx,
                sxdp_queue_id:       q,
                sxdp_shared_umem_fd: 0,
            };
            let (tx_ring, comp_ring, tx_pool) = sock.extract_tx();
            tx_states.push(Arc::new(XdpTxState {
                fd,
                tx:   Mutex::new(tx_ring),
                comp: Mutex::new(comp_ring),
                pool: Mutex::new(tx_pool),
                hdr:  hdr.clone(),
                sa,
                area,
            }));
        }

        let ifl = in_flight.clone();
        let gif = global_in_flight.clone();
        let st  = stats.clone();
        let sd  = shutdown.clone();

        std::thread::Builder::new()
            .name(format!("xdp-recv-q{q}"))
            .spawn(move || xdp_receiver_thread(sock, ifl, gif, st, sd, timeout_dur))
            .map_err(|e| format!("thread spawn: {e}"))?;
    }

    // Store TX states globally for sender workers to access.
    if !tx_states.is_empty() {
        XDP_TX_STATES.set(tx_states).ok();
        tracing::info!("[XDP TX] TX ring active on {} queue(s)", queue_count);
    }

    Ok(handle)
}

/// Find the outbound interface for a DNS server IP.
pub fn iface_for_benchmark(server: std::net::IpAddr) -> String {
    iface_for_server(server)
        .or_else(default_interface)
        .unwrap_or_else(|| "eth0".to_string())
}

/// Async wrapper: spawns the XDP sender OS thread (no receiver — XDP handles it).
/// Uses AF_XDP TX ring when available (set by start_xdp_receive_path),
/// otherwise falls back to sendmmsg on a regular UDP socket.
#[allow(clippy::too_many_arguments)]
pub async fn run_xdp_sender_worker(
    server_addr:      SocketAddr,
    query_source:     Arc<dyn QuerySource>,
    stats:            Arc<StatsCollector>,
    shutdown:         Arc<AtomicBool>,
    timeout_ms:       u64,
    qps_per_worker:   Arc<AtomicU64>,
    verbose:          bool,
    worker_id:        usize,
    max_outstanding:  usize,
    global_in_flight: Arc<AtomicUsize>,
    xdp_in_flight:    Arc<Mutex<HashMap<u16, Instant>>>,
    global_id:        Arc<AtomicU16>,
) {
    let _ = timeout_ms;

    // If XDP TX is active, use the TX ring instead of sendmmsg.
    if let Some(states) = XDP_TX_STATES.get() {
        if !states.is_empty() {
            let state = states[worker_id % states.len()].clone();
            let ifl   = xdp_in_flight;
            let gif   = global_in_flight;
            let gid   = global_id;
            let qs    = query_source;
            let st    = stats;
            let sd    = shutdown;
            let qps   = qps_per_worker;
            let sender = std::thread::spawn(move || {
                super::super::pin_to_cpu(worker_id);
                xdp_tx_sender_thread(state, ifl, gif, gid, qs, st, sd, qps, verbose, max_outstanding);
            });
            tokio::task::spawn_blocking(move || { sender.join().ok(); }).await.ok();
            return;
        }
    }

    // Fallback: regular UDP socket + sendmmsg.
    let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => { tracing::error!("XDP sender bind: {e}"); return; }
    };
    if let Err(e) = socket.connect(server_addr) {
        tracing::error!("XDP sender connect: {e}"); return;
    }
    let fd = {
        use std::os::unix::io::AsRawFd;
        socket.as_raw_fd()
    };
    let ifl  = xdp_in_flight;
    let gif  = global_in_flight;
    let gid  = global_id;
    let qs   = query_source;
    let st   = stats;
    let sd   = shutdown;
    let qps  = qps_per_worker;
    let sender = std::thread::spawn(move || {
        super::super::pin_to_cpu(worker_id);
        let _sock = socket;
        xdp_sender_thread(fd, ifl, gif, gid, qs, st, sd, qps, verbose, max_outstanding);
    });
    tokio::task::spawn_blocking(move || { sender.join().ok(); }).await.ok();
}
