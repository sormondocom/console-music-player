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
use tokio::time;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::p2p::transfer::{sha256_hex, InboundTransfer, CHUNK_SIZE};

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
        cmd_rx:   UnboundedReceiver<P2pCommand>,
        event_tx: UnboundedSender<P2pEvent>,
    ) -> anyhow::Result<()> {
        let keypair = Keypair::generate_ed25519();
        let mut swarm = network::build_swarm(keypair)?;
        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
        swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;

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
            }
        }

        info!("music P2P node shutting down");
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
            }

            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!(%peer_id, "connection established");
                network::add_gossipsub_peer(&mut self.swarm, peer_id);
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

            _ => {}
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
                debug!(%sender_fp, %track_count, "catalog presence");
                if self.keystore.get_by_fingerprint(&sender_fp).is_some() {
                    if let Err(e) = self.publish(MusicKind::CatalogRequest).await {
                        warn!("catalog request failed: {e}");
                    }
                }
            }

            MusicKind::CatalogRequest if verified => {
                if self.keystore.get_by_fingerprint(&sender_fp).is_some() {
                    if let Err(e) = self.publish(MusicKind::CatalogResponse {
                        tracks: self.local_catalog.clone(),
                        page: None,
                        total_pages: None,
                    }).await {
                        warn!("catalog response failed: {e}");
                    }
                }
            }

            MusicKind::CatalogResponse { tracks, .. } if verified => {
                let mut owned: Vec<RemoteTrack> = tracks;
                for t in &mut owned {
                    t.owner_fp   = sender_fp.clone();
                    t.owner_nick = sender_nick.clone();
                }
                let _ = self.event_tx.send(P2pEvent::RemoteCatalogReceived {
                    peer_fp:   sender_fp,
                    peer_nick: sender_nick,
                    tracks:    owned,
                }).ok();
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
