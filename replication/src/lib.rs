//! Godot 4.5's high-level multiplayer protocol, as measured off the wire.
//!
//! # Why this exists
//!
//! A program that is not Godot has to play against a *stock, unmodified*
//! Godot build. That means speaking `SceneMultiplayer`'s protocol, which is an
//! engine internal: undocumented, and with no promise of stability between
//! releases.
//!
//! This began as the networking half of a Fluorite port of Godot's TPS demo,
//! which is where the captures come from and why the covered subset is the
//! shape it is. See README.md for what is and is not implemented.
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
//! from packet captures of two peers playing the real demo, and of two peers
//! replicating one known value so that a single field could be isolated. The
//! bytes are under `tests/fixtures/`; the harness that produced them lives
//! with the game it was built for and is not part of this repository.
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
pub mod spawn;
pub mod sync;
pub mod variant;
