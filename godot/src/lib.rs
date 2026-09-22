//! A `MultiplayerAPIExtension` that speaks Godot's own replication protocol.
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
//! # Where it has got to
//!
//! It plays the demo, as server or as client, against a stock Godot 4.5.2 on
//! the other side of the socket -- all four pairings, and a server with two
//! clients at once. At the wire its join sequence, packet sizes and RPC forms
//! match a stock server's, compared packet by packet against a capture of
//! one.
//!
//! What is not done: RPC arguments, which nothing in this demo sends, and
//! per-peer visibility.
//!
//! # How it was built, and why that shape
//!
//! The first version of this file replicated nothing. It installed, managed
//! the peer, and printed what the engine handed it.
//!
//! That was deliberate. The crate beside this one already decodes and
//! re-encodes real captured traffic byte for byte, so the *protocol* was never
//! the unknown. What was unknown is the shape of the engine side: what
//! `configuration` actually is when a synchronizer registers, what order
//! registrations arrive in relative to spawns, and which of the nine virtuals
//! the engine really calls during a session. Guessing at those and writing a
//! full implementation against the guess is how the protocol work would have
//! gone if the captures had not been taken first.
//!
//! Everything below is what that printing found, and the rest of the file is
//! written against it rather than against an assumption.
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

use godot::classes::multiplayer_peer::TransferMode;
use godot::classes::{
    Engine, IMultiplayerApiExtension, MultiplayerApiExtension, MultiplayerPeer, MultiplayerSpawner,
    MultiplayerSynchronizer, OfflineMultiplayerPeer, PackedScene, ResourceLoader, ResourceUid,
    SceneTree,
};
use godot::global::Error as GodotError;
use godot::prelude::*;
use godot_replication::path::{
    encode_despawn, parse_despawn, ConfirmPath, SimplifyPath, COMMAND_CONFIRM_PATH,
    COMMAND_DESPAWN, COMMAND_SIMPLIFY_PATH,
};
use godot_replication::rpc::{RemoteCall, RpcConfig, LEAD_CACHED, LEAD_PATH};
use godot_replication::spawn::{Spawn, COMMAND_SPAWN};
use godot_replication::sync::{encode, parse as parse_sync, SyncPacket, SyncRecord, COMMAND_SYNC};
use godot_replication::variant::{decode_compact, encode_compact, Value};
use std::cell::RefCell;
use std::rc::Rc;

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
    /// Peer arrivals and departures the transport has reported but this class
    /// has not processed yet: `(id, connected)`, oldest first.
    ///
    /// A queue rather than a direct signal handler, and the reason is worth
    /// recording. `MultiplayerPeer::peer_connected` fires *re-entrantly, from
    /// inside `peer.poll()`*, which this class calls while `&mut self` is
    /// already borrowed by `poll`. A handler that touches `self` hits gdext's
    /// cell, which catches the double borrow and panics, and Godot reports
    /// only "Error calling from signal 'peer_connected' to callable", which
    /// says nothing about why. Any `MultiplayerApiExtension` in Rust will hit
    /// this.
    ///
    /// The closures below touch nothing but this queue, so the re-entrancy is
    /// harmless, and `track_peers` drains it once the borrow is gone.
    ///
    /// The ids matter, not just the count: every send is per peer now, and an
    /// earlier count-only version had no way to learn them. It waited for each
    /// peer's first packet -- which never came, because a client sends nothing
    /// until the server has spawned its player.
    peer_events: Rc<RefCell<Vec<(i32, bool)>>>,
    /// Nodes a `MultiplayerSpawner` has handed over, in registration order.
    spawned: Vec<Spawned>,
    /// Next object id. Separate from path-cache ids: in the capture the
    /// spawner's path id and the first robot's object id are both 1.
    next_net_id: u32,
    spawns_sent: u32,
    /// Every path this peer has announced, with who has heard and confirmed it.
    ///
    /// One table for all three users -- spawner paths, owned synchronizers,
    /// and RPC targets -- because they are the same thing: an id this peer
    /// assigned to a node, which each remote peer learns and confirms
    /// separately. Ids are never reused or renumbered, since a peer that has
    /// already cached one would otherwise resolve it to the wrong node.
    paths: Vec<PathEntry>,
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
    rpcs_sent: u64,
    /// A SPAWN being applied: set before its node is added to the tree, and
    /// consumed as that node's synchronizers register.
    pending: Option<Pending>,
    spawns_received: u32,
    despawns_received: u32,
    rpcs_received: u64,
    /// Peers this API believes are connected.
    ///
    /// Tracked here because nothing else can: `MultiplayerPeer` has no peer
    /// list, and keeping one is precisely the job of the `MultiplayerAPI` this
    /// class replaces. `get_peer_ids` is one of the nine virtuals and was
    /// returning an empty vector until this existed.
    peers: Vec<i32>,
}

