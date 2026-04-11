//! UDP directed-broadcast LAN discovery.
//!
//! # Why not mDNS alone?
//!
//! `libp2p-mdns` joins the IPv4 multicast group `224.0.0.1:5353` on whichever
//! interface the OS picks for the default route.  On multi-homed hosts (two NICs,
//! a VPN adapter, a Docker bridge, etc.) only one interface participates, so two
//! machines on *different* adapters — even on the same physical switch — never
//! see each other's mDNS packets.  Windows Firewall also routinely drops inbound
//! multicast even when outbound is allowed.
//!
//! # How this works
//!
//! Every [`BEACON_INTERVAL`] seconds we:
//!
//! 1. Enumerate every non-loopback IPv4 interface via `if-addrs`.
//! 2. Compute each interface's directed-broadcast address (`ip | ~mask`).
//! 3. Send a small beacon datagram to `<broadcast>:BEACON_PORT` on all of them.
//!
//! Directed broadcasts are forwarded by the kernel per-interface — no multicast
//! group join, no special firewall rules — so every subnet the host is attached to
//! receives the beacon.
//!
//! Simultaneously we listen for beacons from other nodes.  Each newly seen
//! `(PeerId, tcp_port)` pair is emitted through a channel so the main node can
//! dial the peer.
//!
//! The beacon payload is `MAGIC (4 B) || JSON` and is intentionally tiny (~100 B).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::time;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// UDP port used for beacon traffic on every node.  Picked to be outside the
/// IANA well-known and registered ranges while still being < 49152.
pub const BEACON_PORT: u16 = 17_101;

/// How often to re-broadcast on every interface.  2 s gives near-real-time
/// discovery with negligible bandwidth (~200 B × N-interfaces × 0.5 Hz).
const BEACON_INTERVAL: Duration = Duration::from_secs(2);

/// Four-byte protocol magic prepended to every datagram.
/// Drops unrelated UDP packets cheaply without a full parse.
const MAGIC: &[u8; 4] = b"CMP1";

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Beacon {
    /// Our libp2p `PeerId` as a base58/multibase string (via `Display`).
    peer_id: String,
    /// The TCP port our swarm is listening on.  The receiver combines this with
    /// the datagram's source IP to build a dial-able `/ip4/…/tcp/…` Multiaddr.
    tcp_port: u16,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Spawn the LAN beacon background task.
///
/// **Parameters**
/// - `local_peer_id`: our own PeerId — used to filter self-beacons.
/// - `port_rx`: watch channel that delivers our TCP listen port once the swarm
///   has bound a non-loopback address.  The task blocks until `Some(port)`
///   arrives before it starts transmitting beacons.
///
/// **Returns** an unbounded channel that emits `(PeerId, Multiaddr)` for each
/// newly discovered peer.  The caller (the node's event loop) should dial each
/// address as it arrives.
pub fn spawn(
    local_peer_id: PeerId,
    port_rx: watch::Receiver<Option<u16>>,
) -> mpsc::UnboundedReceiver<(PeerId, Multiaddr)> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(local_peer_id, port_rx, tx));
    rx
}

// ---------------------------------------------------------------------------
// Async run loop
// ---------------------------------------------------------------------------

