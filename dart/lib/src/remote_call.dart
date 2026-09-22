// SPDX-License-Identifier: Apache-2.0
//
// A remote call on the wire (plan.md DR-7, DR-7a).
//
// ## Two forms, and the lead byte says which
//
//     0x80  [u8 0x80][u8 cache_id][u8 method]                       3 bytes
//     0xa0  [u8 0xa0][u32 0x80000006][u8 method][path][NUL]
//
// The short form names a path id the receiver has already confirmed; the long
// one carries the path itself, for a call that cannot wait for the
// confirmation. In one shooting session the two appear in almost equal numbers
// -- 108 long and 106 short -- because bullets are spawned and destroyed
// constantly and each is exploded before its path comes back confirmed.
//
// ## The long form's 32-bit field is an offset, not an id
//
// It is `0x80000000 | 6` in every captured packet: the flag says "a path
// follows" and the 6 is where it starts, counted from the beginning of the
// packet. Reading it as a node id looked right -- a small number in a
// plausible place -- and the mistake only surfaced when this had to be
// *written*: a stock client logged `Failed to get path from RPC:
// ain/Level/...`, the path minus its first two characters, which is what
// reading from the wrong offset looks like from the other side.
//
// ## The method is a number, and the numbering is measured
//
// It is the index in *sorted* name order, not declaration order, and the md5
// in SIMPLIFY_PATH is what lets both ends check they agree -- see
// `rpc_config.dart`. Nothing in this demo sends arguments, so nothing follows
// the method byte but the path.

import 'dart:convert';
import 'dart:typed_data';

/// Lead byte of a call addressed by a confirmed path id.
const int leadCached = 0x80;

/// Lead byte of a call carrying its target's path.
const int leadPath = 0xa0;

/// Where the path starts in a long-form call with no arguments.
const int pathOffset = 6;

/// The long form's flag that the field below it is a path offset.
const int pathOffsetFlag = 0x80000000;

/// A remote call, in whichever form it crossed.
sealed class RemoteCall {
  const RemoteCall();

  /// Index into the node's sorted RPC method list.
  int get method;

  Uint8List encode();

  static RemoteCall parse(final Uint8List packet) {
    if (packet.isEmpty) {
      throw const FormatException('empty remote call');
    }
    switch (packet[0]) {
      case leadCached:
        if (packet.length < 3) {
          throw const FormatException('cached call is cut short');
        }
        return CachedCall(cacheId: packet[1], method: packet[2]);
      case leadPath:
        if (packet.length < pathOffset + 1) {
          throw const FormatException('path call is cut short');
        }
        final int field =
            ByteData.sublistView(packet).getUint32(1, Endian.little);
        if (field != (pathOffsetFlag | pathOffset)) {
          throw FormatException(
            'expected a path offset of ${pathOffsetFlag | pathOffset}, '
            'got $field',
          );
        }
        final int end = packet.indexOf(0, pathOffset);
        if (end < 0) {
          throw const FormatException('the path is not NUL-terminated');
        }
        return PathCall(
          method: packet[5],
          path: utf8.decode(packet.sublist(pathOffset, end)),
        );
      default:
        throw FormatException('unknown remote call form ${packet[0]}');
    }
  }
}

/// Addressed by a path id the receiver has confirmed.
class CachedCall extends RemoteCall {
  const CachedCall({required this.cacheId, required this.method});

  final int cacheId;

  @override
  final int method;

  @override
  Uint8List encode() =>
      Uint8List.fromList(<int>[leadCached, cacheId, method]);
}

/// Carrying its target's path, for a call that could not wait.
class PathCall extends RemoteCall {
  const PathCall({required this.method, required this.path});

  @override
  final int method;

  final String path;

  @override
  Uint8List encode() {
    final ByteData head = ByteData(pathOffset)
      ..setUint8(0, leadPath)
      ..setUint32(1, pathOffsetFlag | pathOffset, Endian.little)
      ..setUint8(5, method);
    return (BytesBuilder(copy: false)
          ..add(head.buffer.asUint8List())
          ..add(utf8.encode(path))
          ..addByte(0))
        .takeBytes();
  }
}