/// A path this peer has announced, and to whom.
struct PathEntry {
    node: InstanceId,
    id: u32,
    path: String,
    hash: String,
    sent_to: Vec<i32>,
    confirmed_by: Vec<i32>,
}

/// A received SPAWN whose node is being added.
///
/// Godot matches a spawn's synchronizer ids to synchronizers by registration
/// order: the source's own error text refers to a "pending spawn" and to a
/// synchronizer "unable to process the pending spawn since it has no network
/// ID". So the ids and the state are held here, the node is added, and each
/// synchronizer that registers for this node takes the next id and its share
/// of the state.
struct Pending {
    node: InstanceId,
    net_id: u32,
    sync_ids: std::collections::VecDeque<u32>,
    state: Vec<u8>,
    offset: usize,
}

/// A node that arrived through a spawner.
struct Spawned {
    spawner: Gd<MultiplayerSpawner>,
    node: Gd<Node>,
    /// Which peers have been sent its SPAWN.
    sent_to: Vec<i32>,
    /// True when a peer spawned this for us, so it is never spawned back.
    remote: bool,
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
            // direction: an offline-authority probe measured that an
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
            peer_events: Rc::new(RefCell::new(Vec::new())),
            spawned: Vec::new(),
            next_net_id: 1,
            spawns_sent: 0,
            paths: Vec::new(),
            incoming: std::collections::BTreeMap::new(),
            remote_paths: Vec::new(),
            applied: 0,
            refused: 0,
            remote_sender: 0,
            despawns_sent: 0,
            sync_counter: 0,
            syncs_sent: 0,
            last_sync_bytes: 0,
            rpcs_sent: 0,
            pending: None,
            spawns_received: 0,
            despawns_received: 0,
            rpcs_received: 0,
        }
    }

    fn poll(&mut self) -> GodotError {
        self.polls += 1;
        if let Some(peer) = self.peer.as_mut() {
            peer.poll();
        }
        self.track_peers();
        self.read_incoming();
        if !self.peers.is_empty() {
            self.send_to_peers();
            self.send_sync();
        }
        // Every hundredth, so a long session leaves a trail without drowning
        // the interesting lines.
        if self.polls.is_multiple_of(100) {
            godot_print!(
                "[replication] polls={} watching={} syncs_sent={} last_sync={} bytes rpcs={} \
                 rpcs_sent={} announced={} confirmed={} rejected={} spawns={} despawns={} \
                 applied={} refused={} spawns_in={} despawns_in={} rpcs_in={}",
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
                self.refused,
                self.spawns_received,
                self.despawns_received,
                self.rpcs_received
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
        self.peer_events.borrow_mut().clear();
        self.peer = multiplayer_peer;
        // Both directions come from the transport: a server learns each
        // joining peer's id here, and a client learns the server's (always 1)
        // the moment the link is up -- which is what a stock client reports as
        // `peers=[1]` in its own join line.
        if let Some(peer) = self.peer.as_mut() {
            for (signal, connected) in [("peer_connected", true), ("peer_disconnected", false)] {
                let queue = Rc::clone(&self.peer_events);
                peer.connect(
                    signal,
                    &Callable::from_fn(signal, move |args: &[&Variant]| {
                        if let Some(id) = args.first().and_then(|v| v.try_to::<i64>().ok()) {
                            queue.borrow_mut().push((id as i32, connected));
                        }
                    }),
                );
            }
        }
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
        if peer != local && !self.peers.is_empty() {
            // Anything owed to a peer goes out before the call does. A script
            // can call an RPC in the same frame the node was spawned -- the
            // player's `shoot` is called the frame a bullet is added, and
            // `land` can be called on a player's first frame -- and this
            // class batches spawns into `poll` while an RPC leaves as soon as
            // the script makes it. Without this flush the call reached the
            // far side first, and a stock client logged
            // "Failed to get path from RPC: main/Level/SpawnedNodes/<peer>"
            // followed by "Requested node was not found".
            self.send_to_peers();
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
                // A node we are adding because a peer spawned it is recorded
                // as already sent, under the peer's object id, so that it is
                // never spawned back and its DESPAWN can find it.
                let remote = self
                    .pending
                    .as_ref()
                    .filter(|p| p.node == node.instance_id())
                    .map(|p| p.net_id);
                self.spawned.push(Spawned {
                    spawner,
                    node,
                    sent_to: Vec::new(),
                    remote: remote.is_some(),
                    net_id: remote.unwrap_or(0),
                });
            }
            return GodotError::OK;
        }
        let Ok(sync) = configuration.try_to::<Gd<MultiplayerSynchronizer>>() else {
            return GodotError::OK;
        };
        let object_id = object.instance_id();
        self.watch(object, &sync);
        self.adopt_pending(object_id);
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
        let local = self.get_unique_id();
        if !entry.remote
            && entry.spawner.is_instance_valid()
            && entry.spawner.get_multiplayer_authority() == local
        {
            // To the peers that were told it existed, and only those.
            let told: Vec<i32> = entry.sent_to.clone();
            if let Some(peer) = self.peer.as_mut() {
                let bytes = encode_despawn(entry.net_id);
                for id in told {
                    peer.set_target_peer(id);
                    peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
                    self.despawns_sent += 1;
                }
                peer.set_target_peer(0);
            }
        }
        GodotError::OK
    }
}

