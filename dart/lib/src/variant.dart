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

import 'dart:typed_data';

/// Godot's `Variant::Type` values, for the types this demo puts on the wire.
enum VariantType {
  bool$(1),
  int$(2),
  float$(3),
  vector2(5),
  vector3(9),
  transform3d(18);

  const VariantType(this.id);
  final int id;

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
typedef DecodedVariant = ({VariantType type, Object value, int next});

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

  switch (type) {
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