async fn run(
    local_peer_id: PeerId,
    mut port_rx: watch::Receiver<Option<u16>>,
    tx: mpsc::UnboundedSender<(PeerId, Multiaddr)>,
) {
    // ── 1. Wait for the swarm to bind its TCP listen port ─────────────────
    let my_tcp_port: u16 = loop {
        // `changed()` errors only if the sender dropped (node shut down).
        if port_rx.changed().await.is_err() {
            return;
        }
        if let Some(p) = *port_rx.borrow() {
            break p;
        }
    };

    let my_peer_id_str = local_peer_id.to_string();
    let beacon_payload  = build_beacon(&my_peer_id_str, my_tcp_port);

    // ── 2. Create the UDP socket ──────────────────────────────────────────
    //
    // We use socket2 to set SO_REUSEADDR *before* bind so that two instances
    // on the same machine (e.g. dev + prod) can both receive beacons.
    let socket = match make_socket() {
        Ok(s) => s,
        Err(e) => {
            warn!("LAN beacon: socket setup failed: {e} — UDP discovery disabled");
            return;
        }
    };

    // ── 3. Select loop: tick → broadcast; recv → emit ─────────────────────
    let mut ticker   = time::interval(BEACON_INTERVAL);
    // First tick fires immediately — start broadcasting right away.
    let mut recv_buf = vec![0u8; 512];
    // Track (peer_id_str, tcp_port) pairs we've already forwarded so we don't
    // flood the node with duplicate dial requests every 2 seconds.
    let mut seen: std::collections::HashSet<(String, u16)> = std::collections::HashSet::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                send_beacons(&socket, &beacon_payload).await;
            }

            result = socket.recv_from(&mut recv_buf) => {
                match result {
                    Ok((len, from)) => {
                        handle_recv(&recv_buf[..len], from, &my_peer_id_str, &mut seen, &tx);
                    }
                    Err(e) => {
                        debug!("LAN beacon recv: {e}");
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Socket construction
// ---------------------------------------------------------------------------

/// Build a bound, broadcast-enabled UDP socket wrapped in `tokio::net::UdpSocket`.
fn make_socket() -> anyhow::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // SO_REUSEADDR lets multiple processes (or two dev instances) bind the
    // same port.  Must be set before bind().
    sock.set_reuse_address(true)?;

    // SO_BROADCAST is required to send to subnet broadcast addresses.
    sock.set_broadcast(true)?;

    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, BEACON_PORT).into())?;

    // Convert to a non-blocking std socket, then hand off to tokio.
    sock.set_nonblocking(true)?;
    let std_sock: std::net::UdpSocket = sock.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

// ---------------------------------------------------------------------------
// Beacon send
// ---------------------------------------------------------------------------

/// Build the serialised beacon payload: `MAGIC || JSON`.
fn build_beacon(peer_id: &str, tcp_port: u16) -> Vec<u8> {
    let body = serde_json::to_vec(&Beacon {
        peer_id:  peer_id.to_string(),
        tcp_port,
    })
    .expect("Beacon serialisation is infallible");

    let mut out = Vec::with_capacity(MAGIC.len() + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&body);
    out
}

/// Send `payload` to every interface's subnet broadcast address.
async fn send_beacons(socket: &UdpSocket, payload: &[u8]) {
    for bcast in broadcast_addrs() {
        let dst = SocketAddrV4::new(bcast, BEACON_PORT);
        if let Err(e) = socket.send_to(payload, dst).await {
            debug!("LAN beacon → {dst}: {e}");
        }
    }
}

/// Enumerate all non-loopback IPv4 interfaces and compute the directed
/// broadcast address for each one (`host_ip | ~netmask`).
fn broadcast_addrs() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| {
            let if_addrs::IfAddr::V4(v4) = iface.addr else { return None };
            if v4.ip.is_loopback() { return None; }
            let ip   = u32::from(v4.ip);
            let mask = u32::from(v4.netmask);
            Some(Ipv4Addr::from(ip | !mask))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Beacon receive
// ---------------------------------------------------------------------------

/// Parse an incoming datagram.  If it is a valid beacon from a new peer,
/// forward `(PeerId, Multiaddr)` on the channel for the node to dial.
fn handle_recv(
    data:         &[u8],
    from:         SocketAddr,
    my_peer_id:   &str,
    seen:         &mut std::collections::HashSet<(String, u16)>,
    tx:           &mpsc::UnboundedSender<(PeerId, Multiaddr)>,
) {
    // Magic check
    if data.len() <= MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
        return;
    }

    // Deserialise
    let Ok(beacon) = serde_json::from_slice::<Beacon>(&data[MAGIC.len()..]) else {
        return;
    };

    // Ignore self-echoes
    if beacon.peer_id == my_peer_id {
        return;
    }

    // Dedup: only forward each (peer_id, port) pair once per session
    let key = (beacon.peer_id.clone(), beacon.tcp_port);
    if seen.contains(&key) {
        return;
    }

    // Parse PeerId
    let Ok(peer_id) = beacon.peer_id.parse::<PeerId>() else {
        debug!("LAN beacon: unparseable PeerId '{}' from {from}", beacon.peer_id);
        return;
    };

    // Build multiaddr from the *datagram's source IP* (not any self-reported
    // IP) — this is inherently correct because the packet actually came from there.
    let IpAddr::V4(src_ip) = from.ip() else { return };
    let Ok(ma) = format!("/ip4/{src_ip}/tcp/{}", beacon.tcp_port).parse::<Multiaddr>() else {
        return;
    };

    seen.insert(key);
    debug!(%peer_id, %ma, "LAN beacon: new peer");
    let _ = tx.send((peer_id, ma));
}