impl ReplicationApi {
    /// Takes the peers the transport reported since the last tick onto the
    /// list, and tells the scripts.
    ///
    /// Nothing is renumbered when someone joins, and nothing is resent
    /// wholesale. Each path and each spawn records which peers have had it, so
    /// a new peer simply has none of them yet and `send_to_peers` fills it in
    /// over the following ticks. Renumbering would break the peers already
    /// connected, whose caches hold the old ids.
    ///
    /// The signals go out synchronously, and that is load-bearing rather than
    /// incidental. `level.gd` creates the joining client's player from
    /// `peer_connected`, so emitting it deferred put that player's SPAWN a
    /// tick behind the rest of the join -- after the first SYNC, where a stock
    /// server sends it before. Emitting through `base_mut()` lets the
    /// handler's `add_child` register the new node back into this same object
    /// while `poll` is still on the stack, which is what `base_mut()` is for;
    /// the panic that made the earlier version defer everything comes from
    /// handlers reached any other way (see `peer_events`).
    fn track_peers(&mut self) {
        let events = std::mem::take(&mut *self.peer_events.borrow_mut());
        let local = self.get_unique_id();
        for (id, connected) in events {
            godot_print!(
                "[replication] peer {id} {}",
                if connected { "up" } else { "down" }
            );
            if connected {
                if !self.peers.contains(&id) {
                    self.peers.push(id);
                }
            } else {
                self.peers.retain(|&known| known != id);
                self.paths.iter_mut().for_each(|entry| {
                    entry.sent_to.retain(|&known| known != id);
                    entry.confirmed_by.retain(|&known| known != id);
                });
                self.spawned
                    .iter_mut()
                    .for_each(|entry| entry.sent_to.retain(|&known| known != id));
                self.remote_paths.retain(|(peer, _, _)| *peer != id);
            }
            let signal = if connected {
                "peer_connected"
            } else {
                "peer_disconnected"
            };
            self.base_mut().emit_signal(signal, &[id.to_variant()]);
            // A client's link to the server counts as being connected to it.
            if connected && id == 1 && local != 1 {
                self.base_mut().emit_signal("connected_to_server", &[]);
            }
        }
    }

