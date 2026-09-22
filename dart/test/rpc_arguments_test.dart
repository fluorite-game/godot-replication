// SPDX-License-Identifier: Apache-2.0
//
// RPC calls that carry arguments, against bytes a stock engine produced.
//
// The same fixture the Rust suite asserts against: the output of
// `oracle/rpc_oracle.gd`, which installs a `MultiplayerPeerExtension` under a
// stock `SceneMultiplayer` so the engine encodes each call exactly as it would
// for a socket and hands the bytes over instead of sending them.
//
// No captured packet carries an argument list -- the game this protocol was
// measured from never calls an RPC with parameters -- so without the oracle
// every assertion here would be checking the implementation against itself.

import 'dart:io';
import 'dart:typed_data';

import 'package:godot_replication/src/remote_call.dart';
import 'package:godot_replication/src/variant.dart';
import 'package:test/test.dart';

typedef Sample = ({String label, Uint8List bytes});

/// The fixture opens with the SIMPLIFY_PATH the engine sent before it could
/// address anything, which is a path packet and not a call.
List<Sample> calls() {
  final File file = File('../replication/tests/fixtures/rpc_arguments.hex');
  if (!file.existsSync()) {
    throw StateError('no fixture at ${file.path}');
  }
  return <Sample>[
    for (final String line in file.readAsLinesSync())
      if (!line.startsWith('#') && line.trim().isNotEmpty)
        () {
          final List<String> parts = line.split('\t');
          final String hex = parts[1];
          return (
            label: parts[0],
            bytes: Uint8List.fromList(<int>[
              for (int i = 0; i + 1 < hex.length; i += 2)
                int.parse(hex.substring(i, i + 2), radix: 16),
            ]),
          );
        }(),
  ].where((final Sample s) => s.bytes.first != 0x01).toList();
}

RemoteCall byLabel(final String label) => RemoteCall.parse(
      calls().firstWhere((final Sample s) => s.label == label).bytes,
    );

void main() {
  test('every call the engine encoded parses', () {
    for (final Sample s in calls()) {
      expect(() => RemoteCall.parse(s.bytes), returnsNormally, reason: s.label);
    }
  });

  test('re-encoding reproduces the engine bytes', () {
    for (final Sample s in calls()) {
      expect(RemoteCall.parse(s.bytes).encode(), s.bytes, reason: s.label);
    }
  });

  test('the lead byte is two flags over one command', () {
    // 0x20 says the target travels as a path, 0x80 says there are no
    // arguments. Asserted against the fixture rather than against the
    // constants, because the engine chose these and this package did not.
    for (final Sample s in calls()) {
      final int lead = s.bytes[0];
      final RemoteCall call = RemoteCall.parse(s.bytes);
      expect(call is PathCall, lead & 0x20 != 0, reason: '${s.label}: path');
      expect(call.args.isEmpty, lead & 0x80 != 0, reason: '${s.label}: args');
    }
  });

  test("the long form's offset is where the path actually starts", () {
    // The field exists because the path sits after the arguments, so its value
    // moves with them. A decoder keeping the no-argument constant would read
    // the path out of the middle of an argument.
    for (final Sample s in calls()) {
      if (s.bytes[0] & 0x20 == 0) continue;
      final int field =
          ByteData.sublistView(s.bytes).getUint32(1, Endian.little);
      final int offset = field & 0x7fffffff;
      final int end = s.bytes.indexOf(0, offset);
      expect(
        String.fromCharCodes(s.bytes.sublist(offset, end)),
        'RpcOracle',
        reason: '${s.label}: path at $offset',
      );
    }
  });

  test('the arguments are the values that were passed', () {
    expect(byLabel('no_args').args, isEmpty);
    expect(byLabel('one_int 0').args.single.value, 0);
    expect(byLabel('one_int -1').args.single.value, -1);
    // Wide enough that the compact form has to widen with it.
    expect(byLabel('one_int 2^33').args.single.value, 8589934592);
    expect(byLabel('one_string ascii').args.single.value, 'shoot');
    expect(
      byLabel('two_vectors').args.map((final VariantField a) => a.value),
      <List<double>>[
        <double>[1.0, -2.0, 3.0],
        <double>[-4.0, 5.0, -6.0],
      ],
    );

    final List<VariantField> mixed = byLabel('three_mixed').args;
    expect(mixed.map((final VariantField a) => a.type), <VariantType>[
      VariantType.int$,
      VariantType.string,
      VariantType.bool$,
    ]);
    expect(mixed.map((final VariantField a) => a.value), <Object>[5, 'hit', true]);

    // The same call once the receiver confirmed the path cache: a different
    // lead byte and no path, and the arguments have to survive that.
    expect(
      byLabel('cached three_mixed').args.map((final VariantField a) => a.value),
      mixed.map((final VariantField a) => a.value),
    );
  });

  test('a three hundred byte argument needs the offset to be a word', () {
    // The reason the offset is four bytes. One argument here is longer than a
    // byte can address, so a decoder that truncated the field would look for
    // the path 256 bytes early.
    final RemoteCall call = byLabel('one_string 300 bytes');
    expect(call, isA<PathCall>());
    expect((call as PathCall).path, 'RpcOracle');
    expect((call.args.single.value! as String).length, 300);
  });

  test('an offset that disagrees with the arguments is refused', () {
    final Uint8List bytes = Uint8List.fromList(
      calls().firstWhere((final Sample s) => s.label == 'three_mixed').bytes,
    );
    bytes[1] = (bytes[1] + 1) & 0xff;
    expect(() => RemoteCall.parse(bytes), throwsFormatException);
  });
}
