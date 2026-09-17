//! A `MultiplayerAPIExtension` that will speak Godot's own replication
//! protocol (plan.md DR-7).
//!
//! # What this is for
//!
//! `MultiplayerSpawner`, `MultiplayerSynchronizer` and every `@rpc` call route
//! through whatever `MultiplayerAPI` the `SceneTree` has installed. Replacing
//! it means the demo's own scripts -- `player.gd`, `red_robot.gd`,
//! `bullet.gd`, `level.gd` -- stay byte-identical, which matters here more
//! than usual: those files are the oracle the whole Fluorite port is measured
//! against.
//!
//! # This version answers a question rather than playing a game
//!
//! It installs, manages the peer, and reports what Godot hands it. It does not
//! replicate anything yet.
//!
//! That is deliberate. The crate beside this one already decodes and re-encodes
//! real captured traffic byte for byte, so the *protocol* is not the unknown.
//! What is unknown is the shape of the engine side: what `configuration`
//! actually is when a synchronizer registers, what order registrations arrive
//! in relative to spawns, and which of the nine virtuals the engine really
//! calls during a session. Guessing at those and writing a full implementation
//! against the guess is how the protocol work would have gone if the captures
//! had not been taken first.
//!
//! So this prints what arrives, and the next version is written against that.
//!
//! # What it found
//!
//! Running the shipping demo over this, headless, for ten seconds:
//!
//! **`configuration` is the spawner or synchronizer node itself**, and
//! `object` is the node being replicated. Registrations arrived as 12
//! `RigidBody3D`/`MultiplayerSynchronizer` pairs (four robots' three death
//! parts each), 5 `CharacterBody3D`/`MultiplayerSynchronizer`, 5
//! `CharacterBody3D`/`MultiplayerSpawner` through one shared spawner node, and
//! one each for the player's `ServerSynchronizer` and `InputSynchronizer`. So
//! the real implementation reads `replication_config` off the configuration
//! node and the values off the object -- it does not have to parse `.tscn`
//! files at runtime, which the port's bake does ahead of time for its own
//! reasons.
//!
//! The very first registration is different: `object` is null and
//! `configuration` is the `NodePath` `/root`, which is `set_multiplayer()`
//! itself registering the subtree.
//!
//! **Two properties had to exist before the demo would run at all**, and both
//! were found by running it rather than by reading a class list. Neither is on
//! `MultiplayerAPI`; both are on `SceneMultiplayer`, which is what scripts are
//! actually written against:
//!
//! - `server_relay`, assigned on line five of `main.gd:_ready`. Missing, the
//!   assignment raised a script error that aborted `_ready` before
//!   `go_to_main_menu()`, and the whole session silently did nothing.
//! - a non-null `multiplayer_peer`. `main.gd:15` calls `.close()` on it
//!   unconditionally, which is only safe because SceneMultiplayer defaults to
//!   an `OfflineMultiplayerPeer`.
//!
//! `rpc` is reached: `explode` and `land` both arrived in a ten-second
//! single-peer session, which are the same two the packet captures show.

use godot::classes::multiplayer_peer::{ConnectionStatus, TransferMode};
use godot::classes::{
    ENetMultiplayerPeer, Engine, IMultiplayerApiExtension, MultiplayerApiExtension,
    MultiplayerPeer, MultiplayerSpawner, MultiplayerSynchronizer, OfflineMultiplayerPeer,
    ResourceUid, SceneTree,
};
use godot::global::Error as GodotError;
use godot::prelude::*;
use godot_replication::path::{
    encode_despawn, ConfirmPath, SimplifyPath, COMMAND_CONFIRM_PATH, COMMAND_SIMPLIFY_PATH,
};
use godot_replication::rpc::{RemoteCall, RpcConfig};
use godot_replication::spawn::Spawn;
use godot_replication::sync::{encode, parse as parse_sync, SyncPacket, SyncRecord, COMMAND_SYNC};
use godot_replication::variant::{encode_compact, Value};

struct ReplicationExtension;

#[gdextension]
unsafe impl ExtensionLibrary for ReplicationExtension {}

/// The replacement multiplayer API.
///
/// `tool` is required by gdext for any class deriving a virtual extension
/// class, because such classes also run in the editor. Without it the derive
/// fails with eleven cascading errors, only the first of which says so.
#[derive(GodotClass)]
#[class(base = MultiplayerApiExtension, tool)]
pub struct ReplicationApi {
    base: Base<MultiplayerApiExtension>,
    peer: Option<Gd<MultiplayerPeer>>,

    /// `SceneMultiplayer::server_relay`, which a replacement has to provide
    /// itself.
    ///
    /// Found by running the demo over this extension rather than by reading
    /// the class list: `main.gd:5` assigns it on line five of `_ready`, and
    /// because `MultiplayerApiExtension` does not inherit it the assignment
    /// raised a script error that aborted `_ready` before `go_to_main_menu()`.
    /// Nothing else in the session then happened, and the cause was one line
    /// in a log that otherwise looked healthy.
    ///
    /// The general lesson is worth more than the property: a drop-in
    /// replacement has to cover `SceneMultiplayer`'s surface, not just
    /// `MultiplayerAPI`'s. Scripts are written against the concrete class the
    /// engine ships.
    #[var]
    server_relay: bool,

