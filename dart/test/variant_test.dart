// SPDX-License-Identifier: Apache-2.0
//
// The Variant codec against bytes the engine produced.
//
// Every expected string here is `var_to_bytes()` output printed by
// `tools/variant_wire_oracle.gd` under Godot 4.5.2-stable. They are quoted
// rather than computed, so a change in the port cannot quietly change what it
// is being held to.

import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:godot_replication/src/variant.dart';

String _hex(final Uint8List bytes) =>
    bytes.map((final int b) => b.toRadixString(16).padLeft(2, '0')).join();

Uint8List _bytes(final String hex) => Uint8List.fromList(<int>[
      for (int i = 0; i < hex.length; i += 2)
        int.parse(hex.substring(i, i + 2), radix: 16),
    ]);

void main() {
  test('bools and ints match the engine byte for byte', () {
    expect(_hex(encodeBool(false)), '0100000000000000');
    expect(_hex(encodeBool(true)), '0100000001000000');
    expect(_hex(encodeInt(0)), '0200000000000000');
    expect(_hex(encodeInt(5)), '0200000005000000');
    expect(_hex(encodeInt(-1)), '02000000ffffffff');
    // The two enums cross as plain ints: Animations.WALK and State.APPROACH.
    expect(_hex(encodeInt(3)), '0200000003000000');
    expect(_hex(encodeInt(1)), '0200000001000000');
  });

  test('an int too big for 32 bits sets the width flag and grows', () {
    // Godot writes the narrow form whenever it fits, so the flag is not a
    // style choice -- a decoder that always reads eight bytes consumes the
    // next field's header as this value's tail.
    expect(_hex(encodeInt(8589934592)), '020001000000000002000000');
    expect(encodeInt(2147483647).length, 8);
    expect(encodeInt(2147483648).length, 12);
  });

  test('a float is 32-bit when that is lossless and 64-bit when it is not', () {
    expect(_hex(encodeDouble(0.0)), '0300000000000000');
    expect(_hex(encodeDouble(0.5)), '030000000000003f');
    // The robot's floor height, which does not survive a 32-bit round trip.
    expect(_hex(encodeDouble(-6.3808)), '030001006744696ff08519c0');
  });

  test('vectors match the engine', () {
    expect(_hex(encodeVector2(0, 0)), '050000000000000000000000');
    expect(_hex(encodeVector2(1.0, -1.0)), '050000000000803f000080bf');
    expect(_hex(encodeVector3(0, 0, 0)), '09000000000000000000000000000000');
    expect(_hex(encodeVector3(71.5907, -6.3808, 46.2736)),
        '09000000702e8f42832fccc02b183942');
  });

  test('a Transform3D is nine basis floats row-major, then three of origin', () {
    expect(
      _hex(encodeTransform3D(
        <double>[1, 0, 0, 0, 1, 0, 0, 0, 1],
        <double>[0, 0, 0],
      )),
      '120000000000803f000000000000000000000000'
      '0000803f0000000000000000000000000000803f'
      '000000000000000000000000',
    );

    // Marker3D1's basis, the one a robot spawns on. Godot's
    // `Basis(x_axis, y_axis, z_axis)` takes *columns*, so an x-axis of
    // (0.843905, 0, -0.536493) encodes its first three floats as
    // (0.843905, 0, 0.536493) -- row zero, which is (x.x, y.x, z.x). A
    // transposed encoder is wrong about one axis only, which reads as a tuning
    // problem rather than an encoding one.
    expect(
      _hex(encodeTransform3D(
        <double>[0.843905, 0, 0.536493, 0, 1, 0, -0.536493, 0, 0.843905],
        <double>[71.5907, -6.3808, 46.2736],
      )),
      '12000000280a583f000000009b57093f000000000000803f00000000'
      '9b5709bf00000000280a583f702e8f42832fccc02b183942',
    );
  });

  test('decoding returns what the engine encoded, and where the next one is',
      () {
    final DecodedVariant health = decodeVariant(_bytes('0200000005000000'));
    expect(health.type, VariantType.int$);
    expect(health.value, 5);
    expect(health.next, 8);

    final DecodedVariant floor =
        decodeVariant(_bytes('030001006744696ff08519c0'));
    expect(floor.type, VariantType.float$);
    expect(floor.value, closeTo(-6.3808, 1e-12));
    expect(floor.next, 12);

    final DecodedVariant marker =
        decodeVariant(_bytes('09000000702e8f42832fccc02b183942'));
    expect(marker.type, VariantType.vector3);
    final List<double> at = marker.value as List<double>;
    expect(at[0], closeTo(71.5907, 1e-4));
    expect(at[1], closeTo(-6.3808, 1e-4));
    expect(at[2], closeTo(46.2736, 1e-4));
  });

  test('two values back to back are found by following next', () {
    // Which is the whole job inside a packet: nothing delimits one field from
    // the following one except this codec getting the length right.
    final Uint8List pair = Uint8List.fromList(<int>[
      ...encodeInt(5),
      ...encodeBool(true),
    ]);
    final DecodedVariant first = decodeVariant(pair);
    expect(first.value, 5);
    final DecodedVariant second = decodeVariant(pair, first.next);
    expect(second.value, true);
    expect(second.next, pair.length);
  });

  test('an unknown type is refused rather than skipped', () {
    // An unknown type has an unknown length, so there is no next field to find
    // -- guessing turns one unreadable value into an unreadable packet.
    expect(() => decodeVariant(_bytes('ff00000000000000')),
        throwsA(isA<FormatException>()));
    expect(() => decodeVariant(_bytes('0200')), throwsA(isA<FormatException>()));
  });

  group('the compact form, which is what SYNC carries', () {
    // Every expected string is a body read out of a capture taken by
    // `tools/capture_sync_probe.sh`, one property at a time, with the value
    // chosen to be unmistakable in a hex dump.

    test('a bool is one byte with the value in bit 7', () {
      expect(_hex(encodeCompactBool(false)), '01');
      expect(_hex(encodeCompactBool(true)), '81');
      expect(decodeCompactVariant(_bytes('01')).value, false);
      expect(decodeCompactVariant(_bytes('81')).value, true);
      expect(decodeCompactVariant(_bytes('81')).next, 1);
    });

    test('an int carries a width code in bits 7 and 6', () {
      // The first two were read off captures; the other two were predicted
      // from them and then captured, which is why this reads as a table
      // rather than as a discovery.
      expect(_hex(encodeCompactInt(0)), '0200');
      expect(_hex(encodeCompactInt(5)), '0205');
      expect(_hex(encodeCompactInt(300)), '422c01');
      expect(_hex(encodeCompactInt(287454020)), '8244332211');
      expect(_hex(encodeCompactInt(8589934592)), 'c20000000002000000');
    });

    test('decoding an int reads the width rather than assuming one', () {
      // Assuming the widest eats the next field; assuming the narrowest
      // truncates this one. Both produce a plausible number.
      expect(decodeCompactVariant(_bytes('0205')).value, 5);
      expect(decodeCompactVariant(_bytes('0205')).next, 2);
      expect(decodeCompactVariant(_bytes('422c01')).value, 300);
      expect(decodeCompactVariant(_bytes('422c01')).next, 3);
      expect(decodeCompactVariant(_bytes('8244332211')).value, 287454020);
      expect(decodeCompactVariant(_bytes('c20000000002000000')).value,
          8589934592);
    });

    test('every other type falls back to the four-byte header', () {
      // Measured: a float on the wire is `03000100...`, which is exactly what
      // var_to_bytes produces. Only bool and int are compacted.
      final DecodedVariant floor =
          decodeCompactVariant(_bytes('030001006744696ff08519c0'));
      expect(floor.type, VariantType.float$);
      expect(floor.value, closeTo(-6.3808, 1e-12));
      expect(floor.next, 12);

      final DecodedVariant stick =
          decodeCompactVariant(_bytes('050000000000803f000080bf'));
      expect(stick.type, VariantType.vector2);
      expect((stick.value as List<double>)[1], -1.0);
    });

    test('a robot record decodes as its scene says it should', () {
      // The shape the wire actually carries for a red robot, 25640 times in
      // one capture: global_transform, state, target_position. `health` and
      // `dead` are mode 0 and are absent from the stream -- which is what
      // net/replication_table.dart said before any of this was decoded.
      final Uint8List record = Uint8List.fromList(<int>[
        ...encodeTransform3D(
          <double>[1, 0, 0, 0, 1, 0, 0, 0, 1],
          <double>[71.5907, -6.3808, 46.2736],
        ),
        ...encodeCompactInt(1), // State.APPROACH
        ...encodeVector3(64.0, -1.2, 78.0),
      ]);
      final List<VariantType> seen = <VariantType>[];
      int at = 0;
      while (at < record.length) {
        final DecodedVariant one = decodeCompactVariant(record, at);
        seen.add(one.type);
        at = one.next;
      }
      expect(at, record.length, reason: 'lands on the end, as the wire does');
      expect(seen, <VariantType>[
        VariantType.transform3d,
        VariantType.int$,
        VariantType.vector3,
      ]);
    });
  });
}
