//! Music P2P node — the central async coordinator.
//!
//! Adapted from pgp-chat-core/src/chat/room.rs.
//! Key changes vs. original:
//!   - `RoomCommand`/`ChatNetEvent` → `P2pCommand`/`P2pEvent`
//!   - No room passphrase — gossipsub payloads signed but not room-encrypted
//!     (transport encryption by Noise/QUIC is sufficient; no shared secret needed)
//!   - File/chat variants removed; music-specific variants wired in
//!   - Track transfer and party line stubs (Phases 4-6 will fill these in)

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use libp2p::{
    gossipsub::{self, IdentTopic},
    identity::Keypair,
    kad,
    swarm::SwarmEvent,
    Multiaddr, PeerId,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio::time;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::p2p::transfer::{sha256_hex, InboundTransfer, CHUNK_SIZE};

use libp2p::mdns;

use crate::p2p::{
    identity::PgpIdentity,
    keystore::PeerKeyStore,
    network::{self, MusicBehaviour, MusicBehaviourEvent},
    trust::{NodeInfo, NodeStatus, TrustState},
    wire::{MusicKind, MusicMessage, RemoteTrack, SignedMusicMessage},
    P2pCommand, P2pEvent,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SEEN_MSG_CAPACITY:        usize    = 512;
const EVENT_CHANNEL_CAP:        usize    = 128;
const CMD_CHANNEL_CAP:          usize    = 64;
const STATUS_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);
const NOMINATION_LIFETIME_SECS: u64     = 60;
/// Tracks per CatalogResponse page.  At ~250 B JSON per track this keeps each
/// gossipsub message well under the 4 MB cap even with envelope overhead.
const CATALOG_PAGE_SIZE:        usize    = 300;

/// Gossipsub topic name — all music P2P nodes share one topic per network.
const TOPIC: &str = "cmp-p2p-v1";

// ---------------------------------------------------------------------------
// NodeNomination — lightweight node-side nomination state (not the UI type)
// ---------------------------------------------------------------------------

struct NodeNomination {
    track:      RemoteTrack,
    votes_yes:  std::collections::HashSet<String>,
    created_at: std::time::Instant,
}

impl NodeNomination {
    fn new(track: RemoteTrack) -> Self {
        Self {
            track,
            votes_yes:  std::collections::HashSet::new(),
            created_at: std::time::Instant::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed().as_secs() >= NOMINATION_LIFETIME_SECS
    }
}

// ---------------------------------------------------------------------------
// UpnpLease — tracks an active UPnP port mapping so we can remove it cleanly
// ---------------------------------------------------------------------------

/// Holds the minimal state needed to remove a UPnP port mapping from the
/// router when the node shuts down.
struct UpnpLease {
    gateway:       igd::Gateway,
    external_port: u16,
    local_addr:    SocketAddrV4,
}

// ---------------------------------------------------------------------------
// MusicNode — the async coordinator
// ---------------------------------------------------------------------------

pub struct MusicNode {
    swarm:         libp2p::Swarm<MusicBehaviour>,
    topic:         IdentTopic,
    identity:      PgpIdentity,
    keystore:      PeerKeyStore,
    node_map:      HashMap<String, NodeInfo>,
    revoked_fps:   HashSet<String>,
    event_tx:      UnboundedSender<P2pEvent>,
    cmd_rx:        UnboundedReceiver<P2pCommand>,
    seen_messages: VecDeque<Uuid>,
    /// Our current local catalog — updated via `AnnounceLibrary`.
    local_catalog: Vec<RemoteTrack>,
    /// Maps track UUIDs to local file paths for serving (never transmitted).
    local_paths:   HashMap<Uuid, PathBuf>,
    /// Outbound transfers pending `AcceptTrackRequest`: transfer_id → (path, track, requester_fp).
    pending_outbound: HashMap<Uuid, (PathBuf, RemoteTrack, String)>,
    /// Track IDs we've requested, waiting for a matching `TrackOffer`.
    pending_requests: HashMap<Uuid, String>, // track_id → owner_fp
    /// Active inbound transfers being assembled in memory.
    inbound_transfers: HashMap<Uuid, InboundTransfer>,
    /// Node-side nomination state for vote counting.
    party_nominations: HashMap<Uuid, NodeNomination>,
    /// Nomination IDs originated by this node (so it can broadcast PartyStart on majority).
    my_nominations: HashSet<Uuid>,
    /// Our own confirmed listen multiaddrs (emitted to UI as ListenAddrsUpdated).
    listen_addrs: Vec<Multiaddr>,
    /// Active UPnP port lease — `Some` after a successful router port mapping.
    /// Removed from the router when the node shuts down.
    upnp_lease: Option<UpnpLease>,
    /// Ensures we only attempt UPnP once per session.
    upnp_attempted: bool,
    /// Notifies the LAN beacon task of our TCP listen port once the swarm binds.
    port_tx: watch::Sender<Option<u16>>,
    /// Peers discovered by the LAN beacon task, ready to be dialled.
    beacon_rx: UnboundedReceiver<(PeerId, Multiaddr)>,
    /// Accumulates paginated CatalogResponse pages keyed by sender fingerprint.
    /// Flushed to RemoteCatalogReceived when the final page arrives.
    partial_catalogs: HashMap<String, Vec<RemoteTrack>>,
}

impl MusicNode {
    /// Spawn the node on the tokio runtime.
    ///
    /// The caller should first create a `P2pHandle` via `P2pHandle::channel()`,
    /// then pass the resulting `NodeChannels` here.  The handle is stored in
    /// `App::p2p_node`; the node runs as a background tokio task.
    pub fn spawn(
        identity: PgpIdentity,
        bootstrap_peers: Vec<(PeerId, Multiaddr)>,
        listen_port: Option<u16>,
        cmd_rx:   UnboundedReceiver<P2pCommand>,
        event_tx: UnboundedSender<P2pEvent>,
    ) -> anyhow::Result<()> {
        let keypair = Keypair::generate_ed25519();
        let local_peer_id = keypair.public().to_peer_id();
        let mut swarm = network::build_swarm(keypair)?;

        // Use a fixed port when configured (allows consistent port-forwarding for
        // internet peers); fall back to 0 (random) if unset.
        let port = listen_port.unwrap_or(0);
        swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{port}").parse()?)?;
        swarm.listen_on(format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?)?;

        // LAN beacon: watch channel carries our TCP port to the beacon task once
        // the swarm has bound.  Beacon discoveries come back via `beacon_rx`.
        let (port_tx, port_rx) = watch::channel(None::<u16>);
        let beacon_rx = crate::p2p::lan_beacon::spawn(local_peer_id, port_rx);

        let node = Self {
            swarm,
            topic: IdentTopic::new(TOPIC),
            identity,
            keystore: PeerKeyStore::new(),
            node_map: HashMap::new(),
            revoked_fps: HashSet::new(),
            event_tx,
            cmd_rx,
            seen_messages: VecDeque::with_capacity(SEEN_MSG_CAPACITY),
            local_catalog: Vec::new(),
            local_paths: HashMap::new(),
            pending_outbound: HashMap::new(),
            pending_requests: HashMap::new(),
            inbound_transfers: HashMap::new(),
            party_nominations: HashMap::new(),
            my_nominations: HashSet::new(),
            listen_addrs: Vec::new(),
            upnp_lease: None,
            upnp_attempted: false,
            port_tx,
            beacon_rx,
            partial_catalogs: HashMap::new(),
        };

        tokio::spawn(async move {
            node.run(bootstrap_peers).await;
        });

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Main run loop
    // -----------------------------------------------------------------------

    async fn run(mut self, bootstrap_peers: Vec<(PeerId, Multiaddr)>) {
        if let Err(e) = self.swarm.behaviour_mut().gossipsub.subscribe(&self.topic) {
            warn!("gossipsub subscribe failed: {e}");
        }

        network::bootstrap(&mut self.swarm, &bootstrap_peers);

        // Announce our PGP key
        if let Err(e) = self.publish_announce_key().await {
            warn!("initial key announcement failed: {e}");
        }

        let mut status_ticker = time::interval(STATUS_ANNOUNCE_INTERVAL);
        status_ticker.tick().await; // skip immediate first tick

        loop {
            tokio::select! {
                event = self.swarm.next() => {
                    let Some(event) = event else { break };
                    self.handle_swarm_event(event).await;
                }

                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if self.handle_command(cmd).await { break; }
                        }
                        None => break,
                    }
                }

                _ = status_ticker.tick() => {
                    if let Err(e) = self.publish_status_announce().await {
                        warn!("status announce failed: {e}");
                    }
                    self.prune_expired_nominations().await;
                }

                discovered = self.beacon_rx.recv() => {
                    if let Some((peer_id, addr)) = discovered {
                        // Feed into Kademlia and dial.  The swarm deduplicates
                        // connection attempts, so repeated beacons are harmless.
                        self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                        if let Err(e) = self.swarm.dial(addr.clone()) {
                            debug!(%peer_id, %addr, "LAN beacon dial: {e}");
                        }
                    }
                }
            }
        }

        info!("music P2P node shutting down");

        // Remove the UPnP port mapping we opened, so the router slot is freed
        // immediately rather than waiting for the lease to expire.
        if let Some(lease) = self.upnp_lease.take() {
            info!("UPnP: removing port {} mapping from router", lease.external_port);
            let _ = tokio::task::spawn_blocking(move || {
                match lease.gateway.remove_port(
                    igd::PortMappingProtocol::TCP,
                    lease.external_port,
                ) {
                    Ok(()) => info!("UPnP: port {} removed from router", lease.external_port),
                    Err(e) => warn!("UPnP: failed to remove port {}: {e}", lease.external_port),
                }
            })
            .await;
        }
    }

    // -----------------------------------------------------------------------
    // Command dispatch
    // -----------------------------------------------------------------------

    /// Returns `true` to break the run loop.
    async fn handle_command(&mut self, cmd: P2pCommand) -> bool {
        match cmd {
            P2pCommand::AnnounceKey => {
                if let Err(e) = self.publish_announce_key().await {
                    warn!("key announcement failed: {e}");
                }
            }

            P2pCommand::ApproveKey(fp) => {
                if let Some(nick) = self.keystore.approve(&fp) {
                    info!(%fp, %nick, "peer approved");
                    self.node_map_set_trust(&fp, TrustState::Trusted);
                    let _ = self.publish_announce_key().await;

                    // Trigger mutual catalog exchange with the newly trusted peer.
                    //
                    // The CatalogPresence broadcast at node startup was received
                    // while this peer was still Pending, so the handler silently
                    // dropped it.  Publish fresh messages now (each publish() gets
                    // a new message UUID so the seen-message dedup won't suppress them):
                    //
                    //  1. CatalogPresence — tells them we have tracks; if they've
                    //     already approved us they'll immediately send a CatalogRequest.
                    //  2. CatalogRequest  — asks for their catalog; they'll respond if
                    //     they've approved us (signature verifies against their key).
                    let count = self.local_catalog.len() as u32;
                    let _ = self.event_tx.send(P2pEvent::Info(format!(
                        "{nick} approved — exchanging catalogs…"
                    ))).ok();
                    if let Err(e) = self.publish(MusicKind::CatalogPresence { track_count: count }).await {
                        warn!("post-approve CatalogPresence failed: {e}");
                    }
                    if let Err(e) = self.publish(MusicKind::CatalogRequest).await {
                        warn!("post-approve CatalogRequest failed: {e}");
                    }

                    let _ = self.event_tx.send(P2pEvent::PeerTrusted {
                        fingerprint: fp,
                        nickname: nick,
                    }).ok();
                }
            }

            P2pCommand::DenyKey(fp) => {
                self.keystore.reject(&fp);
                self.node_map_set_trust(&fp, TrustState::Rejected);
                info!(%fp, "peer rejected");
            }

            P2pCommand::GetPeerList => {
                let snapshot: Vec<NodeInfo> = self.node_map.values().cloned().collect();
                let _ = self.event_tx.send(P2pEvent::PeerListSnapshot(snapshot)).ok();
            }

            P2pCommand::Disconnect => {
                return true;
            }

            // ── Library sharing (Phase 4) ──────────────────────────────────
            P2pCommand::AnnounceLibrary(tracks) => {
                let count = tracks.len() as u32;
                self.local_catalog = tracks;
                if let Err(e) = self.publish(MusicKind::CatalogPresence { track_count: count }).await {
                    warn!("catalog presence broadcast failed: {e}");
                }
            }

            P2pCommand::RequestCatalog { peer_fp } => {
                let _ = peer_fp; // gossipsub broadcast; responder filters by trust
                if let Err(e) = self.publish(MusicKind::CatalogRequest).await {
                    warn!("catalog request failed: {e}");
                }
            }

            P2pCommand::ConnectPeer { addr } => {
                match addr.parse::<Multiaddr>() {
                    Ok(ma) => {
                        info!(%ma, "dialling explicit peer");
                        if let Err(e) = self.swarm.dial(ma) {
                            warn!("explicit dial failed: {e}");
                            let _ = self.event_tx.send(P2pEvent::Warning(
                                format!("Could not connect: {e}")
                            )).ok();
                        }
                        // The peer will announce their key via AnnounceKey once connected;
                        // they will appear as Pending and require normal approval.
                    }
                    Err(e) => {
                        warn!("invalid peer address '{addr}': {e}");
                        let _ = self.event_tx.send(P2pEvent::Warning(
                            format!("Invalid address: {e}")
                        )).ok();
                    }
                }
            }

            // ── Track streaming (Phase 5) ──────────────────────────────────
            P2pCommand::SetLocalPaths(paths) => {
                self.local_paths = paths;
            }

            P2pCommand::RequestTrack { track_id, peer_fp } => {
                self.pending_requests.insert(track_id, peer_fp);
                if let Err(e) = self.publish(MusicKind::TrackRequest { track_id }).await {
                    warn!("track request failed: {e}");
                }
            }

            P2pCommand::AcceptTrackRequest { transfer_id } => {
                if let Some((path, track, _requester_fp)) =
                    self.pending_outbound.remove(&transfer_id)
                {
                    let bytes = match tokio::fs::read(&path).await {
                        Ok(b)  => b,
                        Err(e) => {
                            warn!(%transfer_id, "failed to read track for serving: {e}");
                            let _ = self.publish(MusicKind::TrackDecline {
                                transfer_id,
                                reason: format!("server read error: {e}"),
                            }).await;
                            return false;
                        }
                    };
                    let sha256   = sha256_hex(&bytes);
                    let chunks: Vec<&[u8]> = bytes.chunks(CHUNK_SIZE).collect();
                    let total    = chunks.len() as u32;

                    if let Err(e) = self.publish(MusicKind::TrackOffer {
                        transfer_id,
                        track,
                        total_chunks: total,
                    }).await {
                        warn!(%transfer_id, "TrackOffer failed: {e}");
                        return false;
                    }

                    for (index, chunk) in chunks.iter().enumerate() {
                        if let Err(e) = self.publish(MusicKind::TrackChunk {
                            transfer_id,
                            index: index as u32,
                            total,
                            encrypted_data: chunk.to_vec(),
                        }).await {
                            warn!(%transfer_id, index, "TrackChunk failed: {e}");
                            return false;
                        }
                    }

                    if let Err(e) = self.publish(MusicKind::TrackComplete {
                        transfer_id,
                        sha256,
                    }).await {
                        warn!(%transfer_id, "TrackComplete failed: {e}");
                    }
                }
            }

            P2pCommand::DeclineTrackRequest { transfer_id } => {
                self.pending_outbound.remove(&transfer_id);
                if let Err(e) = self.publish(MusicKind::TrackDecline {
                    transfer_id,
                    reason: "declined".to_string(),
                }).await {
                    warn!("track decline failed: {e}");
                }
            }

            // ── Party line (Phase 6) ───────────────────────────────────────
            P2pCommand::NominateTrack(track) => {
                let nomination_id = Uuid::new_v4();
                self.my_nominations.insert(nomination_id);
                self.party_nominations.insert(nomination_id, NodeNomination::new(track.clone()));
                if let Err(e) = self.publish(MusicKind::PartyNominate {
                    nomination_id,
                    track,
                }).await {
                    warn!("party nominate failed: {e}");
                }
            }

            P2pCommand::CastVote { nomination_id, vote } => {
                if let Err(e) = self.publish(MusicKind::PartyVote {
                    nomination_id,
                    vote,
                }).await {
                    warn!("party vote publish failed: {e}");
                }
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Swarm event dispatch
    // -----------------------------------------------------------------------

    async fn handle_swarm_event(&mut self, event: SwarmEvent<MusicBehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "listening");
                // Skip loopback and circuit-relay addresses — not useful for sharing.
                let addr_str = address.to_string();
                if !addr_str.contains("/127.0.0.1") && !addr_str.contains("/::1") {
                    if !self.listen_addrs.contains(&address) {
                        self.listen_addrs.push(address.clone());
                    }
                    // Emit full shareable multiaddrs: address + /p2p/<PeerId>
                    let local_peer_id = *self.swarm.local_peer_id();
                    let addrs: Vec<String> = self.listen_addrs.iter()
                        .map(|a| format!("{}/p2p/{}", a, local_peer_id))
                        .collect();
                    let _ = self.event_tx.send(P2pEvent::ListenAddrsUpdated(addrs)).ok();

                    // Notify the LAN beacon task of our TCP port so it can start
                    // broadcasting.  We only need the first TCP address.
                    if addr_str.contains("/tcp/") {
                        if *self.port_tx.borrow() == None {
                            if let Some(p) = Self::extract_tcp_port(&address) {
                                let _ = self.port_tx.send(Some(p));
                            }
                        }
                        // Attempt UPnP once on that same address.
                        if !self.upnp_attempted {
                            self.try_upnp(&address).await;
                        }
                    }
                }
            }

            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!(%peer_id, "connection established");
                network::add_gossipsub_peer(&mut self.swarm, peer_id);
                // Re-announce our PGP key so the new peer learns our identity.
                // The startup announcement fires before any peers are connected
                // (gossipsub has no subscribers yet), so this is the only
                // reliable path for mutual key exchange on LAN discovery.
                if let Err(e) = self.publish_announce_key().await {
                    debug!(%peer_id, "key re-announce after connect: {e}");
                }
            }

            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                info!(%peer_id, "connection closed");
                if let Some(fp) = self.keystore.fingerprint_for_peer(&peer_id).map(str::to_string) {
                    if let Some(info) = self.node_map.get_mut(&fp) {
                        info.status = NodeStatus::Offline;
                    }
                    let nick = self.node_map.get(&fp)
                        .map(|n| n.nickname.clone())
                        .unwrap_or_default();
                    let _ = self.event_tx.send(P2pEvent::PeerOffline {
                        fingerprint: fp,
                        nickname: nick,
                    }).ok();
                }
            }

            SwarmEvent::Behaviour(bev) => self.handle_behaviour_event(bev).await,

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Behaviour event dispatch
    // -----------------------------------------------------------------------

    async fn handle_behaviour_event(&mut self, event: MusicBehaviourEvent) {
        match event {
            MusicBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            }) => {
                debug!(%propagation_source, "gossipsub message");
                self.handle_gossipsub_message(propagation_source, message.data).await;
            }

            MusicBehaviourEvent::Identify(ref ev) => {
                network::handle_identify_event(&mut self.swarm, ev);
            }

            MusicBehaviourEvent::Kademlia(kad::Event::RoutingUpdated { peer, .. }) => {
                debug!(%peer, "kademlia routing updated");
            }

            MusicBehaviourEvent::Mdns(mdns::Event::Discovered(peers)) => {
                for (peer_id, addr) in peers {
                    info!(%peer_id, %addr, "mDNS: discovered peer");
                    self.swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                    network::add_gossipsub_peer(&mut self.swarm, peer_id);
                    if let Err(e) = self.swarm.dial(addr) {
                        debug!(%peer_id, "mDNS dial failed: {e}");
                    }
                }
            }

            MusicBehaviourEvent::Mdns(mdns::Event::Expired(peers)) => {
                for (peer_id, _addr) in peers {
                    debug!(%peer_id, "mDNS: peer expired");
                    self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                }
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // UPnP — automatic router port mapping
    // -----------------------------------------------------------------------

    /// Attempt to open an inbound TCP port on the home router via UPnP/IGD.
    ///
    /// Called once, on the first non-loopback TCP `NewListenAddr` event.
    /// Emits an `Info` toast on success (with the external IP:port that
    /// internet peers can use) or a `Warning` toast on failure (explaining
    /// what the user needs to do manually).  Always sets `upnp_attempted`
    /// so this path is only taken once per session.
    async fn try_upnp(&mut self, listen_addr: &Multiaddr) {
        self.upnp_attempted = true;

        // Extract the TCP port the OS actually bound (may differ from the
        // configured port when 0 was requested).
        let port = match Self::extract_tcp_port(listen_addr) {
            Some(p) if p != 0 => p,
            _ => {
                debug!("UPnP: skipping — could not extract TCP port from {listen_addr}");
                return;
            }
        };

        // Determine which local IPv4 address sits on the default route.
        // This is the address the router needs to forward traffic to.
        let local_ip = match Self::local_ipv4() {
            Some(ip) => ip,
            None => {
                let _ = self.event_tx.send(P2pEvent::Warning(
                    "UPnP: could not determine local IP address — skipping router port mapping. \
                     Internet peers will not be able to reach you unless you forward a port manually."
                    .to_string(),
                )).ok();
                return;
            }
        };
        let local_addr = SocketAddrV4::new(local_ip, port);
        let description = format!("cmp-p2p port {port}");

        info!("UPnP: attempting to open port {port} on router (local {local_addr})");

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<(igd::Gateway, std::net::Ipv4Addr)> {
            let gateway = igd::search_gateway(igd::SearchOptions {
                timeout: Some(Duration::from_secs(3)),
                ..Default::default()
            })?;
            let external_ip = gateway.get_external_ip()?;
            // lease_duration = 0 → the router keeps the mapping until we
            // explicitly call remove_port() on clean shutdown, or until it
            // reboots.  We always clean up ourselves.
            gateway.add_port(
                igd::PortMappingProtocol::TCP,
                port,
                local_addr,
                0,
                &description,
            )?;
            Ok((gateway, external_ip))
        })
        .await;

        match result {
            Ok(Ok((gateway, external_ip))) => {
                info!("UPnP: port {port} mapped → {external_ip}:{port}");

                // Append the externally-reachable address to the shareable
                // list so it appears in the P2P Peers screen and Connect screen.
                let external_ma: Multiaddr = format!("/ip4/{external_ip}/tcp/{port}")
                    .parse()
                    .unwrap_or_else(|_| listen_addr.clone());
                if !self.listen_addrs.contains(&external_ma) {
                    self.listen_addrs.push(external_ma);
                }
                let local_peer_id = *self.swarm.local_peer_id();
                let addrs: Vec<String> = self.listen_addrs.iter()
                    .map(|a| format!("{}/p2p/{}", a, local_peer_id))
                    .collect();
                let _ = self.event_tx.send(P2pEvent::ListenAddrsUpdated(addrs)).ok();

                // Crystal-clear success message: tell the user exactly what
                // was opened and what it means.
                let _ = self.event_tx.send(P2pEvent::Info(format!(
                    "UPnP: your router opened port {port} — \
                     internet peers can reach you at {external_ip}:{port}"
                ))).ok();

                self.upnp_lease = Some(UpnpLease { gateway, external_port: port, local_addr });
            }

            Ok(Err(e)) => {
                info!("UPnP not available: {e}");
                // Tell the user what failed and what they need to do if they
                // want internet connectivity.
                let _ = self.event_tx.send(P2pEvent::Warning(format!(
                    "UPnP: router did not respond ({e}). \
                     Internet peers cannot connect to you unless you forward \
                     TCP port {port} manually on your router."
                ))).ok();
            }

            Err(join_err) => {
                warn!("UPnP task panicked: {join_err}");
            }
        }
    }

    /// Extract the TCP port number from a libp2p `Multiaddr`.
    fn extract_tcp_port(addr: &Multiaddr) -> Option<u16> {
        use libp2p::multiaddr::Protocol;
        addr.iter().find_map(|p| {
            if let Protocol::Tcp(port) = p { Some(port) } else { None }
        })
    }

    /// Return the local IPv4 address that would be used to reach the internet.
    ///
    /// Uses a UDP connect-without-send trick: asking the OS which source
    /// address it would use to reach 8.8.8.8 reveals the default-route
    /// interface IP without actually sending any traffic.
    fn local_ipv4() -> Option<std::net::Ipv4Addr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("8.8.8.8:80").ok()?;
        match socket.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // Gossipsub message handler
    // -----------------------------------------------------------------------

    async fn handle_gossipsub_message(&mut self, source: PeerId, data: Vec<u8>) {
        // Deserialise
        let signed = match serde_json::from_slice::<SignedMusicMessage>(&data) {
            Ok(s)  => s,
            Err(e) => { debug!("failed to deserialise music message: {e}"); return; }
        };

        // Replay dedup by message ID
        let msg_id = signed.message.id;
        if self.seen_messages.contains(&msg_id) {
            return;
        }
        if self.seen_messages.len() >= SEEN_MSG_CAPACITY {
            self.seen_messages.pop_front();
        }
        self.seen_messages.push_back(msg_id);

        let sender_fp = signed.message.sender_fp.clone();
        let sender_nick = signed.message.sender_nick.clone();

        if self.revoked_fps.contains(&sender_fp) {
            return;
        }

        let is_announce = matches!(signed.message.kind, MusicKind::AnnounceKey { .. });

        // Signature verification (skip for AnnounceKey — key not yet known)
        let verified = if !is_announce {
            self.keystore
                .get_by_fingerprint(&sender_fp)
                .map(|pub_key| {
                    serde_json::to_vec(&signed.message)
                        .ok()
                        .and_then(|bytes| {
                            crate::p2p::crypto::verify_data(&bytes, &signed.signature, pub_key).ok()
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        } else {
            true // AnnounceKey is self-authenticating via fingerprint cross-check
        };

        // Dispatch
        match signed.message.kind {
            MusicKind::AnnounceKey { public_key_armored, nickname } => {
                self.handle_key_announcement(source, &sender_fp, &nickname, &public_key_armored).await;
            }

            MusicKind::StatusAnnounce { status } => {
                self.handle_status_announce(&sender_fp, &sender_nick, status);
            }

            MusicKind::Revoke { fingerprint } if verified => {
                self.handle_revocation(&fingerprint, &sender_nick).await;
            }

            MusicKind::CatalogPresence { track_count } => {
                info!(%sender_fp, %sender_nick, %track_count, "catalog presence received");
                if self.keystore.get_by_fingerprint(&sender_fp).is_some() {
                    let _ = self.event_tx.send(P2pEvent::Info(format!(
                        "{sender_nick} has {track_count} tracks — requesting catalog…"
                    ))).ok();
                    if let Err(e) = self.publish(MusicKind::CatalogRequest).await {
                        warn!("catalog request failed: {e}");
                        let _ = self.event_tx.send(P2pEvent::Warning(format!(
                            "Failed to request catalog from {sender_nick}: {e}"
                        ))).ok();
                    }
                } else {
                    debug!(%sender_fp, "CatalogPresence from untrusted peer — ignored");
                }
            }

            MusicKind::CatalogRequest if verified => {
                if self.keystore.get_by_fingerprint(&sender_fp).is_some() {
                    let catalog = self.local_catalog.clone();
                    let total = catalog.len();
                    let pages: Vec<&[RemoteTrack]> = catalog.chunks(CATALOG_PAGE_SIZE).collect();
                    let total_pages = pages.len() as u32;

                    info!(%sender_nick, total, total_pages, "sending catalog");
                    let _ = self.event_tx.send(P2pEvent::Info(format!(
                        "Sending catalog to {sender_nick} ({total} tracks, {total_pages} page{})…",
                        if total_pages == 1 { "" } else { "s" }
                    ))).ok();

                    for (i, page_tracks) in pages.iter().enumerate() {
                        if let Err(e) = self.publish(MusicKind::CatalogResponse {
                            tracks: page_tracks.to_vec(),
                            page: Some(i as u32),
                            total_pages: Some(total_pages),
                        }).await {
                            warn!(page = i, "catalog page send failed: {e}");
                            let _ = self.event_tx.send(P2pEvent::Warning(format!(
                                "Catalog page {} of {total_pages} failed to send: {e}", i + 1
                            ))).ok();
                            break;
                        }
                    }
                } else {
                    debug!(%sender_fp, "CatalogRequest from untrusted peer — ignored");
                }
            }

            MusicKind::CatalogResponse { tracks, page, total_pages } if verified => {
                let page_num  = page.unwrap_or(0);
                let total_pgs = total_pages.unwrap_or(1);
                let page_len  = tracks.len();

                info!(%sender_nick, page_num, total_pgs, page_len, "catalog page received");

                // Accumulate pages.
                let acc = self.partial_catalogs
                    .entry(sender_fp.clone())
                    .or_default();
                acc.extend(tracks);

                if page_num + 1 < total_pgs {
                    // More pages coming — report progress.
                    let received = acc.len();
                    let _ = self.event_tx.send(P2pEvent::Info(format!(
                        "Receiving catalog from {sender_nick}: page {} of {total_pgs} ({received} tracks so far)…",
                        page_num + 1,
                    ))).ok();
                } else {
                    // Final page — flush the accumulator.
                    let mut owned = self.partial_catalogs.remove(&sender_fp).unwrap_or_default();
                    let total_received = owned.len();
                    for t in &mut owned {
                        t.owner_fp   = sender_fp.clone();
                        t.owner_nick = sender_nick.clone();
                    }
                    info!(%sender_nick, total_received, "catalog complete");
                    let _ = self.event_tx.send(P2pEvent::Info(format!(
                        "Catalog from {sender_nick} complete — {total_received} tracks"
                    ))).ok();
                    let _ = self.event_tx.send(P2pEvent::RemoteCatalogReceived {
                        peer_fp:   sender_fp,
                        peer_nick: sender_nick,
                        tracks:    owned,
                    }).ok();
                }
            }

            MusicKind::TrackRequest { track_id } if verified => {
                // Only serve tracks if the requester is trusted.
                if self.keystore.get_by_fingerprint(&sender_fp).is_none() {
                    return;
                }
                if let (Some(path), Some(track)) = (
                    self.local_paths.get(&track_id).cloned(),
                    self.local_catalog.iter().find(|t| t.id == track_id).cloned(),
                ) {
                    let transfer_id = Uuid::new_v4();
                    self.pending_outbound.insert(
                        transfer_id,
                        (path, track.clone(), sender_fp.clone()),
                    );
                    let _ = self.event_tx.send(P2pEvent::InboundTrackRequest {
                        transfer_id,
                        track,
                        requester_fp: sender_fp,
                    }).ok();
                } else {
                    debug!(%track_id, "TrackRequest for unknown track — ignoring");
                }
            }

            MusicKind::TrackOffer { transfer_id, track, total_chunks } => {
                // Accept if we're waiting for this track.
                if self.pending_requests.remove(&track.id).is_some() {
                    let inbound = InboundTransfer::new(track.clone(), total_chunks);
                    self.inbound_transfers.insert(transfer_id, inbound);
                    // Signal the UI to transition Requesting → Buffering.
                    let _ = self.event_tx.send(P2pEvent::TrackBufferProgress {
                        transfer_id,
                        received: 0,
                        total: track.file_size,
                    }).ok();
                }
            }

            MusicKind::TrackChunk { transfer_id, index, total: _, encrypted_data } => {
                if let Some(inbound) = self.inbound_transfers.get_mut(&transfer_id) {
                    inbound.insert_chunk(index, encrypted_data);
                    let received = inbound.received_bytes();
                    let total    = inbound.track.file_size;
                    let _ = self.event_tx.send(P2pEvent::TrackBufferProgress {
                        transfer_id,
                        received,
                        total,
                    }).ok();
                }
            }

            MusicKind::TrackComplete { transfer_id, sha256 } => {
                if let Some(mut inbound) = self.inbound_transfers.remove(&transfer_id) {
                    inbound.expected_hash = Some(sha256);
                    match inbound.assemble() {
                        Ok(bytes) => {
                            let _ = self.event_tx.send(P2pEvent::TrackBufferReady {
                                transfer_id,
                                bytes,
                                track: inbound.track,
                            }).ok();
                        }
                        Err(reason) => {
                            warn!(%transfer_id, %reason, "track assembly failed");
                            let _ = self.event_tx.send(P2pEvent::TrackTransferFailed {
                                transfer_id,
                                reason,
                            }).ok();
                        }
                    }
                }
            }

            MusicKind::TrackDecline { transfer_id, reason } => {
                self.inbound_transfers.remove(&transfer_id);
                let _ = self.event_tx.send(P2pEvent::TrackTransferFailed {
                    transfer_id,
                    reason,
                }).ok();
            }

            MusicKind::PartyNominate { nomination_id, track } => {
                // Register in node-side state for vote counting (idempotent).
                self.party_nominations
                    .entry(nomination_id)
                    .or_insert_with(|| NodeNomination::new(track.clone()));
                let _ = self.event_tx.send(P2pEvent::TrackNominated {
                    nomination_id,
                    track,
                    nominated_by: sender_nick,
                }).ok();
            }

            MusicKind::PartyVote { nomination_id, vote: ref v } => {
                // Record vote and check for majority.
                let online_trusted = self.online_trusted_count();
                let mut should_start = false;
                if let Some(nom) = self.party_nominations.get_mut(&nomination_id) {
                    if let crate::p2p::wire::PartyVote::Yes = v {
                        nom.votes_yes.insert(sender_fp.clone());
                    }
                    // Majority = more than half of online trusted peers voted yes.
                    // Minimum quorum: 2 peers (can't pass solo).
                    should_start = online_trusted >= 2
                        && nom.votes_yes.len() * 2 > online_trusted
                        && self.my_nominations.contains(&nomination_id);
                }
                if should_start {
                    // Only the nominating node broadcasts PartyStart.
                    let start_at = chrono::Utc::now()
                        + chrono::TimeDelta::seconds(5);
                    let track_opt = self.party_nominations.remove(&nomination_id)
                        .map(|n| n.track);
                    self.my_nominations.remove(&nomination_id);
                    if let Some(track) = track_opt {
                        let _ = self.publish(MusicKind::PartyStart {
                            nomination_id,
                            start_at,
                        }).await;
                        // Also emit to our own UI directly (we won't see our own gossipsub).
                        let _ = self.event_tx.send(P2pEvent::PartyLinePassed {
                            nomination_id,
                            track,
                            start_at,
                        }).ok();
                    }
                }
                let vote = match v { crate::p2p::wire::PartyVote::Yes => crate::p2p::wire::PartyVote::Yes, _ => crate::p2p::wire::PartyVote::No };
                let _ = self.event_tx.send(P2pEvent::VoteReceived {
                    nomination_id,
                    voter_fp: sender_fp,
                    vote,
                }).ok();
            }

            MusicKind::PartyStart { nomination_id, start_at } => {
                // Non-nominating peers receive this and start playback.
                let track = self.party_nominations
                    .remove(&nomination_id)
                    .map(|n| n.track)
                    .unwrap_or_default();
                self.my_nominations.remove(&nomination_id);
                let _ = self.event_tx.send(P2pEvent::PartyLinePassed {
                    nomination_id,
                    track,
                    start_at,
                }).ok();
            }

            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Key announcement
    // -----------------------------------------------------------------------

    async fn handle_key_announcement(
        &mut self,
        peer_id: PeerId,
        announced_fp: &str,
        nickname: &str,
        armored: &str,
    ) {
        use pgp::composed::{Deserializable, SignedPublicKey};
        use pgp::types::KeyTrait;
        use std::io::Cursor;

        // Never process our own key announcement (happens when beacon echoes back).
        if announced_fp == self.identity.fingerprint() {
            return;
        }
        if self.revoked_fps.contains(announced_fp) || self.keystore.is_rejected(announced_fp) {
            return;
        }
        if self.keystore.is_known(announced_fp) {
            return;
        }

        let key = match SignedPublicKey::from_armor_single(Cursor::new(armored.as_bytes())) {
            Ok((k, _))  => k,
            Err(e) => { warn!("failed to parse announced key: {e}"); return; }
        };

        let actual_fp = hex::encode(key.fingerprint());
        if actual_fp != announced_fp {
            warn!(%peer_id, %announced_fp, %actual_fp, "fingerprint mismatch — ignoring");
            let _ = self.event_tx.send(P2pEvent::Warning(format!(
                "Peer {peer_id} key fingerprint mismatch — ignored"
            )));
            return;
        }

        info!(%peer_id, %actual_fp, %nickname, "received key announcement");

        self.keystore.insert_pending(peer_id, actual_fp.clone(), key, nickname.to_string());
        self.node_map_upsert(&actual_fp, nickname, TrustState::Pending, NodeStatus::Online);

        let _ = self.event_tx.send(P2pEvent::PeerApprovalRequired {
            fingerprint: actual_fp,
            nickname: nickname.to_string(),
        }).ok();
    }

    // -----------------------------------------------------------------------
    // Status announce
    // -----------------------------------------------------------------------

    fn handle_status_announce(&mut self, sender_fp: &str, sender_nick: &str, status: NodeStatus) {
        if let Some(info) = self.node_map.get_mut(sender_fp) {
            info.status = status;
            info.last_seen = Utc::now();
        } else {
            self.node_map.insert(sender_fp.to_string(), NodeInfo {
                fingerprint: sender_fp.to_string(),
                nickname:    sender_nick.to_string(),
                trust:       TrustState::Pending,
                status,
                last_seen:   Utc::now(),
            });
        }
    }

    // -----------------------------------------------------------------------
    // Revocation
    // -----------------------------------------------------------------------

    async fn handle_revocation(&mut self, fingerprint: &str, nickname: &str) {
        info!(%fingerprint, %nickname, "peer revoked");
        self.revoked_fps.insert(fingerprint.to_string());
        self.keystore.remove_fingerprint(fingerprint);
        if let Some(info) = self.node_map.get_mut(fingerprint) {
            info.trust  = TrustState::Rejected;
            info.status = NodeStatus::Offline;
        }
        let _ = self.event_tx.send(P2pEvent::PeerOffline {
            fingerprint: fingerprint.to_string(),
            nickname:    nickname.to_string(),
        }).ok();
    }

    // -----------------------------------------------------------------------
    // Node map helpers
    // -----------------------------------------------------------------------

    fn node_map_upsert(&mut self, fp: &str, nick: &str, trust: TrustState, status: NodeStatus) {
        let info = self.node_map.entry(fp.to_string()).or_insert_with(|| NodeInfo {
            fingerprint: fp.to_string(),
            nickname:    nick.to_string(),
            trust:       trust.clone(),
            status:      status.clone(),
            last_seen:   Utc::now(),
        });
        info.trust     = trust;
        info.status    = status;
        info.last_seen = Utc::now();
    }

    fn node_map_set_trust(&mut self, fp: &str, trust: TrustState) {
        if let Some(info) = self.node_map.get_mut(fp) {
            info.trust = trust;
        }
    }

    /// Count currently-online trusted peers.
    fn online_trusted_count(&self) -> usize {
        self.node_map
            .values()
            .filter(|i| i.trust == TrustState::Trusted && i.status == NodeStatus::Online)
            .count()
    }

    /// Prune expired nominations; emit `PartyLineFailed` for each.
    async fn prune_expired_nominations(&mut self) {
        let expired: Vec<Uuid> = self.party_nominations
            .iter()
            .filter(|(_, n)| n.is_expired())
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.party_nominations.remove(&id);
            self.my_nominations.remove(&id);
            let _ = self.event_tx.send(P2pEvent::PartyLineFailed { nomination_id: id }).ok();
        }
    }

    // -----------------------------------------------------------------------
    // Publish helpers
    // -----------------------------------------------------------------------

    async fn publish(&mut self, kind: MusicKind) -> anyhow::Result<()> {
        let msg = MusicMessage {
            id:          Uuid::new_v4(),
            room:        TOPIC.to_string(),
            sender_fp:   self.identity.fingerprint(),
            sender_nick: self.identity.nickname().to_string(),
            timestamp:   Utc::now(),
            kind,
        };

        let msg_bytes  = serde_json::to_vec(&msg)?;
        let signature  = crate::p2p::crypto::sign_data(
            &msg_bytes,
            self.identity.secret_key(),
            self.identity.passphrase_fn(),
        )?;

        let signed = SignedMusicMessage { message: msg, signature };
        let payload = serde_json::to_vec(&signed)?;

        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), payload)
            .map_err(|e| anyhow::anyhow!("gossipsub publish: {e}"))?;
        Ok(())
    }

    async fn publish_announce_key(&mut self) -> anyhow::Result<()> {
        let armored = self.identity.public_key_armored()?;
        self.publish(MusicKind::AnnounceKey {
            public_key_armored: armored,
            nickname: self.identity.nickname().to_string(),
        }).await
    }

    async fn publish_status_announce(&mut self) -> anyhow::Result<()> {
        self.publish(MusicKind::StatusAnnounce {
            status: NodeStatus::Online,
        }).await
    }
}
