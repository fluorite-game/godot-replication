// SPDX-License-Identifier: Apache-2.0
//
// Godot's binary Variant encoding (plan.md DR-7).
//
// Every byte layout here was read out of the engine by
// `tools/variant_wire_oracle.gd`, which calls `var_to_bytes()` -- the
// script-visible side of the same `encode_variant` the multiplayer protocol
// uses -- and the tests assert against those exact bytes. That matters more
// than usual here: a float one byte out of place decodes to a plausible number
// rather than to an error, so a codec checked against a reading of the format
// can be wrong for a long time without anything saying so.
//
// ## The shape
//
// A four-byte little-endian header carries the type in its low 16 bits and
// flags in its high 16, then the payload. Only one flag appears in this demo's
// data: bit 0, which widens an int or a float from 32 to 64 bits. Godot writes
// the narrow form whenever it is lossless -- `0.5` encodes as four bytes,
// `-6.3808` as eight -- so a decoder that assumes doubles reads the next
// field's bytes as the tail of this one.
//
// ## The six types
//
// These are all the demo replicates, across all five
// `SceneReplicationConfig` blocks: Transform3D for the three transforms,
// Vector3 for target_position, shoot_target, both camera rotations and both
// part velocities, Vector2 for motion, int for player_id, health and the two
// enums, bool for the four flags, and float for fade_value.

import 'dart:convert';
import 'dart:typed_data';

/// Godot's `Variant::Type` values, for the types this demo puts on the wire.
enum VariantType {
  nil(0),
  bool$(1),
  int$(2),
  float$(3),
  string(4),
  vector2(5),
  vector2i(6),
  rect2(7),
  rect2i(8),
  vector3(9),
  vector3i(10),
  transform2d(11),
  vector4(12),
  vector4i(13),
  plane(14),
  quaternion(15),
  aabb(16),
  basis(17),
  transform3d(18),
  projection(19),
  color(20),
  stringName(21),
  nodePath(22),
  dictionary(27),
  array(28),
  packedByteArray(29),
  packedInt32Array(30),
  packedInt64Array(31),
  packedFloat32Array(32),
  packedFloat64Array(33),
  packedStringArray(34),
  packedVector2Array(35),
  packedVector3Array(36),
  packedColorArray(37),
  packedVector4Array(38);

  const VariantType(this.id);
  final int id;

  /// How many `f32` a fixed-size float type carries, or null if it is not one.
  ///
  /// Most of Godot's geometry is a run of little-endian floats and nothing
  /// else, so the codec handles them as one case rather than eighteen.
  int? get floatCount => switch (this) {
        VariantType.vector2 => 2,
        VariantType.vector3 => 3,
        VariantType.vector4 ||
        VariantType.rect2 ||
        VariantType.plane ||
        VariantType.quaternion ||
        VariantType.color =>
          4,
        VariantType.transform2d || VariantType.aabb => 6,
        VariantType.basis => 9,
        VariantType.transform3d => 12,
        VariantType.projection => 16,
        _ => null,
      };

  /// How many `i32` a fixed-size integer-vector type carries, or null.
  int? get intCount => switch (this) {
        VariantType.vector2i => 2,
        VariantType.vector3i => 3,
        VariantType.vector4i || VariantType.rect2i => 4,
        _ => null,
      };

  /// How many `f32` one element of a packed vector array carries, or null.
  int? get packedVectorWidth => switch (this) {
        VariantType.packedVector2Array => 2,
        VariantType.packedVector3Array => 3,
        VariantType.packedColorArray || VariantType.packedVector4Array => 4,
        _ => null,
      };

  static VariantType? byId(final int id) {
    for (final VariantType type in VariantType.values) {
      if (type.id == id) return type;
    }
    return null;
  }
}

/// The header's bit 0: this int or float is 64-bit rather than 32.
const int variantFlag64 = 0x0001;

/// How many bytes each of the compact int width codes carries.
///
/// The code is bits 7-6 of the compact header byte. Measured by replicating
/// one int of each magnitude and reading the packet: 0 gave `02 00`, 300 gave
/// `42 2c01`, 0x11223344 gave `82 44332211`, and 2^33 gave `c2` plus eight
/// bytes. The 2-byte and 8-byte cases were *predicted* from the first two and
/// then captured, rather than being read off and rationalised.
const List<int> compactIntWidths = <int>[1, 2, 4, 8];