    /// Counted rather than logged per call: `poll` runs at the physics rate,
    /// and a line a frame buries everything else in the log.
    polls: u64,
    registrations: u64,
    rpcs: u64,
    /// One per synchronizer the engine has handed over.
    watched: Vec<Watched>,
    /// Reported once each, because a shape repeats four times over for the
    /// robots and a line per registration says the same thing four times.
    reported: Vec<String>,
    /// Next path-cache id to hand out.
    ///
    /// Godot starts at 1 and allocates contiguously -- measured across 112
    /// captured announcements, whose ids run 1..=n with no gaps.
    next_cache_id: u32,
    /// How many paths have been announced, and how many the far side accepted.
    announced: u32,
    confirmed: u32,
    rejected: u32,
    /// How many peers the transport reported last tick.
    ///
    /// Polled rather than taken from the peer's `peer_connected` signal, and
    /// the reason is worth recording. The signal does fire -- but it fires
    /// *re-entrantly, from inside `peer.poll()`*, which this class calls while
    /// `&mut self` is already borrowed by `poll`. gdext's cell catches the
    /// double borrow and panics, and Godot reports only
    /// "Error calling from signal 'peer_connected' to callable", which says
    /// nothing about why. Any `MultiplayerApiExtension` in Rust will hit this.
    ///
    /// Reading a count each tick has no such problem, and answers the only
    /// question this needs answered: is anyone there.
    linked: i64,
    /// Nodes a `MultiplayerSpawner` has handed over, in registration order.
    spawned: Vec<Spawned>,
    /// Set when a peer joins; the join sequence runs on the next poll.
    join_pending: bool,
    /// Next object id. Separate from path-cache ids: in the capture the
    /// spawner's path id and the first robot's object id are both 1.
    next_net_id: u32,
    spawns_sent: u32,
    /// Path-cache ids given to spawner nodes, by instance.
    spawner_ids: Vec<(InstanceId, u32)>,
    /// Packets received, by command byte. The first of each kind is logged;
    /// a client syncing at sixty hertz would otherwise bury everything else.
    incoming: std::collections::BTreeMap<u8, u64>,
    /// Paths remote peers have announced to us: (peer, id) -> node.
    remote_paths: Vec<(i32, u32, Gd<Node>)>,
    /// SYNC records applied, and refused for coming from a peer that does not
    /// own the synchronizer.
    applied: u64,
    refused: u64,
    /// Who is calling, while an RPC body runs. See `rpc`.
    remote_sender: i32,
    despawns_sent: u32,
    /// Increments per SYNC packet sent.
    sync_counter: u16,
    syncs_sent: u64,
    last_sync_bytes: usize,
    /// Nodes this peer has announced for RPCs: instance, path id, confirmed.
    rpc_paths: Vec<(InstanceId, u32, bool)>,
    rpcs_sent: u64,
    /// Peers this API believes are connected.
    ///
    /// Tracked here because nothing else can: `MultiplayerPeer` has no peer
    /// list, and keeping one is precisely the job of the `MultiplayerAPI` this
    /// class replaces. `get_peer_ids` is one of the nine virtuals and was
    /// returning an empty vector until this existed.
    peers: Vec<i32>,
}

/// A node that arrived through a spawner.
struct Spawned {
    spawner: Gd<MultiplayerSpawner>,
    node: Gd<Node>,
    /// Whether the current peers have been sent its SPAWN.
    sent: bool,
    /// The object id that SPAWN gave it, which DESPAWN names.
    net_id: u32,
}

/// A synchronizer and the node it replicates.
struct Watched {
    /// Kept so its visibility can be asked each tick.
    ///
    /// `public_visibility` is not a rendering flag -- it decides whether this
    /// synchronizer sends to peers at all. `red_robot.tscn` sets it false on
    /// all three death-part synchronizers (lines 10844, 10895, 10947) and
    /// `part.gd:31` sets it true when the part explodes, so a robot's debris
    /// is silent on the wire until it is actually flying.
    sync: Gd<MultiplayerSynchronizer>,
    object: Gd<Object>,
    /// The id announced for it, once announced.
    cache_id: Option<u32>,
    /// Its id in a SPAWN, if one listed it. SYNC addresses it by this.
    spawn_id: Option<u32>,
    /// The `spawn = true` properties, which ride the SPAWN packet.
    spawn_props: Vec<NodePath>,
    /// The mode-ALWAYS properties, in the order `replication_config` lists
    /// them -- which is the order they go on the wire.
    ///
    /// Mode 0 (NEVER) properties are filtered out here rather than at send
    /// time: they cross once with the spawn packet, and a SYNC carrying them
    /// would be traffic the original does not send. The port reached the same
    /// filter from the `.tscn` files (`net/replication_table.dart`); this
    /// reaches it from the live objects.
    streamed: Vec<NodePath>,
}

#[godot_api]
impl IMultiplayerApiExtension for ReplicationApi {
    fn init(base: Base<MultiplayerApiExtension>) -> Self {
        godot_print!("[replication] extension installed");
        Self {
            base,
            // Never null, and that is not a nicety. `main.gd:15` calls
            // `multiplayer.multiplayer_peer.close()` unconditionally on every
            // return to the menu, which is only safe because SceneMultiplayer
            // defaults to an OfflineMultiplayerPeer. A replacement returning
            // null there raises "Cannot call method 'close' on a null value"
            // and aborts the function -- found by running the demo over this,
            // one gap at a time.
            //
            // It is also the same fact the port arrived at from the other
            // direction: `tools/offline_authority_probe.gd` measured that an
            // offline Godot reports is_server true and unique_id 1, which is
            // what LoopbackAuthority was built to reproduce (DR-1).
            peer: Some(OfflineMultiplayerPeer::new_gd().upcast()),
            // Godot's own default, so a script that never sets it behaves the
            // same as it does on SceneMultiplayer.
            server_relay: true,
            polls: 0,
            registrations: 0,
            rpcs: 0,
            watched: Vec::new(),
            reported: Vec::new(),
            next_cache_id: 1,
            announced: 0,
            confirmed: 0,
            rejected: 0,
            peers: Vec::new(),
            linked: 0,
            spawned: Vec::new(),
            join_pending: false,
            next_net_id: 1,
            spawns_sent: 0,
            spawner_ids: Vec::new(),
            incoming: std::collections::BTreeMap::new(),
            remote_paths: Vec::new(),
            applied: 0,
            refused: 0,
            remote_sender: 0,
            despawns_sent: 0,
            sync_counter: 0,
            syncs_sent: 0,
            last_sync_bytes: 0,
            rpc_paths: Vec::new(),
            rpcs_sent: 0,
        }
    }

