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

use godot::classes::{
    IMultiplayerApiExtension, MultiplayerApiExtension, MultiplayerPeer, MultiplayerSynchronizer,
    OfflineMultiplayerPeer,
};
use godot::global::Error as GodotError;
use godot::prelude::*;
use godot_replication::sync::{encode, SyncPacket, SyncRecord};
use godot_replication::variant::Value;

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
        }
    }

    fn poll(&mut self) -> GodotError {
        self.polls += 1;
        if let Some(peer) = self.peer.as_mut() {
            peer.poll();
        }
        // Every hundredth, so a long session leaves a trail without drowning
        // the interesting lines.
        if self.polls.is_multiple_of(100) {
            let packet = self.build_sync();
            godot_print!(
                "[replication] polls={} watching={} records={} sync={} bytes rpcs={}",
                self.polls,
                self.watched.len(),
                packet.records.len(),
                encode(&packet).len(),
                self.rpcs
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
        PackedInt32Array::from(self.peer_ids().as_slice())
    }

    fn rpc(
        &mut self,
        peer: i32,
        object: Option<Gd<Object>>,
        method: StringName,
        _args: VarArray,
    ) -> GodotError {
        self.rpcs += 1;
        godot_print!(
            "[replication] rpc -> peer {peer}: {}::{method}",
            object
                .as_ref()
                .map_or_else(|| "?".to_string(), |o| o.get_class().to_string())
        );
        // OK rather than an error: an error here makes the caller report a
        // failure, and nothing is being sent yet either way. What this version
        // is for is learning which calls arrive.
        GodotError::OK
    }

    fn get_remote_sender_id(&self) -> i32 {
        0
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
        let Ok(sync) = configuration.try_to::<Gd<MultiplayerSynchronizer>>() else {
            // A MultiplayerSpawner, which is the other kind. Spawn and despawn
            // are their own packets and their own work.
            godot_print!(
                "[replication] configuration_add: spawner for {}",
                object.get_class()
            );
            return GodotError::OK;
        };
        self.watch(object, &sync);
        GodotError::OK
    }

    fn object_configuration_remove(
        &mut self,
        object: Option<Gd<Object>>,
        _configuration: Variant,
    ) -> GodotError {
        godot_print!(
            "[replication] configuration_remove: object={}",
            object
                .as_ref()
                .map_or_else(|| "none".to_string(), |o| o.get_class().to_string())
        );
        GodotError::OK
    }
}

impl ReplicationApi {
    /// Builds the packet this peer would send for the tick just run.
    ///
    /// One record per watched synchronizer, in registration order, each
    /// carrying its mode-ALWAYS properties read fresh off the node. This is
    /// what `poll` would hand the peer; it is not sent yet, because a send
    /// without a receive on the other side proves nothing and the receive is
    /// its own piece of work.
    ///
    /// A synchronizer whose node has gone is skipped rather than faulted: the
    /// demo frees bullets and robots constantly, and a stale entry in this
    /// list is expected traffic, not an error.
    fn build_sync(&mut self) -> SyncPacket {
        let mut records = Vec::new();
        // Net ids are placeholders until the path cache is implemented; the
        // shape and the field encoding are what this exercises.
        for (index, watched) in self.watched.iter().enumerate() {
            if !watched.object.is_instance_valid() || !watched.sync.is_instance_valid() {
                continue;
            }
            // Invisible synchronizers send nothing. Measured before it was
            // implemented: a capture of two peers playing contains 7, 9, 10 or
            // 11 records per packet, and the composition is always 1 input + 2
            // players + 4 robots + however many bullets are in the air. Not one
            // packet in 13901 carries a death part, because nothing died and
            // their visibility was never turned on.
            //
            // This is also why there is no packet-split rule to implement. The
            // size varies with the number of live bullets, not with a cap --
            // the largest observed is 877 bytes, far under any MTU.
            if !watched.sync.is_visibility_public() {
                continue;
            }
            let fields = Self::read(&watched.object, &watched.streamed);
            if fields.is_empty() {
                continue;
            }
            records.push(SyncRecord {
                net_id: 0x8000_0001 + u32::try_from(index).unwrap_or(0),
                fields,
            });
        }
        #[allow(clippy::cast_possible_truncation)]
        SyncPacket {
            counter: self.polls as u16,
            records,
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
        let mut modes = Vec::new();
        let mut config = config;
        for path in config.get_properties().iter_shared() {
            let mode = config.property_get_replication_mode(&path);
            modes.push(format!("{path}={mode:?}"));
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
        Some(target.get_indexed(&NodePath::from(property.to_string().as_str())))
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

    fn peer_ids(&self) -> Vec<i32> {
        // Nothing to enumerate until the real implementation tracks peers; the
        // shape is here so the virtual is not lying about its type.
        Vec::new()
    }
}
