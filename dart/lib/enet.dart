// SPDX-License-Identifier: Apache-2.0

/// ENet as Godot's `ENetMultiplayerPeer` speaks it.
///
/// A separate entry point because it is a separate concern: the replication
/// protocol rides on this, but nothing about the framing needs it, and a
/// consumer bringing its own transport should not pay for a socket
/// implementation it will not use.
///
/// Implemented rather than bound, for reasons the captures settled: nothing in
/// 17266 packets was compressed, nothing was fragmented -- the largest was
/// 1057 bytes against a 1392-byte MTU -- there were two channels, and the
/// handshake was one command. Every genuinely hard part of the C library is
/// one this protocol never exercises, and binding it would have cost a native
/// artifact per platform in front of the tests.
library;

export 'src/enet/peer.dart';
export 'src/enet/protocol.dart';