    fn poll(&mut self) -> GodotError {
        self.polls += 1;
        if let Some(peer) = self.peer.as_mut() {
            peer.poll();
        }
        self.track_peers();
        self.read_incoming();
        if self.join_pending {
            self.join_pending = false;
            self.send_join();
        }
        if self.linked > 0 {
            self.send_pending_spawns();
            self.send_sync();
        }
        // Every hundredth, so a long session leaves a trail without drowning
        // the interesting lines.
        if self.polls.is_multiple_of(100) {
            godot_print!(
                "[replication] polls={} watching={} syncs_sent={} last_sync={} bytes rpcs={} \
                 rpcs_sent={} announced={} confirmed={} rejected={} spawns={} despawns={} \
                 applied={} refused={}",
                self.polls,
                self.watched.len(),
                self.syncs_sent,
                self.last_sync_bytes,
                self.rpcs,
                self.rpcs_sent,
                self.announced,
                self.confirmed,
                self.rejected,
                self.spawns_sent,
                self.despawns_sent,
                self.applied,
                self.refused
            );
        }
        GodotError::OK
    }

    fn set_multiplayer_peer(&mut self, multiplayer_peer: Option<Gd<MultiplayerPeer>>) {
        godot_print!(
            "[replication] set_multiplayer_peer: {}",
            multiplayer_peer
                .as_ref()
                .map_or_else(|| "none".to_string(), |p| p.get_class().to_string())
        );
        self.peers.clear();
        self.linked = 0;
        self.peer = multiplayer_peer;
    }

    fn get_multiplayer_peer(&mut self) -> Option<Gd<MultiplayerPeer>> {
        self.peer.clone()
    }

    fn get_unique_id(&self) -> i32 {
        // 1 with no peer, which is what an offline Godot reports and what the
        // port's LoopbackAuthority was built to match.
        self.peer.as_ref().map_or(1, |p| p.get_unique_id())
    }

    fn get_peer_ids(&self) -> PackedInt32Array {
        PackedInt32Array::from(self.peers.as_slice())
    }

    fn rpc(
        &mut self,
        peer: i32,
        object: Option<Gd<Object>>,
        method: StringName,
        args: VarArray,
    ) -> GodotError {
        self.rpcs += 1;
        let Some(mut target) = object else {
            return GodotError::ERR_INVALID_PARAMETER;
        };
        let first = format!("rpc {}::{method}", target.get_class());
        if !self.reported.contains(&first) {
            godot_print!("[replication] first {first} -> peer {peer}");
            self.reported.push(first);
        }
        let local = self.get_unique_id();
        // Remote peers first, then the local call, which is the order
        // SceneMultiplayer uses. It also matters: a local body can free the
        // node (a bullet's `explode` leads to exactly that), and its path has
        // to be read before that happens.
        if peer != local && self.linked > 0 {
            self.send_rpc(peer, &target, &method);
        }
        // `call_local` is this API's job, not the engine's. A custom
        // MultiplayerAPI that only sends leaves the local half undone -- and
        // this demo's gameplay lives in those bodies. Measured before this
        // existed: `shoot()` starts the fire cooldown, so without the local
        // call the player fired every frame (1133 bullets in twenty seconds,
        // against the capture's two a second); `explode()` starts the
        // animation that frees a bullet, so none ever died and the server
        // was tracking 1138 live synchronizers.
        if (peer == 0 || peer == local) && rpc_calls_local(&target, &method) {
            self.remote_sender = local;
            // Through the base guard, because the body is GDScript and may call
            // back into this API: `red_robot.gd:103` asks
            // `multiplayer.is_server()` inside `hit()`. A plain call would be a
            // second borrow of `self` and gdext would panic, as it did for
            // `peer_connected`.
            {
                let _reentrant = self.base_mut();
                target.callv(&method, &args);
            }
            self.remote_sender = 0;
        }
        GodotError::OK
    }

    fn get_remote_sender_id(&self) -> i32 {
        self.remote_sender
    }

    fn object_configuration_add(
        &mut self,
        object: Option<Gd<Object>>,
        configuration: Variant,
    ) -> GodotError {
        self.registrations += 1;
        // The first registration of all is `set_multiplayer()` itself: a null
        // object and the subtree's NodePath. Everything after is a spawner or
        // a synchronizer handing itself over, with `object` the node it acts
        // on.
        let Some(object) = object else {
            godot_print!("[replication] configuration_add: subtree {configuration}");
            return GodotError::OK;
        };
        if let Ok(spawner) = configuration.try_to::<Gd<MultiplayerSpawner>>() {
            if let Ok(node) = object.try_cast::<Node>() {
                self.spawned.push(Spawned {
                    spawner,
                    node,
                    sent: false,
                    net_id: 0,
                });
            }
            return GodotError::OK;
        }
        let Ok(sync) = configuration.try_to::<Gd<MultiplayerSynchronizer>>() else {
            return GodotError::OK;
        };
        self.watch(object, &sync);
        GodotError::OK
    }