/// One decoded value and where the next one starts.
typedef DecodedVariant = ({VariantType type, Object? value, int next});

/// One value with the type it travelled as.
///
/// Containers hold these rather than bare values, because the Dart value alone
/// does not say which Variant type it was: a `Vector2`, a `Color` and a
/// `PackedFloat32Array` are all `List<double>` here, and inferring the type
/// from the list wrote an `Array` where the engine wrote a `Vector2`. The
/// fixture caught it; nothing else would have, because both are well-formed.
typedef VariantField = ({VariantType type, Object? value});

class _Writer {
  final BytesBuilder _out = BytesBuilder();

  void header(final VariantType type, {final int flags = 0}) {
    final ByteData head = ByteData(4)
      ..setUint32(0, type.id | (flags << 16), Endian.little);
    _out.add(head.buffer.asUint8List());
  }

  void u32(final int value) {
    final ByteData word = ByteData(4)..setUint32(0, value, Endian.little);
    _out.add(word.buffer.asUint8List());
  }

  void i32(final int value) {
    final ByteData word = ByteData(4)..setInt32(0, value, Endian.little);
    _out.add(word.buffer.asUint8List());
  }

  void i64(final int value) {
    final ByteData word = ByteData(8)..setInt64(0, value, Endian.little);
    _out.add(word.buffer.asUint8List());
  }

  void f32(final double value) {
    final ByteData word = ByteData(4)..setFloat32(0, value, Endian.little);
    _out.add(word.buffer.asUint8List());
  }

  void f64(final double value) {
    final ByteData word = ByteData(8)..setFloat64(0, value, Endian.little);
    _out.add(word.buffer.asUint8List());
  }

  Uint8List take() => _out.toBytes();
}

Uint8List encodeBool(final bool value) {
  final _Writer out = _Writer()..header(VariantType.bool$);
  out.u32(value ? 1 : 0);
  return out.take();
}

/// Narrow when it fits, as Godot does: a 64-bit int is the flagged form.
Uint8List encodeInt(final int value) {
  final bool wide = value < -2147483648 || value > 2147483647;
  final _Writer out = _Writer()
    ..header(VariantType.int$, flags: wide ? variantFlag64 : 0);
  if (wide) {
    out.i64(value);
  } else {
    out.i32(value);
  }
  return out.take();
}

/// Narrow when it round-trips, which is Godot's own rule.
///
/// `0.5` encodes in four bytes and `-6.3808` in eight, because the second does
/// not survive a trip through a 32-bit float and the first does.
Uint8List encodeDouble(final double value) {
  final ByteData probe = ByteData(4)..setFloat32(0, value, Endian.little);
  final bool lossless = probe.getFloat32(0, Endian.little) == value;
  final _Writer out = _Writer()
    ..header(VariantType.float$, flags: lossless ? 0 : variantFlag64);
  if (lossless) {
    out.f32(value);
  } else {
    out.f64(value);
  }
  return out.take();
}

Uint8List encodeVector2(final double x, final double y) {
  final _Writer out = _Writer()..header(VariantType.vector2);
  out.f32(x);
  out.f32(y);
  return out.take();
}

Uint8List encodeVector3(
  final double x,
  final double y,
  final double z,
) {
  final _Writer out = _Writer()..header(VariantType.vector3);
  out.f32(x);
  out.f32(y);
  out.f32(z);
  return out.take();
}

/// Nine basis floats then three of origin, all 32-bit.
///
/// The basis is row-major, which is worth stating because Godot's
/// `Basis(x_axis, y_axis, z_axis)` constructor takes *columns*: a basis built
/// from x-axis (0.843905, 0, -0.536493) encodes its first three floats as
/// 0.843905, 0, 0.536493 -- row zero, which is (x.x, y.x, z.x). Getting this
/// transposed produces a rotation that is wrong only about one axis, which
/// looks like a tuning problem rather than an encoding one.
Uint8List encodeTransform3D(
  final List<double> basisRowMajor,
  final List<double> origin,
) {
  if (basisRowMajor.length != 9 || origin.length != 3) {
    throw ArgumentError('a Transform3D is nine basis floats and three origin');
  }
  final _Writer out = _Writer()..header(VariantType.transform3d);
  for (final double value in basisRowMajor) {
    out.f32(value);
  }
  for (final double value in origin) {
    out.f32(value);
  }
  return out.take();
}

