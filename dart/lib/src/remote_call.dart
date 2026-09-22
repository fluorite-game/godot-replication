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

import 'sync.dart' show encodeCompactField;
import 'variant.dart';

/// Lead byte of a call addressed by a confirmed path id.
const int leadCached = 0x80;
/// Lead byte of a cached call that carries arguments.
const int leadCachedArgs = 0x00;
/// Lead byte of a path-addressed call that carries arguments.
const int leadPathArgs = 0x20;
/// The lead bit meaning the target travels as a path, not a cache id.
const int leadBitPath = 0x20;
/// The lead bit meaning no argument list follows the method id.
const int leadBitNoArgs = 0x80;

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

  /// The call's arguments, in order. Empty for the bare forms.
  List<VariantField> get args;

  static RemoteCall parse(final Uint8List packet) {
    if (packet.isEmpty) {
      throw const FormatException('empty remote call');
    }
    final int lead = packet[0];
    // Two flags over one command; anything outside them is a different packet.
    if (lead & ~(leadBitPath | leadBitNoArgs) != 0) {
      throw FormatException('unknown remote call form $lead');
    }
    final bool byPath = lead & leadBitPath != 0;
    final bool hasArgs = lead & leadBitNoArgs == 0;

    if (byPath) {
      if (packet.length < pathOffset + 1) {
        throw const FormatException('path call is cut short');
      }
      final int field =
          ByteData.sublistView(packet).getUint32(1, Endian.little);
      if (field & pathOffsetFlag == 0) {
        throw FormatException('path offset $field carries no flag');
      }
      final int offset = field & ~pathOffsetFlag;
      final (List<VariantField> args, int after) =
          hasArgs ? _readArgs(packet, 6) : (const <VariantField>[], 6);
      // The arguments have to end exactly where the path begins. Reading the
      // path from a disagreeing offset would turn that into a call made with
      // the wrong arguments, which is the failure this protocol is worst at
      // reporting.
      if (after != offset) {
        throw FormatException(
          'arguments end at $after but the path offset says $offset',
        );
      }
      if (offset >= packet.length) {
        throw const FormatException('the path starts past the packet');
      }
      final int end = packet.indexOf(0, offset);
      if (end < 0) {
        throw const FormatException('the path is not NUL-terminated');
      }
      return PathCall(
        method: packet[5],
        path: utf8.decode(packet.sublist(offset, end)),
        args: args,
      );
    }

    if (packet.length < 3) {
      throw const FormatException('cached call is cut short');
    }
    final (List<VariantField> args, int after) =
        hasArgs ? _readArgs(packet, 3) : (const <VariantField>[], 3);
    if (after != packet.length) {
      throw FormatException('${packet.length - after} bytes after the call');
    }
    return CachedCall(cacheId: packet[1], method: packet[2], args: args);
  }
}

/// Reads `[u8 count][value; count]`, returning the values and the offset past
/// them.
///
/// The values are in the compact form -- the same one SYNC uses, where a bool
/// or a small int gets a one-byte header and everything else keeps the plain
/// four-byte one.
(List<VariantField>, int) _readArgs(final Uint8List packet, final int at) {
  if (at >= packet.length) {
    throw const FormatException('no argument count');
  }
  final int count = packet[at];
  final List<VariantField> args = <VariantField>[];
  int cursor = at + 1;
  for (int i = 0; i < count; i++) {
    final DecodedVariant read = decodeCompactVariant(packet, cursor);
    args.add((type: read.type, value: read.value));
    cursor = read.next;
  }
  return (args, cursor);
}

/// Writes `[u8 count][value; count]`.
///
/// The count is one byte, which is the engine's own limit rather than a
/// simplification here: a call with more than 255 arguments cannot be
/// expressed in this framing at all.
Uint8List _writeArgs(final List<VariantField> args) {
  final BytesBuilder out = BytesBuilder(copy: false)..addByte(args.length);
  for (final VariantField arg in args) {
    out.add(encodeCompactField(arg.type, arg.value));
  }
  return out.takeBytes();
}

/// Addressed by a path id the receiver has confirmed.
class CachedCall extends RemoteCall {
  const CachedCall({
    required this.cacheId,
    required this.method,
    this.args = const <VariantField>[],
  });

  final int cacheId;

  @override
  final int method;

  @override
  final List<VariantField> args;

  @override
  Uint8List encode() {
    if (args.isEmpty) {
      return Uint8List.fromList(<int>[leadCached, cacheId, method]);
    }
    return (BytesBuilder(copy: false)
          ..add(<int>[leadCachedArgs, cacheId, method])
          ..add(_writeArgs(args)))
        .takeBytes();
  }
}

/// Carrying its target's path, for a call that could not wait.
class PathCall extends RemoteCall {
  const PathCall({
    required this.method,
    required this.path,
    this.args = const <VariantField>[],
  });

  @override
  final int method;

  final String path;

  @override
  final List<VariantField> args;

  @override
  Uint8List encode() {
    // The path sits after the arguments, which is why the offset is a field
    // and not a constant: it moves with them, and a 300-byte argument puts it
    // past anything a byte could address.
    final Uint8List body =
        args.isEmpty ? Uint8List(0) : _writeArgs(args);
    final int offset = pathOffset + body.length;
    final ByteData head = ByteData(pathOffset)
      ..setUint8(0, args.isEmpty ? leadPath : leadPathArgs)
      ..setUint32(1, pathOffsetFlag | offset, Endian.little)
      ..setUint8(5, method);
    return (BytesBuilder(copy: false)
          ..add(head.buffer.asUint8List())
          ..add(body)
          ..add(utf8.encode(path))
          ..addByte(0))
        .takeBytes();
  }
}