    fn object_configuration_remove(
        &mut self,
        object: Option<Gd<Object>>,
        configuration: Variant,
    ) -> GodotError {
        let Some(object) = object else {
            return GodotError::OK;
        };
        let gone = object.instance_id();

        if let Ok(sync) = configuration.try_to::<Gd<MultiplayerSynchronizer>>() {
            let key = sync.instance_id();
            self.watched.retain(|w| w.sync.instance_id() != key);
            return GodotError::OK;
        }
        // A spawned node leaving. Peers that were told it exists are told it is
        // gone: without this a client keeps every bullet ever fired.
        let Some(index) = self
            .spawned
            .iter()
            .position(|e| e.node.instance_id() == gone)
        else {
            return GodotError::OK;
        };
        let entry = self.spawned.remove(index);
        if entry.sent && self.linked > 0 {
            if let Some(peer) = self.peer.as_mut() {
                let bytes = encode_despawn(entry.net_id);
                peer.set_target_peer(0);
                peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
                self.despawns_sent += 1;
            }
        }
        GodotError::OK
    }
}

impl ReplicationApi {
    /// Notices peers arriving and leaving, by counting them.
    ///
    /// ENet-specific, and deliberately so for now: `MultiplayerPeer` exposes
    /// no peer list at all -- keeping one is the job of the `MultiplayerAPI`
    /// this class replaces, so there is nothing generic to ask. Real peer
    /// *ids* come later from `get_packet_peer()` on the first packet each one
    /// sends; a count is enough to decide whether announcing is worth doing.
    fn track_peers(&mut self) {
        let Some(peer) = self.peer.as_ref() else {
            return;
        };
        let now = peer
            .clone()
            .try_cast::<ENetMultiplayerPeer>()
            .ok()
            .and_then(|p| p.get_host())
            .map_or(0, |h| h.get_peers().len() as i64);
        if now == self.linked {
            return;
        }
        godot_print!("[replication] transport peers {} -> {now}", self.linked);
        if now > self.linked {
            // Someone joined. Everything announced so far went to somebody
            // else, or to nobody at all, so the table is announced afresh.
            for watched in &mut self.watched {
                watched.cache_id = None;
                watched.spawn_id = None;
            }
            for entry in &mut self.spawned {
                entry.sent = false;
            }
            self.spawner_ids.clear();
            self.rpc_paths.clear();
            self.next_cache_id = 1;
            self.next_net_id = 1;
            self.announced = 0;
            self.join_pending = true;
        }
        if now == 0 {
            // Everyone left. Tell the scripts, as SceneMultiplayer would --
            // deferred for the same reason peer_connected is.
            for id in std::mem::take(&mut self.peers) {
                self.base_mut().call_deferred(
                    "emit_signal",
                    &["peer_disconnected".to_variant(), id.to_variant()],
                );
            }
        }
        self.linked = now;
    }

    /// What a joining peer is sent: the spawner's path, then a SPAWN for every
    /// node that already exists.
    ///
    /// The order is the capture's -- `SIMPLIFY_PATH` for
    /// `main/Level/MultiplayerSpawner` first, then the robots, back to back
    /// with no wait for the confirmation. Both go reliable on the same
    /// channel, so the far side sees the path before the spawns that name it.
    ///
    /// Broadcast rather than targeted: the probe has one client, and the id of
    /// a joining peer is not known until it sends something.
    fn send_join(&mut self) {
        let Some(mut peer) = self.peer.clone() else {
            return;
        };
        if peer.get_connection_status() != ConnectionStatus::CONNECTED {
            return;
        }
        peer.set_target_peer(0);

        // Spawner paths, one id each, in first-seen order.
        for index in 0..self.spawned.len() {
            let spawner = self.spawned[index].spawner.clone();
            let key = spawner.instance_id();
            if self.spawner_ids.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let Some(path) = relative_path(&spawner.upcast()) else {
                continue;
            };
            let id = self.next_cache_id;
            self.next_cache_id += 1;
            let bytes = SimplifyPath {
                id,
                path: path.clone(),
                rpc_hash: RpcConfig::default().hash(),
            }
            .encode();
            let sent = peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
            self.announced += 1;
            godot_print!(
                "[replication] path {id} -> {path} ({} bytes, {sent:?})",
                bytes.len()
            );
            self.spawner_ids.push((key, id));
        }
    }