/// Reads one Variant at [offset].
///
/// Throws on a type it does not know rather than skipping: an unknown type has
/// an unknown length, so there is no way to find the next field, and guessing
/// turns one unreadable value into an unreadable rest-of-packet.
DecodedVariant decodeVariant(final Uint8List bytes, [final int offset = 0]) {
  final ByteData view = ByteData.sublistView(bytes);
  if (offset + 4 > bytes.length) {
    throw FormatException('no Variant header at $offset');
  }
  final int head = view.getUint32(offset, Endian.little);
  final int id = head & 0xFFFF;
  final int flags = head >> 16;
  final VariantType? type = VariantType.byId(id);
  if (type == null) {
    throw FormatException('unknown Variant type $id at $offset');
  }
  final int at = offset + 4;
  final bool wide = (flags & variantFlag64) != 0;

  int need(final int n) {
    if (at + n > bytes.length) {
      throw FormatException('Variant ${type.name} at $offset is cut short');
    }
    return at + n;
  }

  // Everything whose body is a run of f32, or of i32, is one case rather than
  // eighteen that differ only in a length.
  final int? floats = type.floatCount;
  if (floats != null) {
    final int end = need(floats * 4);
    return (
      type: type,
      value: <double>[
        for (int i = 0; i < floats; i++)
          view.getFloat32(at + i * 4, Endian.little),
      ],
      next: end,
    );
  }
  final int? ints = type.intCount;
  if (ints != null) {
    final int end = need(ints * 4);
    return (
      type: type,
      value: <int>[
        for (int i = 0; i < ints; i++) view.getInt32(at + i * 4, Endian.little),
      ],
      next: end,
    );
  }

  switch (type) {
    case VariantType.nil:
      return (type: type, value: null, next: at);
    // Handled by the floatCount and intCount paths above, and listed so the
    // switch stays exhaustive without a default. A default would have let the
    // next type added to the enum fall through silently, which is how a
    // decoder starts returning plausible wrong answers.
    case VariantType.vector2i:
    case VariantType.rect2:
    case VariantType.rect2i:
    case VariantType.vector3i:
    case VariantType.transform2d:
    case VariantType.vector4:
    case VariantType.vector4i:
    case VariantType.plane:
    case VariantType.quaternion:
    case VariantType.aabb:
    case VariantType.basis:
    case VariantType.projection:
    case VariantType.color:
      throw StateError('${type.name} should have been read as a run of scalars');
    case VariantType.string:
    case VariantType.stringName:
      final (String text, int next) = _readString(bytes, view, at, offset);
      return (type: type, value: text, next: next);
    case VariantType.nodePath:
      return _readNodePath(bytes, view, at, offset, type);
    case VariantType.dictionary:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final List<(VariantField, VariantField)> pairs =
          <(VariantField, VariantField)>[];
      int cursor = at + 4;
      for (int i = 0; i < count; i++) {
        final DecodedVariant key = decodeVariant(bytes, cursor);
        final DecodedVariant value = decodeVariant(bytes, key.next);
        pairs.add((
          (type: key.type, value: key.value),
          (type: value.type, value: value.value),
        ));
        cursor = value.next;
      }
      return (type: type, value: pairs, next: cursor);
    case VariantType.array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final List<VariantField> items = <VariantField>[];
      int cursor = at + 4;
      for (int i = 0; i < count; i++) {
        final DecodedVariant item = decodeVariant(bytes, cursor);
        items.add((type: item.type, value: item.value));
        cursor = item.next;
      }
      return (type: type, value: items, next: cursor);
    case VariantType.packedByteArray:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int end = need(4 + count);
      // Padded to four, and the padding is not necessarily zero -- see
      // _readString.
      final int pad = (4 - (count % 4)) % 4;
      return (
        type: type,
        value: Uint8List.sublistView(bytes, at + 4, end),
        next: end + pad,
      );
    case VariantType.packedInt32Array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int end = need(4 + count * 4);
      return (
        type: type,
        value: <int>[
          for (int i = 0; i < count; i++)
            view.getInt32(at + 4 + i * 4, Endian.little),
        ],
        next: end,
      );
    case VariantType.packedInt64Array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int end = need(4 + count * 8);
      return (
        type: type,
        value: <int>[
          for (int i = 0; i < count; i++)
            view.getInt64(at + 4 + i * 8, Endian.little),
        ],
        next: end,
      );
    case VariantType.packedFloat32Array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int end = need(4 + count * 4);
      return (
        type: type,
        value: <double>[
          for (int i = 0; i < count; i++)
            view.getFloat32(at + 4 + i * 4, Endian.little),
        ],
        next: end,
      );
    case VariantType.packedFloat64Array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int end = need(4 + count * 8);
      return (
        type: type,
        value: <double>[
          for (int i = 0; i < count; i++)
            view.getFloat64(at + 4 + i * 8, Endian.little),
        ],
        next: end,
      );
    case VariantType.packedStringArray:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final List<String> texts = <String>[];
      int cursor = at + 4;
      for (int i = 0; i < count; i++) {
        final (String text, int next) =
            _readString(bytes, view, cursor, offset);
        texts.add(text);
        cursor = next;
      }
      return (type: type, value: texts, next: cursor);
    case VariantType.packedVector2Array:
    case VariantType.packedVector3Array:
    case VariantType.packedColorArray:
    case VariantType.packedVector4Array:
      need(4);
      final int count = view.getUint32(at, Endian.little);
      final int width = type.packedVectorWidth!;
      final int end = need(4 + count * width * 4);
      return (
        type: type,
        value: <double>[
          for (int i = 0; i < count * width; i++)
            view.getFloat32(at + 4 + i * 4, Endian.little),
        ],
        next: end,
      );
    case VariantType.bool$:
      return (
        type: type,
        value: view.getUint32(at, Endian.little) != 0,
        next: need(4),
      );
    case VariantType.int$:
      return wide
          ? (type: type, value: view.getInt64(at, Endian.little), next: need(8))
          : (type: type, value: view.getInt32(at, Endian.little), next: need(4));
    case VariantType.float$:
      return wide
          ? (
              type: type,
              value: view.getFloat64(at, Endian.little),
              next: need(8),
            )
          : (
              type: type,
              value: view.getFloat32(at, Endian.little),
              next: need(4),
            );
    case VariantType.vector2:
      // Unreachable: floatCount handled it above. Kept so the switch stays
      // exhaustive without a default, which is what made adding types safe.
      final int end = need(8);
      return (
        type: type,
        value: <double>[
          view.getFloat32(at, Endian.little),
          view.getFloat32(at + 4, Endian.little),
        ],
        next: end,
      );
    case VariantType.vector3:
      final int end = need(12);
      return (
        type: type,
        value: <double>[
          for (int i = 0; i < 3; i++)
            view.getFloat32(at + i * 4, Endian.little),
        ],
        next: end,
      );
    case VariantType.transform3d:
      final int end = need(48);
      return (
        type: type,
        value: <double>[
          for (int i = 0; i < 12; i++)
            view.getFloat32(at + i * 4, Endian.little),
        ],
        next: end,
      );
  }
}

