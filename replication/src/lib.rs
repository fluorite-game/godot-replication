//! Godot 4.5's high-level multiplayer protocol, as measured off the wire.
//!
//! # Why this exists
//!
//! The Fluorite port of Godot's TPS demo has to play against a *stock,
//! unmodified* Godot build (plan.md DR-7). That means speaking
//! `SceneMultiplayer`'s protocol, which is an engine internal: undocumented,
//! and with no promise of stability between releases.
//!
//! Rather than implement it twice -- once in Dart for the port and once in
//! `GDScript` for the Godot side -- it is implemented here once and compiled
//! twice: as a `MultiplayerApiExtension` `GDExtension` for Godot, and as a
//! cdylib the port reaches over FFI. Two implementations that must agree
//! forever is the failure this avoids.
//!
//! # Everything here was measured, not read
//!
//! No part of this was taken from engine source or documentation. It comes
//! from captures in `/mnt/dev/tps-demo-perf/net-corpus`, produced by
//! `tools/capture_net.sh` (two peers playing the real demo) and
//! `tools/capture_sync_probe.sh` (two peers replicating one known value), and
//! read by `tools/net_corpus_report.py` and `tools/sync_frame_report.py`.
//!
//! That matters because of how this fails. A field decoded one byte out of
//! place yields a plausible number rather than an error, and a wrong RPC id is
//! still a valid id at the far end. Nothing complains; the game just behaves
//! slightly wrongly, forever.
//!
//! # The layers
//!
//! - [`variant`] -- Godot's `Variant` encoding, in both the plain form
//!   `var_to_bytes()` produces and the compact form SYNC packets carry.
//! - [`rpc`] -- how a node's RPC methods are numbered, and the hash the two
//!   ends exchange to agree on that numbering.
//! - [`sync`] -- the SYNC packet's framing.
//!
//! `ENet` is deliberately absent: Godot vendors upstream `ENet` and so should any
//! consumer of this crate, so that the transport is compatible by construction
//! rather than by reimplementation. The one thing worth recording here is that
//! across 17266 captured packets of real gameplay, the `ENet` header's
//! `COMPRESSED` flag was never set once.

pub mod path;
pub mod rpc;
pub mod sync;
pub mod variant;