    /// Sends a SPAWN for every node the current peers have not been told about.
    ///
    /// Run every tick, not only on join, because nodes keep arriving. The
    /// joining client's own player is the first such: `level.gd` creates it
    /// from `peer_connected`, which this API emits deferred, so it registers
    /// a tick after the join sequence and is sent here on the tick after
    /// that -- once its synchronizers have registered too, which happens
    /// inside the same `add_child` as the spawner registration.
    fn send_pending_spawns(&mut self) {
        let Some(mut peer) = self.peer.clone() else {
            return;
        };
        peer.set_target_peer(0);
        let local = self.get_unique_id();
        for index in 0..self.spawned.len() {
            let entry = &self.spawned[index];
            if entry.sent || !entry.node.is_instance_valid() {
                continue;
            }
            let Some(&(_, spawner_id)) = self
                .spawner_ids
                .iter()
                .find(|(k, _)| *k == entry.spawner.instance_id())
            else {
                continue;
            };
            let Some(scene) = scene_index(&entry.spawner, &entry.node) else {
                godot_error!(
                    "[replication] {} is not in its spawner's scene list",
                    entry.node.get_name()
                );
                self.spawned[index].sent = true;
                continue;
            };
            let node_id = entry.node.instance_id();
            let name = entry.node.get_name().to_string();

            let net_id = self.next_net_id;
            let mut sync_ids = Vec::new();
            let mut state = Vec::new();
            for watched in &mut self.watched {
                // Listed only if it acts on this node, is ours, and is
                // visible -- which is why a robot lists one synchronizer and
                // not four (its parts are invisible), and why a client's
                // player lists one and the host's two (the input half belongs
                // to its own peer).
                if watched.object.instance_id() != node_id
                    || !watched.sync.is_instance_valid()
                    || !watched.sync.is_visibility_public()
                    || watched.sync.get_multiplayer_authority() != local
                {
                    continue;
                }
                let sync_id = net_id + 1 + u32::try_from(sync_ids.len()).unwrap_or(0);
                watched.spawn_id = Some(sync_id);
                sync_ids.push(sync_id);
                for value in Self::read(&watched.object, &watched.spawn_props) {
                    state.extend_from_slice(&encode_compact(&value));
                }
            }
            self.next_net_id = net_id + 1 + u32::try_from(sync_ids.len()).unwrap_or(0);
            self.spawned[index].sent = true;
            self.spawned[index].net_id = net_id;

            let bytes = Spawn {
                scene,
                spawner: spawner_id,
                net_id,
                sync_ids: sync_ids.clone(),
                name: name.clone(),
                state,
            }
            .encode();
            let sent = peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
            self.spawns_sent += 1;
            godot_print!(
                "[replication] spawn {name} scene {scene} net {net_id} syncs {sync_ids:?} \
                 ({} bytes, {sent:?})",
                bytes.len()
            );
        }
    }

    /// Reads whatever the far side sent.
    ///
    /// Only `CONFIRM_PATH` is acted on so far; everything else is counted and
    /// named. A packet type this does not handle is reported rather than
    /// dropped quietly, because "nothing happened" and "something arrived that
    /// nobody read" look identical in a log otherwise.
    fn read_incoming(&mut self) {
        let Some(peer) = self.peer.as_mut() else {
            return;
        };
        // Drained first and handled after, so that handling can call back
        // into `self` without holding the peer borrow.
        let mut packets = Vec::new();
        while peer.get_available_packet_count() > 0 {
            // The sender of the *next* packet, so asked before reading it.
            // This is how a joining peer's id is learned: the transport count
            // says someone is there, and their first packet -- the
            // CONFIRM_PATH answering the spawner path -- says who.
            let from = peer.get_packet_peer();
            packets.push((from, peer.get_packet().to_vec()));
        }

        let mut arrived = Vec::new();
        for (from, bytes) in packets {
            let Some(&command) = bytes.first() else {
                continue;
            };
            if from != 0 && !self.peers.contains(&from) {
                self.peers.push(from);
                arrived.push(from);
            }
            let seen = {
                let count = self.incoming.entry(command).or_insert(0);
                *count += 1;
                *count
            };
            match command {
                COMMAND_CONFIRM_PATH => self.on_confirm_path(&bytes),
                COMMAND_SIMPLIFY_PATH => self.on_simplify_path(from, &bytes),
                COMMAND_SYNC => self.on_sync(from, &bytes, seen == 1),
                _ if seen == 1 => godot_print!(
                    "[replication] first incoming command {command:#04x} from {from}, {} bytes",
                    bytes.len()
                ),
                _ => {}
            }
        }
        for id in arrived {
            godot_print!("[replication] peer {id} connected");
            // Deferred, and it has to be. `level.gd` answers this signal by
            // spawning a player, and `add_child` re-enters this API through
            // `object_configuration_add` -- which would be a second mutable
            // borrow while `poll` holds the first. gdext panics on that, as
            // it did when the peer's own signal was connected directly.
            self.base_mut().call_deferred(
                "emit_signal",
                &["peer_connected".to_variant(), id.to_variant()],
            );
        }
    }

    fn on_confirm_path(&mut self, bytes: &[u8]) {
        match ConfirmPath::parse(bytes) {
            Ok(answer) if answer.valid => {
                self.confirmed += 1;
                godot_print!("[replication] confirmed id {}", answer.id);
                for entry in &mut self.rpc_paths {
                    if entry.1 == answer.id {
                        entry.2 = true;
                    }
                }
            }
            Ok(answer) => {
                // Not a transport failure: the far side could not resolve the
                // path, which means the two scene trees disagree about what
                // exists.
                self.rejected += 1;
                godot_error!("[replication] id {} REJECTED by the peer", answer.id);
            }
            Err(error) => godot_error!("[replication] malformed CONFIRM_PATH: {error:?}"),
        }
    }