// ---------------------------------------------------------------------------
// The compact form, which is what SYNC packets actually carry.
//
// `encode_and_compress_variant` is a distinct symbol from `encode_variant`, and
// the difference is real: bool and int get a *one-byte* header, everything else
// falls back to the four-byte form above. Assuming the four-byte header
// everywhere is what defeated two earlier attempts to parse a SYNC packet --
// the framing around it had been right the whole time.
//
// Measured with `tools/capture_sync_probe.sh`, one property at a time:
//
//     bool false            01
//     bool true             81            bit 7 is the value
//     int 0                 02 00         bits 7-6 are a width code
//     int 5                 02 05
//     int 300               42 2c01
//     int 0x11223344        82 44332211
//     int 2^33              c2 0000000002000000
//     float -6.3808         03000100 6744696ff08519c0    plain header
//     Vector2 (1, -1)       05000000 0000803f 000080bf   plain header
//
// Then validated where it counts: all 12814 SYNC packets in a real two-peer
// game capture parse to the exact byte under this rule, and the field shapes
// that fall out match the `.tscn` property lists -- including the mode-0
// fields being absent from the stream.

/// The smallest width code that holds [value].
int _compactWidthCode(final int value) {
  if (value >= -128 && value <= 127) return 0;
  if (value >= -32768 && value <= 32767) return 1;
  if (value >= -2147483648 && value <= 2147483647) return 2;
  return 3;
}