    /// Gives every connected peer the paths and spawns it has not had yet.
    ///
    /// Per peer throughout, which is what lets a second client join a session
    /// already in progress: the first client keeps its ids, and the new one
    /// receives the same table from the beginning. Renumbering on each join --
    /// which this did while there was only ever one client -- would have left
    /// the first client resolving stale ids to the wrong nodes.
    ///
    /// Ordering is a constraint in both directions, which is why the passes
    /// run announce, spawn, announce. A spawn names its spawner by path id, so
    /// that path must reach a peer first; and a path *under* a spawned node
    /// cannot be resolved until that node's spawn has reached the same peer,
    /// so the second pass picks up what the first had to skip. Both are
    /// checked per peer, and stock puts all of it in one tick -- a joining
    /// client's whole world arrives before the first SYNC.
    fn send_to_peers(&mut self) {
        let local = self.get_unique_id();
        let peers: Vec<i32> = self.peers.clone();
        if peers.is_empty() {
            return;
        }
        // Forget dead entries with nothing outstanding. Ids stay monotonic --
        // `next_cache_id` never goes back -- so forgetting one cannot give a
        // peer two meanings for the same id. One that has been announced but
        // not yet confirmed is kept, so the confirmation still finds it.
        self.paths.retain(|entry| {
            Gd::<Node>::try_from_instance_id(entry.node).is_ok()
                || entry
                    .sent_to
                    .iter()
                    .any(|id| !entry.confirmed_by.contains(id))
        });

        // Every owned spawner gets its path id before anything is sent. The
        // id has to exist by the time the announcing pass below runs, because
        // a SPAWN names its spawner by it: assigning it later -- in the spawns
        // loop, where it was first needed -- left the spawner's path announced
        // on one tick and the spawns waiting for the next. In that one-tick
        // gap `register_owned_paths` saw synchronizers with no spawn id yet
        // and handed them path ids of their own, so the join sent seven
        // `SIMPLIFY_PATH` packets a stock server does not send.
        for index in 0..self.spawned.len() {
            if self.spawned[index].remote {
                continue;
            }
            let spawner = self.spawned[index].spawner.clone();
            if !spawner.is_instance_valid() || spawner.get_multiplayer_authority() != local {
                continue;
            }
            self.path_id_of(spawner.upcast());
        }

        self.announce_paths(&peers);
        self.send_spawns(&peers, local);
        // Ids for the synchronizers no SPAWN listed, now that the spawns just
        // sent have claimed theirs, and a second announcing pass for them and
        // for anything else under a node this call spawned. Stock sends those
        // in this same tick -- the two BulletCache paths land between the last
        // SPAWN and the first SYNC -- and one pass left them a tick late.
        self.register_owned_paths(local);
        self.announce_paths(&peers);
    }

    /// Announces every path a peer can resolve and has not been sent.
    fn announce_paths(&mut self, peers: &[i32]) {
        for index in 0..self.paths.len() {
            // A dead node's id is kept, so it is never handed out twice, but
            // it is never announced again. Bullets make this matter: each one
            // takes a path id and is freed a second or two later, so by the
            // time a second client joins most of the table names nodes that
            // no longer exist. Announcing those to it made a stock client log
            // a "Node not found: main/Level/SpawnedNodes/Bullet2" for each --
            // and the names are recycled, so a late announcement could also
            // have pointed an id at whichever bullet holds the name now.
            if !Gd::<Node>::try_from_instance_id(self.paths[index].node).is_ok_and(|n| {
                // A node on its way out of the tree is as good as gone.
                n.is_inside_tree()
            }) {
                continue;
            }
            for &peer_id in peers {
                if self.paths[index].sent_to.contains(&peer_id) {
                    continue;
                }
                let under_unsent = {
                    let node = self.paths[index].node;
                    self.spawned.iter().any(|e| {
                        !e.remote
                            && !e.sent_to.contains(&peer_id)
                            && e.node.is_instance_valid()
                            && Gd::<Node>::try_from_instance_id(node).is_ok_and(|n| {
                                e.node.is_ancestor_of(&n) || e.node.instance_id() == node
                            })
                    })
                };
                if under_unsent {
                    continue;
                }
                let bytes = SimplifyPath {
                    id: self.paths[index].id,
                    path: self.paths[index].path.clone(),
                    rpc_hash: self.paths[index].hash.clone(),
                }
                .encode();
                self.send_to(peer_id, &bytes);
                self.paths[index].sent_to.push(peer_id);
                self.announced += 1;
            }
        }
    }

