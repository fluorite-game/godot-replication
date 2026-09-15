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
    IMultiplayerApiExtension, MultiplayerApiExtension, MultiplayerPeer, OfflineMultiplayerPeer,
};
use godot::global::Error as GodotError;
use godot::prelude::*;

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
            godot_print!(
                "[replication] polls={} registrations={} rpcs={} peers={:?}",
                self.polls,
                self.registrations,
                self.rpcs,
                self.peer_ids()
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
        // The question this version exists to answer. A synchronizer or a
        // spawner hands itself over here, and what arrives decides how the
        // real implementation reads the field list.
        godot_print!(
            "[replication] configuration_add: object={} configuration={} ({:?})",
            object
                .as_ref()
                .map_or_else(|| "none".to_string(), |o| o.get_class().to_string()),
            configuration,
            configuration.get_type()
        );
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
    fn peer_ids(&self) -> Vec<i32> {
        // Nothing to enumerate until the real implementation tracks peers; the
        // shape is here so the virtual is not lying about its type.
        Vec::new()
    }
}
