// SPDX-License-Identifier: Apache-2.0
//
// The SYNC packet's framing (plan.md DR-7, DR-7a).
//
// ## Shape
//
//     06 | 0200 | 01000080 | 05000000 | 82 | 44332211
//     ^    ^      ^          ^          ^^^^^^^^^^^^^
//     |    |      |          |          the body: compact Variants
//     |    |      |          body length, u32 little-endian
//     |    |      synchronizer net id, u32 little-endian
//     |    a counter, incrementing once per packet
//     the SceneMultiplayer command, 0x06
//
// The `[net id][length][body]` record repeats: one per synchronizer with
// something to send. A real game packet carried up to eleven of them.
//
// ## The top bit of the net id
//
// Set means the record is addressed by *path cache id* rather than by the id a
// SPAWN handed out -- a synchronizer the far side has been told about with
// SIMPLIFY_PATH and has confirmed. In the capture those are the server's two
// BulletCache synchronizers and a client's InputSynchronizer, which is the one
// record that travels client to server.
//
// ## Two implementations, one corpus
//
// DR-7a keeps the port's codec in Dart and the extension's in Rust, and pins
// both to the same captured packets: `net/replication/tests/fixtures/`. The
// two were derived from different oracles -- this side from `var_to_bytes()`
// through `variant.dart`, that side from the packets themselves -- so where
// they agree, the agreement is evidence about Godot rather than about either
// implementation. Where they disagree, one of them is wrong and the fixture
// says which.

import 'dart:typed_data';

import 'variant.dart';

/// The `SceneMultiplayer` command byte for a sync packet.
const int commandSync = 0x06;

/// Bytes before the first record: the command and a `u16` counter.
const int syncHeaderLength = 3;

/// The bit that marks a net id as a path cache id.
const int syncPathFlag = 0x80000000;

/// One field, kept as the type it arrived as.
///
/// The type travels with the value because re-encoding needs it: an `int` that
/// arrived as a compact 2-byte field and a `float` that arrived as a plain
/// 8-byte one are both `num` in Dart, and writing them back at the wrong width
/// would produce a packet that is readable and different.
typedef SyncField = ({VariantType type, Object? value});

/// One synchronizer's worth of a sync packet.
class SyncRecord {
  /// [body] is the encoded form when there is one -- parsing keeps it, so a
  /// record that arrived can be applied without being written back out first.
  const SyncRecord({required this.netId, required this.fields, this.body});

  /// A record whose body has already been written.
  ///
  /// The sending path has the bytes and not a list of values: gameplay writes
  /// into an [EncodingSink] field by field, and decoding that back into
  /// [SyncField]s just to encode it again would be work done twice, every
  /// tick, for every synchronizer.
  const SyncRecord.encoded({required this.netId, required Uint8List this.body})
      : fields = const <SyncField>[];

  /// The synchronizer's network id. Its top bit is [syncPathFlag].
  final int netId;

  /// Its replicated fields, in the order its `SceneReplicationConfig` lists
  /// them -- mode-ALWAYS properties only. Empty on a record built from bytes.
  final List<SyncField> fields;

  /// The encoded body, when this record was built from one.
  final Uint8List? body;

  /// Whether this record is addressed by path cache id.
  bool get byPath => (netId & syncPathFlag) != 0;
}

/// A decoded sync packet.
class SyncPacket {
  const SyncPacket({required this.counter, required this.records});

  /// Increments once per sync packet sent.
  final int counter;

  final List<SyncRecord> records;
}

/// Reads one SYNC packet.
///
/// Throws rather than returning what it managed: a record whose length does not
/// land on a record boundary means the reading is wrong from that point on, and
/// every field after it would be plausible and false.
SyncPacket parseSync(final Uint8List packet) {
  if (packet.isEmpty || packet[0] != commandSync) {
    throw FormatException(
      'not a SYNC packet: ${packet.isEmpty ? "empty" : packet[0]}',
    );
  }
  if (packet.length < syncHeaderLength) {
    throw const FormatException('SYNC packet has no counter');
  }
  final ByteData view = ByteData.sublistView(packet);
  final int counter = view.getUint16(1, Endian.little);

  final List<SyncRecord> records = <SyncRecord>[];
  int at = syncHeaderLength;
  while (at < packet.length) {
    if (at + 8 > packet.length) {
      throw FormatException('record header at $at is cut short');
    }
    final int netId = view.getUint32(at, Endian.little);
    final int length = view.getUint32(at + 4, Endian.little);
    final int body = at + 8;
    if (body + length > packet.length) {
      throw FormatException(
        'record at $at claims $length bytes, ${packet.length - body} left',
      );
    }
    final int end = body + length;
    final List<SyncField> fields = <SyncField>[];
    int field = body;
    while (field < end) {
      final DecodedVariant read = decodeCompactVariant(packet, field);
      if (read.next > end) {
        throw FormatException(
          'field at $field runs past the end of its record',
        );
      }
      fields.add((type: read.type, value: read.value));
      field = read.next;
    }
    records.add(SyncRecord(
      netId: netId,
      fields: fields,
      body: Uint8List.sublistView(packet, body, end),
    ));
    at = end;
  }
  return SyncPacket(counter: counter, records: records);
}

/// Writes one SYNC packet.
Uint8List encodeSync(final SyncPacket packet) {
  final BytesBuilder out = BytesBuilder(copy: false);
  final ByteData header = ByteData(syncHeaderLength)
    ..setUint8(0, commandSync)
    ..setUint16(1, packet.counter, Endian.little);
  out.add(header.buffer.asUint8List());

  for (final SyncRecord record in packet.records) {
    Uint8List bytes = record.body ?? Uint8List(0);
    if (record.body == null) {
      final BytesBuilder body = BytesBuilder(copy: false);
      for (final SyncField field in record.fields) {
        body.add(encodeCompactField(field.type, field.value));
      }
      bytes = body.takeBytes();
    }
    final ByteData head = ByteData(8)
      ..setUint32(0, record.netId, Endian.little)
      ..setUint32(4, bytes.length, Endian.little);
    out
      ..add(head.buffer.asUint8List())
      ..add(bytes);
  }
  return out.takeBytes();
}

/// One field in the form SYNC carries it.
///
/// bool and int take the one-byte header; everything else falls back to the
/// four-byte one, which is what `encode_and_compress_variant` does.
Uint8List encodeCompactField(final VariantType type, final Object? value) {
  // bool and int are the only two the compact form covers; everything else
  // keeps the plain four-byte header, which is what
  // `encode_and_compress_variant` does.
  switch (type) {
    case VariantType.bool$:
      return encodeCompactBool(value! as bool);
    case VariantType.int$:
      return encodeCompactInt(value! as int);
    default:
      return encodeVariantValue(type, value);
  }
}


/// A [FieldSink] that writes the wire form.
///
/// The other half of the pull: gameplay writes typed values in config order,
/// and this turns them into the bytes a record carries. Nothing here decides
/// *which* fields -- the scene did that, and the caller walks them in order.