    /// A peer names one of its nodes. Resolve it here and say whether we can.
    fn on_simplify_path(&mut self, from: i32, bytes: &[u8]) {
        let announced = match SimplifyPath::parse(bytes) {
            Ok(a) => a,
            Err(error) => {
                godot_error!("[replication] malformed SIMPLIFY_PATH from {from}: {error:?}");
                return;
            }
        };
        let node = scene_root()
            .and_then(|root| root.get_node_or_null(&NodePath::from(announced.path.as_str())));
        // The hash is the other half of the agreement: both ends must number
        // this node's RPCs the same way. A mismatch is reported, not guessed
        // around -- it means the two builds do not run the same script.
        if let Some(node) = node.as_ref() {
            let ours = script_rpc_hash(node);
            if ours != announced.rpc_hash {
                godot_error!(
                    "[replication] {} from {from}: rpc hash {} but ours is {ours}",
                    announced.path,
                    announced.rpc_hash
                );
            }
        }
        let valid = node.is_some();
        godot_print!(
            "[replication] peer {from} announced id {} -> {} ({})",
            announced.id,
            announced.path,
            if valid { "resolved" } else { "NOT FOUND" }
        );
        if let Some(node) = node {
            self.remote_paths
                .retain(|(p, id, _)| !(*p == from && *id == announced.id));
            self.remote_paths.push((from, announced.id, node));
        }
        let reply = ConfirmPath {
            id: announced.id,
            valid,
        }
        .encode();
        if let Some(peer) = self.peer.as_mut() {
            peer.set_target_peer(from);
            peer.put_packet(&PackedByteArray::from(reply.as_slice()));
            peer.set_target_peer(0);
        }
    }

    /// A peer's synchronizer state. Applied only if that peer owns it.
    fn on_sync(&mut self, from: i32, bytes: &[u8], first: bool) {
        let packet = match parse_sync(bytes) {
            Ok(p) => p,
            Err(error) => {
                godot_error!("[replication] malformed SYNC from {from}: {error:?}");
                return;
            }
        };
        for record in packet.records {
            // Records a peer sends for its own synchronizers are addressed by
            // the path id it announced, with the top bit set.
            if record.net_id & 0x8000_0000 == 0 {
                continue;
            }
            let id = record.net_id & 0x7fff_ffff;
            let Some(node) = self
                .remote_paths
                .iter()
                .find(|(p, i, _)| *p == from && *i == id)
                .map(|(_, _, n)| n.instance_id())
            else {
                continue;
            };
            let Some(watched) = self
                .watched
                .iter()
                .find(|w| w.sync.is_instance_valid() && w.sync.instance_id() == node)
            else {
                continue;
            };
            // The authority check SceneMultiplayer makes: a peer may only
            // write the synchronizers it owns. Without it any client could
            // move any robot.
            if watched.sync.get_multiplayer_authority() != from {
                self.refused += 1;
                continue;
            }
            if record.fields.len() != watched.streamed.len() {
                godot_error!(
                    "[replication] sync from {from}: {} fields for {} properties",
                    record.fields.len(),
                    watched.streamed.len()
                );
                continue;
            }
            let object = watched.object.clone();
            let paths = watched.streamed.clone();
            if first {
                godot_print!(
                    "[replication] first sync from {from}: {} fields -> {}",
                    record.fields.len(),
                    watched.sync.get_name()
                );
            }
            for (path, value) in paths.iter().zip(&record.fields) {
                if let Some((mut target, property)) = resolve(&object, path) {
                    target.set_indexed(&property, &to_variant(value));
                }
            }
            self.applied += 1;
        }
    }

    /// Sends this tick's state for every synchronizer the peers know about.
    ///
    /// One record per synchronizer that was listed in a SPAWN, is ours and is
    /// visible, addressed by its spawn id -- the rules the capture showed. The
    /// ownership test is what keeps a client's own input from being echoed
    /// back to it: the server tracks that synchronizer too, but does not own it.
    ///
    /// Unreliable, as the capture's `SEND_UNSEQUENCED` commands are: a lost
    /// state update is replaced by the next one sixty-odd times a second, and
    /// retransmitting stale positions would be worse than dropping them.
    ///
    /// No size cap is applied, because the demo never needs one: packets grow
    /// with bullets in the air, and the largest captured was 877 bytes.
    fn send_sync(&mut self) {
        let local = self.get_unique_id();
        let mut records = Vec::new();
        for watched in &self.watched {
            let Some(net_id) = watched.spawn_id else {
                continue;
            };
            if !watched.object.is_instance_valid()
                || !watched.sync.is_instance_valid()
                || !watched.sync.is_visibility_public()
                || watched.sync.get_multiplayer_authority() != local
            {
                continue;
            }
            let fields = Self::read(&watched.object, &watched.streamed);
            if fields.is_empty() {
                continue;
            }
            records.push(SyncRecord { net_id, fields });
        }
        if records.is_empty() {
            return;
        }
        let Some(peer) = self.peer.as_mut() else {
            return;
        };
        self.sync_counter = self.sync_counter.wrapping_add(1);
        let bytes = encode(&SyncPacket {
            counter: self.sync_counter,
            records,
        });
        peer.set_target_peer(0);
        peer.set_transfer_mode(TransferMode::UNRELIABLE);
        peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
        peer.set_transfer_mode(TransferMode::RELIABLE);
        self.syncs_sent += 1;
        self.last_sync_bytes = bytes.len();
    }

