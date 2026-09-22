// SPDX-License-Identifier: Apache-2.0

/// Godot 4.5's high-level multiplayer protocol, measured off the wire.
///
/// The same protocol as the Rust crate beside this package, implemented
/// separately and deliberately so. The two are checked against one corpus of
/// bytes a stock engine produced -- `replication/tests/fixtures/`, which both
/// test suites read -- and two independent decoders held to one corpus
/// disagree loudly when either is wrong. A binding would have nothing to
/// disagree with.
///
/// What is here is the wire format and nothing above it: no session, no
/// spawner, no notion of what a game object is. Those belong to the
/// application, which is why the field-pull seam and the authority live with
/// the game rather than here.
library;

export 'src/path_packets.dart';
export 'src/remote_call.dart';
export 'src/replication_table.dart';
export 'src/rpc_config.dart';
export 'src/spawn.dart';
export 'src/sync.dart';
export 'src/variant.dart';
