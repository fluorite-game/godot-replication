// SPDX-License-Identifier: Apache-2.0
//
// Every Variant type, against bytes the engine wrote.
//
// The same fixture the Rust suite asserts against -- `variant_types.hex`, the
// output of `oracle/variant_oracle.gd` under Godot 4.5.2. Two samples per
// type, one at its zero and one whose every field differs, because a codec
// written against zeroed samples passes by returning zeroes and one written
// against symmetric values passes with its axes swapped.
//
// Holding both implementations to one corpus is the whole argument for having
// two. If this file and the Rust one disagree, the fixture says which side is
// wrong.

import 'dart:io';
import 'dart:typed_data';

import 'package:godot_replication/src/variant.dart';
import 'package:test/test.dart';

/// One line: the label, the type id the engine reported, and the bytes.
typedef Sample = ({String label, int id, Uint8List bytes});

List<Sample> samples() {
  final File file = File('../replication/tests/fixtures/variant_types.hex');
  if (!file.existsSync()) {
    throw StateError('no fixture at ${file.path}');
  }
  return <Sample>[
    for (final String line in file.readAsLinesSync())
      if (!line.startsWith('#') && line.trim().isNotEmpty)
        () {
          final List<String> parts = line.split('\t');
          final String hex = parts[2];
          return (
            label: parts[0],
            id: int.parse(parts[1]),
            bytes: Uint8List.fromList(<int>[
              for (int i = 0; i + 1 < hex.length; i += 2)
                int.parse(hex.substring(i, i + 2), radix: 16),
            ]),
          );
        }(),
  ];
}

void main() {
  test('the oracle covers every type this package claims', () {
    final Set<int> sampled = samples().map((final Sample s) => s.id).toSet();
    // Walk the id space rather than the enum, so a type added to the enum
    // without a sample fails here instead of shipping untested.
    for (int id = 0; id <= 38; id++) {
      final bool known = VariantType.byId(id) != null;
      final bool covered = sampled.contains(id);
      expect(
        known,
        covered,
        reason: 'id $id is ${known ? "handled" : "refused"} by the package '
            'and ${covered ? "sampled" : "unsampled"} by the oracle',
      );
    }
  });

  test('every sample decodes to the type the engine labelled it', () {
    for (final Sample s in samples()) {
      final DecodedVariant read = decodeVariant(s.bytes);
      expect(read.type.id, s.id, reason: s.label);
    }
  });

  test('decoding consumes exactly what the engine wrote', () {
    // A decoder that stops short leaves a tail, and in a packet the next
    // field's header is read from wherever this one stopped. Short by four
    // bytes is not a truncated value, it is a corrupted packet with no error
    // anywhere in it.
    for (final Sample s in samples()) {
      final DecodedVariant read = decodeVariant(s.bytes);
      expect(read.next, s.bytes.length, reason: s.label);
    }
  });

  test('re-encoding reproduces the engine bytes, padding aside', () {
    // The exception is padding: Godot pads strings to four bytes from whatever
    // its buffer already held rather than zeroing, and the captured NodePath
    // pads "Level" with `30 30 30`. Those cannot be reproduced byte for byte,
    // so they are checked by decoding the re-encoding instead.
    for (final Sample s in samples()) {
      final DecodedVariant read = decodeVariant(s.bytes);
      final Uint8List again = encodeVariantValue(read.type, read.value);
      if (_sameBytes(again, s.bytes)) continue;
      expect(again.length, s.bytes.length,
          reason: '${s.label}: re-encoding changed the length');
      final DecodedVariant round = decodeVariant(again);
      expect(_describe(round.value), _describe(read.value),
          reason: '${s.label}: re-encoding changed the value, not the padding');
    }
  });

  test('a zero sample and a distinctive one do not decode alike', () {
    // Catches a decoder that returns a default. Every type has two samples and
    // they are never the same value.
    final List<Sample> all = samples();
    for (int i = 0; i + 1 < all.length; i += 2) {
      if (all[i].id != all[i + 1].id) continue;
      expect(
        _describe(decodeVariant(all[i].bytes).value),
        isNot(_describe(decodeVariant(all[i + 1].bytes).value)),
        reason: '${all[i].label} and ${all[i + 1].label}',
      );
    }
  });

  test('a type this package refuses says so rather than guessing', () {
    // 24 is Object. An unknown type has an unknown length, so there is no next
    // field to find and continuing would turn one unreadable value into an
    // unreadable packet.
    final Uint8List bytes = Uint8List.fromList(<int>[24, 0, 0, 0, 1, 2, 3, 4]);
    expect(() => decodeVariant(bytes), throwsFormatException);
  });

  test('a string length is bytes, not characters', () {
    // The sample with an em dash and a hiragana in it. A decoder counting
    // characters passes every ASCII sample and fails this one.
    final Sample s =
        samples().firstWhere((final Sample s) => s.label == 'String utf8');
    final DecodedVariant read = decodeVariant(s.bytes);
    expect(read.value, 'smörgås—あ');
    expect(read.next, s.bytes.length);
  });

  test('a NodePath keeps its parts rather than its text', () {
    final Sample s = samples()
        .firstWhere((final Sample s) => s.label == 'NodePath subname');
    final NodePathValue path = decodeVariant(s.bytes).value! as NodePathValue;
    expect(path.names, <String>['Player']);
    expect(path.subnames, <String>['position', 'x']);
    expect(path.absolute, isFalse);
    expect(path.text, 'Player:position:x');
  });
}

bool _sameBytes(final Uint8List a, final Uint8List b) {
  if (a.length != b.length) return false;
  for (int i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// A stable rendering, so nested lists and records compare by content.
String _describe(final Object? value) => switch (value) {
      null => 'nil',
      final List<(Object?, Object?)> pairs => pairs
          .map((final (Object?, Object?) p) =>
              '${_describe(p.$1)}=>${_describe(p.$2)}')
          .join(','),
      final List<Object?> items => items.map(_describe).join(','),
      _ => value.toString(),
    };