    /// Sends one RPC to remote peers, as the capture shows it done.
    ///
    /// The target's path is announced once. Until the peer confirms it, calls
    /// go in the long form that carries the path (`0xa0`); after, in the
    /// three-byte cached form (`0x80`). That is why a shooting session in the
    /// capture has both in nearly equal numbers: the player's path is
    /// confirmed early and its `shoot` calls are short, while each new bullet
    /// is exploded before its path comes back confirmed.
    ///
    /// The method id is the index in the script's sorted RPC names. Arguments
    /// are not encoded -- nothing in this demo sends any.
    fn send_rpc(&mut self, peer_id: i32, target: &Gd<Object>, method: &StringName) {
        let Ok(node) = target.clone().try_cast::<Node>() else {
            return;
        };
        let Some(path) = relative_path(&node) else {
            return;
        };
        let names = script_rpc_names(&node);
        let config = RpcConfig::new(names);
        let Some(method_id) = config
            .id_of(&method.to_string())
            .and_then(|i| u8::try_from(i).ok())
        else {
            godot_error!("[replication] {path} has no RPC named {method}");
            return;
        };
        let Some(mut peer) = self.peer.clone() else {
            return;
        };
        peer.set_target_peer(peer_id);

        let key = node.instance_id();
        let known = self
            .rpc_paths
            .iter()
            .find(|e| e.0 == key)
            .map(|e| (e.1, e.2));
        let (path_id, confirmed) = if let Some(found) = known {
            found
        } else {
            let id = self.next_cache_id;
            self.next_cache_id += 1;
            let bytes = SimplifyPath {
                id,
                path: path.clone(),
                rpc_hash: config.hash(),
            }
            .encode();
            peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
            self.announced += 1;
            self.rpc_paths.push((key, id, false));
            (id, false)
        };

        let call = match u8::try_from(path_id) {
            Ok(cache_id) if confirmed => RemoteCall::Cached {
                cache_id,
                method: method_id,
            },
            // The long form carries the path itself, at a fixed offset.
            _ => RemoteCall::Path {
                method: method_id,
                path,
            },
        };
        let bytes = call.encode();
        peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
        peer.set_target_peer(0);
        self.rpcs_sent += 1;
    }

    /// Takes the field list off a synchronizer and reads it back once.
    ///
    /// Reading the values immediately is the check that matters: it proves the
    /// paths resolve against the object and that every type they yield is one
    /// the codec handles. A field list recovered but never dereferenced would
    /// look correct right up to the first packet.
    fn watch(&mut self, object: Gd<Object>, sync: &Gd<MultiplayerSynchronizer>) {
        let Some(config) = sync.get_replication_config() else {
            return;
        };
        let mut streamed = Vec::new();
        let mut spawn_props = Vec::new();
        let mut modes = Vec::new();
        let mut config = config;
        for path in config.get_properties().iter_shared() {
            let mode = config.property_get_replication_mode(&path);
            modes.push(format!("{path}={mode:?}"));
            if config.property_get_spawn(&path) {
                spawn_props.push(path.clone());
            }
            // ALWAYS is the streaming mode. NEVER rides the spawn.
            if format!("{mode:?}").contains("ALWAYS") {
                streamed.push(path);
            }
        }

        let values = Self::read(&object, &streamed);
        let shape: Vec<String> = values
            .iter()
            .map(|v| format!("{:?}", v.variant_type()))
            .collect();
        let key = format!("{}|{}", object.get_class(), shape.join(","));
        if !self.reported.contains(&key) {
            let record = SyncRecord {
                net_id: 0x8000_0001,
                fields: values,
            };
            let bytes = encode(&SyncPacket {
                counter: 0,
                records: vec![record],
            });
            godot_print!(
                "[replication] {} config=[{}] streamed=[{}] -> {} byte record",
                object.get_class(),
                modes.join(", "),
                shape.join(", "),
                bytes.len()
            );
            self.reported.push(key);
        }
        self.watched.push(Watched {
            sync: sync.clone(),
            object,
            cache_id: None,
            spawn_id: None,
            spawn_props,
            streamed,
        });
    }

    /// Reads the current value of each path off the node.
    fn read(object: &Gd<Object>, paths: &[NodePath]) -> Vec<Value> {
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(value) = Self::read_one(object, path) else {
                godot_error!(
                    "[replication] {path} does not resolve on {}",
                    object.get_class()
                );
                continue;
            };
            if let Some(converted) = Self::convert(&value) {
                out.push(converted);
            } else {
                // Loudly, not silently. A type the codec does not carry means
                // the packet would be short by a field, and every field after
                // it would decode from the wrong offset.
                godot_error!(
                    "[replication] {path} on {} is {:?}, which the codec does not carry",
                    object.get_class(),
                    value.get_type()
                );
            }
        }
        out
    }

    /// Resolves one `SceneReplicationConfig` path against the node.
    ///
    /// These are two-part paths -- a node part and a property part, separated
    /// by a colon: `.:global_transform` is a property of the node itself,
    /// `CameraBase:rotation` a property of a child. `Object::get_indexed`
    /// takes only the property half; handed the whole path it returns NIL for
    /// every field, which is what it did until this split existed.
    ///
    /// NIL rather than an error is the part worth guarding against. A packet
    /// built from those reads would have been well-formed and empty, and the
    /// first sign of trouble would have been a peer that never moved.
    fn read_one(object: &Gd<Object>, path: &NodePath) -> Option<Variant> {
        let (target, property) = resolve(object, path)?;
        Some(target.get_indexed(&property))
    }

    /// Godot's Variant to the crate's, for the six types this demo replicates.
    fn convert(value: &Variant) -> Option<Value> {
        use godot::builtin::VariantType as T;
        use godot::builtin::{Transform3D, Vector2, Vector3};
        Some(match value.get_type() {
            T::BOOL => Value::Bool(value.to::<bool>()),
            T::INT => Value::Int(value.to::<i64>()),
            T::FLOAT => Value::Float(value.to::<f64>()),
            T::VECTOR2 => {
                let v = value.to::<Vector2>();
                Value::Vector2([v.x, v.y])
            }
            T::VECTOR3 => {
                let v = value.to::<Vector3>();
                Value::Vector3([v.x, v.y, v.z])
            }
            T::TRANSFORM3D => {
                let t = value.to::<Transform3D>();
                // Row-major, because that is the order Godot's own encoder
                // writes and its `Basis(x, y, z)` constructor takes columns.
                // Transposed here, the rotation would be wrong about one axis
                // only -- which reads as a tuning problem, not an encoding one.
                let r = t.basis.rows;
                Value::Transform3D([
                    r[0].x, r[0].y, r[0].z, r[1].x, r[1].y, r[1].z, r[2].x, r[2].y, r[2].z,
                    t.origin.x, t.origin.y, t.origin.z,
                ])
            }
            _ => return None,
        })
    }
}