/// A bool as SYNC carries it: one byte, with the value in bit 7.
Uint8List encodeCompactBool(final bool value) =>
    Uint8List.fromList(<int>[VariantType.bool$.id | (value ? 0x80 : 0)]);

/// An int as SYNC carries it: a header byte then 1, 2, 4 or 8 bytes.
///
/// Godot writes the narrowest that fits, so a decoder must read the code
/// rather than assume a width -- guessing wide eats the next field.
Uint8List encodeCompactInt(final int value) {
  final int code = _compactWidthCode(value);
  final int width = compactIntWidths[code];
  final ByteData out = ByteData(1 + width)
    ..setUint8(0, VariantType.int$.id | (code << 6));
  switch (width) {
    case 1:
      out.setInt8(1, value);
    case 2:
      out.setInt16(1, value, Endian.little);
    case 4:
      out.setInt32(1, value, Endian.little);
    default:
      out.setInt64(1, value, Endian.little);
  }
  return out.buffer.asUint8List();
}

/// Reads one value in the compact form, falling back to the plain one.
///
/// The discriminator is the low six bits of the first byte: bool and int are
/// compact, and every other type writes the four-byte header, so those are
/// handed to [decodeVariant] unchanged.
DecodedVariant decodeCompactVariant(
  final Uint8List bytes, [
  final int offset = 0,
]) {
  if (offset >= bytes.length) {
    throw FormatException('no Variant at $offset');
  }
  final ByteData view = ByteData.sublistView(bytes);
  final int lead = bytes[offset];
  final int type = lead & 0x3F;

  if (type == VariantType.bool$.id) {
    return (
      type: VariantType.bool$,
      value: (lead & 0x80) != 0,
      next: offset + 1,
    );
  }
  if (type == VariantType.int$.id) {
    final int width = compactIntWidths[lead >> 6];
    if (offset + 1 + width > bytes.length) {
      throw FormatException('compact int at $offset is cut short');
    }
    final int at = offset + 1;
    final int value = switch (width) {
      1 => view.getInt8(at),
      2 => view.getInt16(at, Endian.little),
      4 => view.getInt32(at, Endian.little),
      _ => view.getInt64(at, Endian.little),
    };
    return (type: VariantType.int$, value: value, next: at + width);
  }
  return decodeVariant(bytes, offset);
}

/// A `NodePath`: names, subnames, and whether it is rooted.
///
/// `Player:position:x` is one name and two subnames; `/root/Level` is two
/// names, absolute. Carried as its parts rather than as text, because the
/// parts are what came off the wire.
class NodePathValue {
  const NodePathValue({
    required this.names,
    required this.subnames,
    required this.absolute,
  });

  final List<String> names;
  final List<String> subnames;
  final bool absolute;

  /// The text form Godot itself parses.
  String get text {
    final StringBuffer out = StringBuffer(absolute ? '/' : '')
      ..write(names.join('/'));
    for (final String sub in subnames) {
      out.write(':$sub');
    }
    return out.toString();
  }

  @override
  bool operator ==(final Object other) =>
      other is NodePathValue &&
      other.absolute == absolute &&
      _sameStrings(other.names, names) &&
      _sameStrings(other.subnames, subnames);

  @override
  int get hashCode => Object.hash(absolute, names.join('/'), subnames.join(':'));

