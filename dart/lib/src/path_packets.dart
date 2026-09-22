// SPDX-License-Identifier: Apache-2.0
//
// The three packets that name things: SIMPLIFY_PATH, CONFIRM_PATH, DESPAWN
// (plan.md DR-7, DR-7a).
//
// ## Why a path gets an id at all
//
// Every later packet about a node addresses it by a small integer, and this is
// the exchange that hands one out. The sender says "from now on, id 7 means
// `main/Level/SpawnedNodes/Bullet2`", the receiver resolves that path in its
// own tree and answers whether it found it. A CONFIRM_PATH carrying false is
// not a transport failure -- it means the two scene trees disagree about what
// exists, which is the failure this exchange surfaces early rather than at the
// first RPC.
//
// ## Layouts, measured
//
//     SIMPLIFY_PATH  01 | 32 bytes of md5, ASCII hex | 00 | u32 id | path | 00
//     CONFIRM_PATH   02 | u8 valid | u32 id
//     DESPAWN        05 | u32 id
//
// The md5 is of the node's sorted RPC method names joined -- see
// `rpc_config.dart`, which is where that hash is built. It is how both ends
// check they agree about a node's remote-call table before either uses a
// method *number*.
//
// The path has no leading slash and no `/root/`: it is spelled from the scene
// root, `main/Level/...`. A path announced as `root/main/...` resolved on
// nobody's tree and the far side logged a node-not-found for every packet
// that used the id.

import 'dart:convert';
import 'dart:typed_data';

/// `SceneMultiplayer`'s command bytes for the three.
const int commandSimplifyPath = 0x01;
const int commandConfirmPath = 0x02;
const int commandDespawn = 0x05;

/// The md5 is written as 32 ASCII hex characters, then a NUL.
const int rpcHashLength = 32;

/// "Address this node by [id] from now on."
class SimplifyPath {
  const SimplifyPath({
    required this.id,
    required this.path,
    required this.rpcHash,
  });

  /// The id this node will be addressed by.
  final int id;

  /// The node's path, as the sender's scene tree spells it.
  final String path;

  /// The md5 of the node's sorted RPC method names, as ASCII hex.
  final String rpcHash;

  static SimplifyPath parse(final Uint8List packet) {
    if (packet.isEmpty || packet[0] != commandSimplifyPath) {
      throw const FormatException('not a SIMPLIFY_PATH packet');
    }
    if (packet.length < 2 + rpcHashLength + 4) {
      throw const FormatException('SIMPLIFY_PATH is cut short');
    }
    final Uint8List hash = packet.sublist(1, 1 + rpcHashLength);
    if (!hash.every(_isHexDigit)) {
      throw const FormatException('the rpc hash is not ASCII hex');
    }
    if (packet[1 + rpcHashLength] != 0) {
      throw const FormatException('the rpc hash is not NUL-terminated');
    }
    final int idAt = 2 + rpcHashLength;
    final int id =
        ByteData.sublistView(packet).getUint32(idAt, Endian.little);
    final int pathAt = idAt + 4;
    final int end = packet.indexOf(0, pathAt);
    if (end < 0) {
      throw const FormatException('the path is not NUL-terminated');
    }
    return SimplifyPath(
      id: id,
      path: ascii.decode(packet.sublist(pathAt, end)),
      rpcHash: ascii.decode(hash),
    );
  }

  Uint8List encode() {
    final BytesBuilder out = BytesBuilder(copy: false)
      ..addByte(commandSimplifyPath)
      ..add(ascii.encode(rpcHash))
      ..addByte(0);
    final ByteData idBytes = ByteData(4)..setUint32(0, id, Endian.little);
    out
      ..add(idBytes.buffer.asUint8List())
      ..add(utf8.encode(path))
      ..addByte(0);
    return out.takeBytes();
  }
}

/// The answer: resolved, or not.
class ConfirmPath {
  const ConfirmPath({required this.id, required this.valid});

  final int id;

  /// False means the receiver could not find that path in its own tree.
  final bool valid;

  static ConfirmPath parse(final Uint8List packet) {
    if (packet.isEmpty || packet[0] != commandConfirmPath) {
      throw const FormatException('not a CONFIRM_PATH packet');
    }
    if (packet.length < 6) {
      throw const FormatException('CONFIRM_PATH is cut short');
    }
    return ConfirmPath(
      id: ByteData.sublistView(packet).getUint32(2, Endian.little),
      valid: packet[1] != 0,
    );
  }

  Uint8List encode() {
    final ByteData out = ByteData(6)
      ..setUint8(0, commandConfirmPath)
      ..setUint8(1, valid ? 1 : 0)
      ..setUint32(2, id, Endian.little);
    return out.buffer.asUint8List();
  }
}

/// Reads a DESPAWN, returning the object id that is going away.
///
/// The id is the one a SPAWN handed out, not a path cache id: a node is
/// despawned by the identity it was spawned under.
int parseDespawn(final Uint8List packet) {
  if (packet.isEmpty || packet[0] != commandDespawn) {
    throw const FormatException('not a DESPAWN packet');
  }
  if (packet.length < 5) {
    throw const FormatException('DESPAWN is cut short');
  }
  return ByteData.sublistView(packet).getUint32(1, Endian.little);
}

Uint8List encodeDespawn(final int id) {
  final ByteData out = ByteData(5)
    ..setUint8(0, commandDespawn)
    ..setUint32(1, id, Endian.little);
  return out.buffer.asUint8List();
}

bool _isHexDigit(final int byte) =>
    (byte >= 0x30 && byte <= 0x39) ||
    (byte >= 0x61 && byte <= 0x66) ||
    (byte >= 0x41 && byte <= 0x46);