/// A node's path as the far side resolves it.
///
/// Captured paths read `main/Level/...`: relative to the SceneTree root, so
/// `/root/` comes off, not just the leading slash. Announcing
/// `root/main/Level/...` is unresolvable at the far end.
fn relative_path(node: &Gd<Node>) -> Option<String> {
    node.get_path()
        .to_string()
        .strip_prefix("/root/")
        .map(str::to_owned)
}

/// Which of the spawner's scenes `node` was instantiated from.
///
/// `level.tscn` lists its spawnable scenes by uid (`uid://cs1k22tdf04k4`),
/// while a node knows its scene by path, so a uid is resolved before
/// comparing.
fn scene_index(spawner: &Gd<MultiplayerSpawner>, node: &Gd<Node>) -> Option<u8> {
    let want = node.get_scene_file_path().to_string();
    let uids = ResourceUid::singleton();
    (0..spawner.get_spawnable_scene_count()).find_map(|i| {
        let listed = spawner.get_spawnable_scene(i).to_string();
        let resolved = if listed.starts_with("uid://") {
            let id = uids.text_to_id(&listed);
            uids.get_id_path(id).to_string()
        } else {
            listed
        };
        (resolved == want).then(|| u8::try_from(i).ok()).flatten()
    })
}

/// Splits a `SceneReplicationConfig` path into the object it names and the
/// property on it.
///
/// These are two-part paths -- a node part and a property part either side of
/// a colon: `.:global_transform` is a property of the node itself,
/// `CameraBase:rotation` one of a child. `Object::get_indexed` takes only the
/// property half; handed the whole path it returns NIL for every field, which
/// is what it did until this split existed.
fn resolve(object: &Gd<Object>, path: &NodePath) -> Option<(Gd<Object>, NodePath)> {
    let property = path.get_concatenated_subnames();
    if property.is_empty() {
        return None;
    }
    let node_part = path.get_concatenated_names().to_string();
    let target: Gd<Object> = if node_part.is_empty() || node_part == "." {
        object.clone()
    } else {
        object
            .clone()
            .try_cast::<Node>()
            .ok()?
            .get_node_or_null(&NodePath::from(node_part.as_str()))?
            .upcast()
    };
    Some((target, NodePath::from(property.to_string().as_str())))
}

/// The crate's Variant back to Godot's.
fn to_variant(value: &Value) -> Variant {
    use godot::builtin::{Basis, Transform3D, Vector2, Vector3};
    match *value {
        Value::Bool(v) => v.to_variant(),
        Value::Int(v) => v.to_variant(),
        Value::Float(v) => v.to_variant(),
        Value::Vector2([x, y]) => Vector2::new(x, y).to_variant(),
        Value::Vector3([x, y, z]) => Vector3::new(x, y, z).to_variant(),
        Value::Transform3D(m) => Transform3D::new(
            // Row-major, the order the encoder writes.
            Basis::from_rows(
                Vector3::new(m[0], m[1], m[2]),
                Vector3::new(m[3], m[4], m[5]),
                Vector3::new(m[6], m[7], m[8]),
            ),
            Vector3::new(m[9], m[10], m[11]),
        )
        .to_variant(),
    }
}

/// The SceneTree root, which remote paths are relative to.
fn scene_root() -> Option<Gd<Node>> {
    Engine::singleton()
        .get_main_loop()?
        .try_cast::<SceneTree>()
        .ok()?
        .get_root()
        .map(Gd::upcast)
}

/// The RPC method names the script a node runs declares.
fn script_rpc_names(node: &Gd<Node>) -> Vec<String> {
    let Some(script) = node.get_script() else {
        return Vec::new();
    };
    script
        .get_rpc_config()
        .call("keys", &[])
        .try_to::<VarArray>()
        .map(|keys| keys.iter_shared().map(|k| k.to_string()).collect())
        .unwrap_or_default()
}

/// The RPC-config hash of the script a node runs, or of no script.
fn script_rpc_hash(node: &Gd<Node>) -> String {
    RpcConfig::new(script_rpc_names(node)).hash()
}

/// Whether `method` on `object` is declared `@rpc(..., "call_local")`.
///
/// Read from the script's own config, the same source the RPC hash comes from.
/// Per-node overrides set with `rpc_config()` are not consulted: this demo
/// declares everything with the annotation and sets none.
fn rpc_calls_local(object: &Gd<Object>, method: &StringName) -> bool {
    let Some(script) = object.get_script() else {
        return false;
    };
    let config = script.get_rpc_config();
    let entry = config.call("get", &[method.to_variant()]);
    if entry.is_nil() {
        return false;
    }
    entry
        .call("get", &["call_local".to_variant(), false.to_variant()])
        .try_to::<bool>()
        .unwrap_or(false)
}