  @override
  String toString() => 'NodePath($text)';
}

bool _sameStrings(final List<String> a, final List<String> b) {
  if (a.length != b.length) return false;
  for (int i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// Reads a length-prefixed UTF-8 string and returns it with the offset past
/// its padding.
///
/// The length is in *bytes* and the text is padded to a four-byte boundary.
/// Two things a careless reader gets wrong: counting characters rather than
/// bytes, which survives every ASCII sample and dies on the first multi-byte
/// one; and assuming the padding is zero. It is not -- a captured NodePath
/// pads `"Level"` with the bytes `30 30 30`, whatever the engine's buffer
/// happened to hold -- so the padding is skipped rather than checked.
(String, int) _readString(
  final Uint8List bytes,
  final ByteData view,
  final int at,
  final int offset,
) {
  if (at + 4 > bytes.length) {
    throw FormatException('no string length at $at (value began $offset)');
  }
  final int length = view.getUint32(at, Endian.little);
  final int end = at + 4 + length;
  if (end > bytes.length) {
    throw FormatException('string at $at runs past the packet');
  }
  final int pad = (4 - (length % 4)) % 4;
  return (utf8.decode(bytes.sublist(at + 4, end), allowMalformed: true), end + pad);
}

/// Reads a `NodePath`: two counts, a flag, then the names and the subnames.
///
/// The name count carries `0x8000_0000`, which is how the engine marks the
/// layout it has written since 4.0. Without that bit it would be reading the
/// older single-string form and finding nonsense.
DecodedVariant _readNodePath(
  final Uint8List bytes,
  final ByteData view,
  final int at,
  final int offset,
  final VariantType type,
) {
  if (at + 12 > bytes.length) {
    throw FormatException('NodePath at $offset is cut short');
  }
  final int raw = view.getUint32(at, Endian.little);
  final int subCount = view.getUint32(at + 4, Endian.little);
  final bool absolute = view.getUint32(at + 8, Endian.little) != 0;
  final int nameCount = raw & 0x7FFFFFFF;
  final List<String> names = <String>[];
  final List<String> subnames = <String>[];
  int cursor = at + 12;
  for (int i = 0; i < nameCount; i++) {
    final (String text, int next) = _readString(bytes, view, cursor, offset);
    names.add(text);
    cursor = next;
  }
  for (int i = 0; i < subCount; i++) {
    final (String text, int next) = _readString(bytes, view, cursor, offset);
    subnames.add(text);
    cursor = next;
  }
  return (
    type: type,
    value: NodePathValue(
      names: names,
      subnames: subnames,
      absolute: absolute,
    ),
    next: cursor,
  );
}

/// Encodes any value in the plain four-byte-header form.
///
/// The counterpart to [decodeVariant], and deliberately one function rather
/// than one per type: the geometry types differ only in how many floats
/// follow, and writing eighteen near-identical encoders is how one of them
/// ends up with the wrong count.
///
/// Padding is written as zeroes. The engine does not -- it pads from whatever
/// its buffer held -- so a packet re-encoded from a capture can differ from
/// the original in its padding bytes while carrying the same values. That is a
/// property of the format, not a bug here, and the tests compare decoded
/// values rather than bytes where it applies.
Uint8List encodeVariantValue(final VariantType type, final Object? value) {
  final BytesBuilder out = BytesBuilder(copy: false);
  void header([final int flags = 0]) {
    final ByteData head = ByteData(4)
      ..setUint32(0, type.id | (flags << 16), Endian.little);
    out.add(head.buffer.asUint8List());
  }

  void u32(final int v) {
    final ByteData b = ByteData(4)..setUint32(0, v, Endian.little);
    out.add(b.buffer.asUint8List());
  }

  void f32(final double v) {
    final ByteData b = ByteData(4)..setFloat32(0, v, Endian.little);
    out.add(b.buffer.asUint8List());
  }

  void str(final String text) {
    final List<int> bytes = utf8.encode(text);
    u32(bytes.length);
    out.add(bytes);
    out.add(Uint8List((4 - (bytes.length % 4)) % 4));
  }

  final int? floats = type.floatCount;
  if (floats != null) {
    header();
    for (final double v in (value! as List<double>).take(floats)) {
      f32(v);
    }
    return out.toBytes();
  }
  final int? ints = type.intCount;
  if (ints != null) {
    header();
    for (final int v in (value! as List<int>).take(ints)) {
      final ByteData b = ByteData(4)..setInt32(0, v, Endian.little);
      out.add(b.buffer.asUint8List());
    }
    return out.toBytes();
  }

  switch (type) {
    case VariantType.nil:
      header();
    case VariantType.bool$:
      header();
      u32((value! as bool) ? 1 : 0);
    case VariantType.int$:
      return encodeInt(value! as int);
    case VariantType.float$:
      return encodeDouble(value! as double);
    case VariantType.string:
    case VariantType.stringName:
      header();
      str(value! as String);
    case VariantType.nodePath:
      final NodePathValue path = value! as NodePathValue;
      header();
      // The high bit marks the post-4.0 layout; without it the engine reads
      // the older single-string form.
      u32(path.names.length | 0x80000000);
      u32(path.subnames.length);
      u32(path.absolute ? 1 : 0);
      for (final String name in <String>[...path.names, ...path.subnames]) {
        str(name);
      }
    case VariantType.dictionary:
      final List<(VariantField, VariantField)> pairs =
          value! as List<(VariantField, VariantField)>;
      header();
      u32(pairs.length);
      for (final (VariantField k, VariantField v) in pairs) {
        out.add(encodeVariantValue(k.type, k.value));
        out.add(encodeVariantValue(v.type, v.value));
      }
    case VariantType.array:
      final List<VariantField> items = value! as List<VariantField>;
      header();
      u32(items.length);
      for (final VariantField item in items) {
        out.add(encodeVariantValue(item.type, item.value));
      }
    case VariantType.packedByteArray:
      final List<int> bytes = value! as List<int>;
      header();
      u32(bytes.length);
      out.add(bytes);
      out.add(Uint8List((4 - (bytes.length % 4)) % 4));
    case VariantType.packedInt32Array:
      final List<int> items = value! as List<int>;
      header();
      u32(items.length);
      for (final int v in items) {
        final ByteData b = ByteData(4)..setInt32(0, v, Endian.little);
        out.add(b.buffer.asUint8List());
      }
    case VariantType.packedInt64Array:
      final List<int> items = value! as List<int>;
      header();
      u32(items.length);
      for (final int v in items) {
        final ByteData b = ByteData(8)..setInt64(0, v, Endian.little);
        out.add(b.buffer.asUint8List());
      }
    case VariantType.packedFloat32Array:
      final List<double> items = value! as List<double>;
      header();
      u32(items.length);
      for (final double v in items) {
        f32(v);
      }
    case VariantType.packedFloat64Array:
      final List<double> items = value! as List<double>;
      header();
      u32(items.length);
      for (final double v in items) {
        final ByteData b = ByteData(8)..setFloat64(0, v, Endian.little);
        out.add(b.buffer.asUint8List());
      }
    case VariantType.packedStringArray:
      final List<String> items = value! as List<String>;
      header();
      u32(items.length);
      for (final String text in items) {
        str(text);
      }
    case VariantType.packedVector2Array:
    case VariantType.packedVector3Array:
    case VariantType.packedColorArray:
    case VariantType.packedVector4Array:
      final List<double> items = value! as List<double>;
      header();
      u32(items.length ~/ type.packedVectorWidth!);
      for (final double v in items) {
        f32(v);
      }
    // Handled by the scalar-run paths above.
    case VariantType.vector2:
    case VariantType.vector2i:
    case VariantType.rect2:
    case VariantType.rect2i:
    case VariantType.vector3:
    case VariantType.vector3i:
    case VariantType.transform2d:
    case VariantType.vector4:
    case VariantType.vector4i:
    case VariantType.plane:
    case VariantType.quaternion:
    case VariantType.aabb:
    case VariantType.basis:
    case VariantType.transform3d:
    case VariantType.projection:
    case VariantType.color:
      throw StateError('${type.name} should have been written as scalars');
  }
  return out.toBytes();
}
