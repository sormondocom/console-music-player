//! libp2p transport + behaviour + peer-discovery helpers for the music P2P node.
//!
//! Adapted from pgp-chat-core/src/network/{transport,behaviour,peer_discovery}.rs.
//! Changes vs. original:
//!   - `ChatBehaviour` → `MusicBehaviour` / `MusicBehaviourEvent`
//!   - Identify protocol ID: `/cmp-p2p/1.0.0`
//!   - Agent version: `cmp/{CARGO_PKG_VERSION}`
//!   - Error type: `anyhow` instead of the chat crate's custom error

use std::time::Duration;

use libp2p::{
    gossipsub, identify, kad, mdns, noise, tcp, yamux,
    identity::Keypair,
    swarm::{NetworkBehaviour, Swarm},
    PeerId, SwarmBuilder,
};

// ---------------------------------------------------------------------------
// Network constants
// ---------------------------------------------------------------------------

const GOSSIPSUB_HEARTBEAT:       Duration = Duration::from_secs(10);
const IDLE_CONNECTION_TIMEOUT:   Duration = Duration::from_secs(60);
const IDENTIFY_PROTOCOL:         &str     = "/cmp-p2p/1.0.0";

/// Gossipsub per-message size cap.  The default (65 536 B) is far too small
/// for a catalog page of 300 tracks (~75 KB JSON + envelope).  4 MB gives
/// comfortable headroom while staying well below typical MTU stacks.
const MAX_TRANSMIT_SIZE: usize = 4 * 1024 * 1024; // 4 MB

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

/// The combined libp2p `NetworkBehaviour` for a music P2P node.
///
/// | Field       | Protocol  | Purpose                                      |
/// |-------------|-----------|----------------------------------------------|
/// | `gossipsub` | GossipSub | Broadcast announce / nominate / vote msgs    |
/// | `kademlia`  | Kademlia  | Distributed peer discovery (DHT)             |
/// | `identify`  | Identify  | Multiaddr exchange; feeds Kademlia            |
/// | `mdns`      | mDNS      | Zero-config LAN peer discovery (no bootstrap)|
#[derive(NetworkBehaviour)]
pub struct MusicBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia:  kad::Behaviour<kad::store::MemoryStore>,
    pub identify:  identify::Behaviour,
    pub mdns:      mdns::tokio::Behaviour,
}

// The macro generates `pub enum MusicBehaviourEvent` with variants:
//   Gossipsub(gossipsub::Event)
//   Kademlia(kad::Event)
//   Identify(identify::Event)

// ---------------------------------------------------------------------------
// Swarm builder
// ---------------------------------------------------------------------------

/// Build a ready-to-use libp2p swarm with TCP + QUIC transports.
///
/// The swarm is configured but not yet listening — call
/// `swarm.listen_on(addr)` after construction.
pub fn build_swarm(keypair: Keypair) -> anyhow::Result<Swarm<MusicBehaviour>> {
    let peer_id = keypair.public().to_peer_id();

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(GOSSIPSUB_HEARTBEAT)
        .validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(MAX_TRANSMIT_SIZE)
        .message_id_fn(|msg: &gossipsub::Message| {
            use std::hash::{Hash, Hasher};
            let mut s = std::collections::hash_map::DefaultHasher::new();
            msg.data.hash(&mut s);
            gossipsub::MessageId::new(&s.finish().to_be_bytes())
        })
        .build()
        .map_err(|e| anyhow::anyhow!("gossipsub config: {e}"))?;

    let swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| anyhow::anyhow!("tcp transport: {e}"))?
        .with_quic()
        .with_behaviour(|key| {
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )
            .map_err(|e| anyhow::anyhow!("gossipsub init: {e}"))?;

            let mut kademlia = kad::Behaviour::new(
                peer_id,
                kad::store::MemoryStore::new(peer_id),
            );
            kademlia.set_mode(Some(kad::Mode::Server));

            let identify = identify::Behaviour::new(
                identify::Config::new(IDENTIFY_PROTOCOL.to_string(), key.public())
                    .with_agent_version(format!("cmp/{}", env!("CARGO_PKG_VERSION"))),
            );

            // Probe every 30 s instead of the 5-minute default so LAN peers
            // are found within half a minute of both nodes being up.
            let mdns_cfg = mdns::Config {
                query_interval: Duration::from_secs(30),
                ..mdns::Config::default()
            };
            let mdns = mdns::tokio::Behaviour::new(
                mdns_cfg,
                key.public().to_peer_id(),
            )
            .map_err(|e| anyhow::anyhow!("mdns init: {e}"))?;

            Ok(MusicBehaviour { gossipsub, kademlia, identify, mdns })
        })
        .map_err(|e| anyhow::anyhow!("behaviour: {e}"))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_CONNECTION_TIMEOUT))
        .build();

    Ok(swarm)
}

// ---------------------------------------------------------------------------
// Peer discovery helpers
// ---------------------------------------------------------------------------

/// Feed listen addresses from `identify::Event::Received` into Kademlia.
pub fn handle_identify_event(swarm: &mut Swarm<MusicBehaviour>, event: &identify::Event) {
    if let identify::Event::Received { peer_id, info, .. } = event {
        for addr in &info.listen_addrs {
            swarm.behaviour_mut().kademlia.add_address(peer_id, addr.clone());
        }
    }
}

/// Dial bootstrap peers and trigger a Kademlia bootstrap query.
pub fn bootstrap(swarm: &mut Swarm<MusicBehaviour>, peers: &[(PeerId, libp2p::Multiaddr)]) {
    for (peer_id, addr) in peers {
        swarm.behaviour_mut().kademlia.add_address(peer_id, addr.clone());
        if let Err(e) = swarm.dial(addr.clone()) {
            tracing::warn!(%peer_id, %addr, "bootstrap dial failed: {e}");
        }
    }
    if !peers.is_empty() {
        if let Err(e) = swarm.behaviour_mut().kademlia.bootstrap() {
            tracing::warn!("kademlia bootstrap failed: {e}");
        }
    }
}

/// Add a peer to gossipsub's explicit peer list once a connection is open.
pub fn add_gossipsub_peer(swarm: &mut Swarm<MusicBehaviour>, peer_id: PeerId) {
    swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
}