    /// Sends a SPAWN to every peer that has the spawner's path and has not
    /// been told about the node.
    fn send_spawns(&mut self, peers: &[i32], local: i32) {
        for index in 0..self.spawned.len() {
            if self.spawned[index].remote || !self.spawned[index].node.is_instance_valid() {
                continue;
            }
            let spawner = self.spawned[index].spawner.clone();
            if !spawner.is_instance_valid() || spawner.get_multiplayer_authority() != local {
                continue;
            }
            let Some(spawner_id) = self.path_id_of(spawner.clone().upcast()) else {
                continue;
            };
            for &peer_id in peers {
                if self.spawned[index].sent_to.contains(&peer_id) {
                    continue;
                }
                if !self
                    .paths
                    .iter()
                    .any(|e| e.id == spawner_id && e.sent_to.contains(&peer_id))
                {
                    continue;
                }
                let Some(bytes) = self.build_spawn(index, spawner_id, local) else {
                    self.spawned[index].sent_to.push(peer_id);
                    continue;
                };
                self.send_to(peer_id, &bytes);
                self.spawned[index].sent_to.push(peer_id);
                self.spawns_sent += 1;
            }
        }
    }

    /// The SPAWN packet for a node, assigning object ids the first time.
    ///
    /// The ids are assigned once and reused for every later peer, so two
    /// clients address the same node by the same id.
    fn build_spawn(&mut self, index: usize, spawner_id: u32, local: i32) -> Option<Vec<u8>> {
        let node = self.spawned[index].node.clone();
        let scene = scene_index(&self.spawned[index].spawner, &node).or_else(|| {
            godot_error!(
                "[replication] {} is not in its spawner's scene list",
                node.get_name()
            );
            None
        })?;
        let node_id = node.instance_id();
        let name = node.get_name().to_string();

        let mut sync_ids = Vec::new();
        let mut state = Vec::new();
        let fresh = self.spawned[index].net_id == 0;
        let net_id = if fresh {
            self.next_net_id
        } else {
            self.spawned[index].net_id
        };
        for watched in &mut self.watched {
            // Listed only if it acts on this node, is ours, and is visible --
            // which is why a robot lists one synchronizer and not four (its
            // parts are invisible), and why a client's player lists one and
            // the host's two (the input half belongs to its own peer).
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
        if fresh {
            self.next_net_id = net_id + 1 + u32::try_from(sync_ids.len()).unwrap_or(0);
            self.spawned[index].net_id = net_id;
        }
        Some(
            Spawn {
                scene,
                spawner: spawner_id,
                net_id,
                sync_ids,
                name,
                state,
            }
            .encode(),
        )
    }

    /// The id this peer announced for a node, assigning one if it has none.
    fn path_id_of(&mut self, node: Gd<Node>) -> Option<u32> {
        let key = node.instance_id();
        if let Some(entry) = self.paths.iter().find(|e| e.node == key) {
            return Some(entry.id);
        }
        let path = relative_path(&node)?;
        let id = self.next_cache_id;
        self.next_cache_id += 1;
        self.paths.push(PathEntry {
            node: key,
            id,
            path,
            hash: script_rpc_hash(&node),
            sent_to: Vec::new(),
            confirmed_by: Vec::new(),
        });
        Some(id)
    }

    /// One packet to one peer, reliably.
    fn send_to(&mut self, peer_id: i32, bytes: &[u8]) {
        if let Some(peer) = self.peer.as_mut() {
            peer.set_target_peer(peer_id);
            peer.put_packet(&PackedByteArray::from(bytes));
            peer.set_target_peer(0);
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
                COMMAND_CONFIRM_PATH => self.on_confirm_path(from, &bytes),
                COMMAND_SIMPLIFY_PATH => self.on_simplify_path(from, &bytes),
                COMMAND_SYNC => self.on_sync(from, &bytes, seen == 1),
                COMMAND_SPAWN => self.on_spawn(from, &bytes),
                COMMAND_DESPAWN => self.on_despawn(from, &bytes),
                LEAD_CACHED | LEAD_PATH => self.on_rpc(from, &bytes),
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

    fn on_confirm_path(&mut self, from: i32, bytes: &[u8]) {
        match ConfirmPath::parse(bytes) {
            Ok(answer) if answer.valid => {
                self.confirmed += 1;
                godot_print!("[replication] confirmed id {}", answer.id);
                for entry in &mut self.paths {
                    if entry.id == answer.id && !entry.confirmed_by.contains(&from) {
                        entry.confirmed_by.push(from);
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
        // Only failures are worth a line each: a shooting session announces
        // every bullet's path, dozens of them.
        if !valid {
            godot_error!(
                "[replication] peer {from} announced id {} -> {}, which is not here",
                announced.id,
                announced.path
            );
        }
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
            // Two addressings. With the top bit, the path id the sender
            // announced for that synchronizer; without, the id a SPAWN from
            // the sender gave it.
            let watched = if record.net_id & 0x8000_0000 != 0 {
                let id = record.net_id & 0x7fff_ffff;
                let Some(node) = self
                    .remote_paths
                    .iter()
                    .find(|(p, i, _)| *p == from && *i == id)
                    .map(|(_, _, n)| n.instance_id())
                else {
                    continue;
                };
                self.watched
                    .iter()
                    .find(|w| w.sync.is_instance_valid() && w.sync.instance_id() == node)
            } else {
                // Spawn ids are the sender's; only one server spawns here.
                self.watched
                    .iter()
                    .find(|w| w.spawn_id == Some(record.net_id) && w.sync.is_instance_valid())
            };
            let Some(watched) = watched else {
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

    /// Gives every owned synchronizer no SPAWN listed a path id.
    ///
    /// Such a synchronizer can only be addressed by path: for a client that is
    /// its InputSynchronizer, and for a server each player's BulletCache
    /// synchronizer. Announcing the id, and waiting for a peer to confirm it,
    /// is `send_to_peers`' job -- this only decides that one is needed.
    fn register_owned_paths(&mut self, local: i32) {
        for index in 0..self.watched.len() {
            let watched = &self.watched[index];
            if watched.spawn_id.is_some()
                || watched.cache_id.is_some()
                || !watched.sync.is_instance_valid()
                || !watched.sync.is_visibility_public()
                || watched.sync.get_multiplayer_authority() != local
            {
                continue;
            }
            let owner = watched.sync.clone().upcast::<Node>();
            self.watched[index].cache_id = self.path_id_of(owner);
        }
    }

    /// A peer spawned a node. Instantiate it here.
    fn on_spawn(&mut self, from: i32, bytes: &[u8]) {
        let spawn = match Spawn::parse(bytes) {
            Ok(s) => s,
            Err(error) => {
                godot_error!("[replication] malformed SPAWN from {from}: {error:?}");
                return;
            }
        };
        let Some(spawner) = self
            .remote_paths
            .iter()
            .find(|(p, i, _)| *p == from && *i == spawn.spawner)
            .and_then(|(_, _, n)| n.clone().try_cast::<MultiplayerSpawner>().ok())
        else {
            godot_error!(
                "[replication] SPAWN {} names spawner {} which peer {from} never announced",
                spawn.name,
                spawn.spawner
            );
            return;
        };
        // Only the spawner's authority may spawn through it.
        if spawner.get_multiplayer_authority() != from {
            godot_error!("[replication] peer {from} may not spawn {}", spawn.name);
            return;
        }
        let Some(scene) = spawnable_scene(&spawner, spawn.scene) else {
            godot_error!("[replication] spawner has no scene {}", spawn.scene);
            return;
        };
        let Some(mut parent) = spawner.get_node_or_null(&spawner.get_spawn_path()) else {
            return;
        };
        let Some(mut node) = scene.instantiate() else {
            return;
        };
        node.set_name(spawn.name.as_str());

        self.pending = Some(Pending {
            node: node.instance_id(),
            net_id: spawn.net_id,
            sync_ids: spawn.sync_ids.iter().copied().collect(),
            state: spawn.state,
            offset: 0,
        });
        {
            // Adding the node registers its spawner entry and synchronizers
            // with this API, re-entrantly.
            let _reentrant = self.base_mut();
            parent.add_child(&node);
        }
        // Recorded here rather than left to the spawner's registration: a
        // MultiplayerSpawner only tracks the children it is the authority
        // for, so on a client it never registers them. Without this entry a
        // DESPAWN has nothing to match -- measured: 56 spawns in, 0 despawns
        // applied, and the client's stale bullets colliding by name with new
        // ones, which Godot then renamed.
        if !self
            .spawned
            .iter()
            .any(|e| e.node.instance_id() == node.instance_id())
        {
            self.spawned.push(Spawned {
                spawner: spawner.clone(),
                node: node.clone(),
                sent_to: Vec::new(),
                remote: true,
                net_id: spawn.net_id,
            });
        }
        if let Some(left) = self.pending.take() {
            if !left.sync_ids.is_empty() || left.offset != left.state.len() {
                godot_error!(
                    "[replication] spawn {}: {} synchronizer ids and {} state bytes unused",
                    spawn.name,
                    left.sync_ids.len(),
                    left.state.len() - left.offset
                );
            }
        }
        self.spawns_received += 1;
    }

    /// Hands the pending spawn's next synchronizer id and state to the
    /// synchronizer that just registered, if it belongs to that node.
    fn adopt_pending(&mut self, object: InstanceId) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if pending.node != object {
            return;
        }
        let Some(watched) = self.watched.last_mut() else {
            return;
        };
        let Some(id) = pending.sync_ids.pop_front() else {
            // Registered, but not listed: a synchronizer the spawning peer
            // does not own, such as a client's own InputSynchronizer.
            return;
        };
        watched.spawn_id = Some(id);
        for path in &watched.spawn_props {
            let Ok((value, next)) = decode_compact(&pending.state, pending.offset) else {
                godot_error!("[replication] spawn state ends inside {path}");
                return;
            };
            pending.offset = next;
            if let Some((mut target, property)) = resolve(&watched.object, path) {
                target.set_indexed(&property, &to_variant(&value));
            }
        }
    }

    /// A peer removed a node it spawned.
    fn on_despawn(&mut self, from: i32, bytes: &[u8]) {
        let Ok(net_id) = parse_despawn(bytes) else {
            return;
        };
        let Some(index) = self.spawned.iter().position(|e| {
            e.remote
                && e.net_id == net_id
                && e.spawner.is_instance_valid()
                && e.spawner.get_multiplayer_authority() == from
        }) else {
            return;
        };
        let mut entry = self.spawned.remove(index);
        if entry.node.is_instance_valid() {
            entry.node.queue_free();
        }
        self.despawns_received += 1;
    }

    /// A peer called a method on one of our nodes.
    fn on_rpc(&mut self, from: i32, bytes: &[u8]) {
        let call = match RemoteCall::parse(bytes) {
            Ok(c) => c,
            Err(error) => {
                godot_error!("[replication] malformed RPC from {from}: {error:?}");
                return;
            }
        };
        let (node, method_id) = match &call {
            RemoteCall::Cached { cache_id, method } => (
                self.remote_paths
                    .iter()
                    .find(|(p, i, _)| *p == from && *i == u32::from(*cache_id))
                    .map(|(_, _, n)| n.clone()),
                *method,
            ),
            RemoteCall::Path { method, path } => (
                scene_root().and_then(|r| r.get_node_or_null(&NodePath::from(path.as_str()))),
                *method,
            ),
        };
        let Some(node) = node else {
            return;
        };
        let names = script_rpc_names(&node);
        let Some(method) = RpcConfig::new(names)
            .method_of(method_id.into())
            .map(str::to_owned)
        else {
            godot_error!("[replication] {} has no RPC {method_id}", node.get_name());
            return;
        };
        let method = StringName::from(method.as_str());
        // `rpc_mode`: 1 is any peer, 2 is the node's authority only, 0 is off.
        match rpc_mode(&node.clone().upcast(), &method) {
            1 => {}
            2 if node.get_multiplayer_authority() == from => {}
            mode => {
                godot_error!(
                    "[replication] peer {from} may not call {}::{method} (rpc_mode {mode})",
                    node.get_name()
                );
                return;
            }
        }
        self.rpcs_received += 1;
        self.remote_sender = from;
        {
            let _reentrant = self.base_mut();
            let mut target = node.upcast::<Object>();
            target.callv(&method, &VarArray::new());
        }
        self.remote_sender = 0;
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
        // Each record with the peers that can resolve the id it is addressed
        // by: those sent the SPAWN that listed it, or those that confirmed its
        // path. The two can differ per peer at any moment -- a client that
        // joined a second ago has neither for most of the world -- so the
        // packet is composed per peer below rather than broadcast. With one
        // client that is the same bytes on the wire as before.
        let mut records: Vec<(Vec<i32>, SyncRecord)> = Vec::new();
        for watched in &self.watched {
            if !watched.object.is_instance_valid()
                || !watched.sync.is_instance_valid()
                || !watched.sync.is_visibility_public()
                || watched.sync.get_multiplayer_authority() != local
            {
                continue;
            }
            // Listed in a SPAWN: its spawn id. Otherwise its confirmed path id
            // with the top bit -- in the capture, the server's two
            // BulletCache synchronizers (0x80000002, 0x80000003) and a
            // client's InputSynchronizer (0x80000001).
            let (net_id, audience) = match (watched.spawn_id, watched.cache_id) {
                (Some(id), _) => {
                    let object = watched.object.instance_id();
                    let Some(entry) = self.spawned.iter().find(|e| e.node.instance_id() == object)
                    else {
                        continue;
                    };
                    (id, entry.sent_to.clone())
                }
                (None, Some(id)) => {
                    let Some(entry) = self.paths.iter().find(|e| e.id == id) else {
                        continue;
                    };
                    (id | 0x8000_0000, entry.confirmed_by.clone())
                }
                _ => continue,
            };
            if audience.is_empty() {
                continue;
            }
            let fields = Self::read(&watched.object, &watched.streamed);
            if fields.is_empty() {
                continue;
            }
            records.push((audience, SyncRecord { net_id, fields }));
        }
        if records.is_empty() {
            return;
        }
        // One counter for the tick, not one per packet: the peers are being
        // told about the same moment.
        self.sync_counter = self.sync_counter.wrapping_add(1);
        let counter = self.sync_counter;
        for peer_id in self.peers.clone() {
            let mine: Vec<SyncRecord> = records
                .iter()
                .filter(|(audience, _)| audience.contains(&peer_id))
                .map(|(_, record)| record.clone())
                .collect();
            if mine.is_empty() {
                continue;
            }
            let bytes = encode(&SyncPacket {
                counter,
                records: mine,
            });
            let Some(peer) = self.peer.as_mut() else {
                return;
            };
            peer.set_target_peer(peer_id);
            peer.set_transfer_mode(TransferMode::UNRELIABLE);
            peer.put_packet(&PackedByteArray::from(bytes.as_slice()));
            peer.set_transfer_mode(TransferMode::RELIABLE);
            peer.set_target_peer(0);
            self.syncs_sent += 1;
            self.last_sync_bytes = bytes.len();
        }
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
        let Some(path_id) = self.path_id_of(node) else {
            return;
        };

        // Per peer, because two clients can be at different stages: one that
        // has confirmed this path takes the three-byte form, one that has not
        // gets the long form carrying the path.
        let targets: Vec<i32> = if peer_id == 0 {
            self.peers.clone()
        } else {
            vec![peer_id]
        };
        for id in targets {
            let confirmed = self
                .paths
                .iter()
                .any(|e| e.id == path_id && e.confirmed_by.contains(&id));
            let call = match u8::try_from(path_id) {
                Ok(cache_id) if confirmed => RemoteCall::Cached {
                    cache_id,
                    method: method_id,
                },
                // The long form carries the path itself, at a fixed offset.
                _ => RemoteCall::Path {
                    method: method_id,
                    path: path.clone(),
                },
            };
            let bytes = call.encode();
            self.send_to(id, &bytes);
            self.rpcs_sent += 1;
        }
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

/// The scene a spawner lists at `index`, loaded.
fn spawnable_scene(spawner: &Gd<MultiplayerSpawner>, index: u8) -> Option<Gd<PackedScene>> {
    let listed = spawner.get_spawnable_scene(i32::from(index)).to_string();
    ResourceLoader::singleton()
        .load(listed.as_str())?
        .try_cast::<PackedScene>()
        .ok()
}

/// A method's `rpc_mode` from its script's config, or 0 if it is not an RPC.
fn rpc_mode(object: &Gd<Object>, method: &StringName) -> i64 {
    let Some(script) = object.get_script() else {
        return 0;
    };
    let entry = script.get_rpc_config().call("get", &[method.to_variant()]);
    if entry.is_nil() {
        return 0;
    }
    entry
        .call("get", &["rpc_mode".to_variant(), 0.to_variant()])
        .try_to::<i64>()
        .unwrap_or(0)
}
