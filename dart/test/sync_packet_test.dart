// SPDX-License-Identifier: Apache-2.0
//
// The Dart framing against packets Godot actually sent.
//
// The fixtures are the same bytes the Rust crate's tests read, and that
// sharing is the point of DR-7a. Two codecs derived from two oracles -- this
// one from `var_to_bytes()` through `variant.dart`, the other from the
// captures themselves -- are only worth having if they are held to one corpus.
// A change to either that Godot would not have made fails here or there, and
// the fixture says which side is wrong.
//
// They used to be read straight out of `../net/`, when the crate lived in this
// repository. It lives at github.com/fluorite-game/godot-replication now, so
// they are vendored by `tools/sync_protocol_fixtures.sh` and that script's
// `--check` is what keeps the copy honest. A stale copy is the failure to
// guard against: both suites would pass while the two implementations quietly
// stopped agreeing about the wire.

import 'dart:io';
import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:godot_replication/src/sync.dart';
import 'package:godot_replication/src/variant.dart';

/// Vendored from the crate that was written against them first. See the note
/// above, and `tools/sync_protocol_fixtures.sh`.
const String _fixtures = '../replication/tests/fixtures';

List<Uint8List> fixturePackets(final String name) {
  final File file = File('$_fixtures/$name');
  if (!file.existsSync()) {
    throw StateError('missing fixture ${file.path}');
  }
  return <Uint8List>[
    for (final String line in file.readAsLinesSync())
      if (!line.startsWith('#') && line.trim().isNotEmpty)
        Uint8List.fromList(<int>[
          for (int i = 0; i < line.trim().length; i += 2)
            int.parse(line.trim().substring(i, i + 2), radix: 16),
        ]),
  ];
}

void main() {
  test('every captured SYNC packet parses', () {
    final List<Uint8List> packets = fixturePackets('sync_packets.hex');
    expect(packets.length, greaterThan(100),
        reason: 'the fixture is present and not truncated');
    for (int i = 0; i < packets.length; i++) {
      // Not merely "did not crash": a field read at the wrong width leaves the
      // next record header misaligned, and the length checks refuse it.
      expect(() => parseSync(packets[i]), returnsNormally,
          reason: 'packet $i (${packets[i].length} bytes)');
    }
  });

  test('and re-encodes to the exact byte', () {
    // The stronger half. Parsing proves the lengths add up; re-encoding proves
    // every width decision matches Godot's -- the narrow-when-lossless float,
    // the compact int's width code, the bool in bit 7.
    final List<Uint8List> packets = fixturePackets('sync_packets.hex');
    for (int i = 0; i < packets.length; i++) {
      expect(encodeSync(parseSync(packets[i])), packets[i],
          reason: 'packet $i round-trips');
    }
  });

  test('the shapes that fall out are the scenes property lists', () {
    // What the records *are*, not just that they decode. Every distinct field
    // shape in the corpus, by how the record is addressed.
    final Set<String> spawnShapes = <String>{};
    final Set<String> pathShapes = <String>{};
    for (final Uint8List bytes in fixturePackets('sync_packets.hex')) {
      for (final SyncRecord record in parseSync(bytes).records) {
        final String shape = record.fields
            .map((final SyncField f) => f.type.name)
            .join(',');
        (record.byPath ? pathShapes : spawnShapes).add(shape);
      }
    }

    // bullet.tscn: one Transform3D. player ServerSynchronizer: transform,
    // PlayerModel:transform, motion, current_animation. red_robot.tscn's
    // mode-1 three: global_transform, state, target_position. The player's
    // InputSynchronizer: two camera rotations, shoot_target, motion, shooting,
    // aiming. The mode-0 fields are absent, as their replication mode says.
    expect(spawnShapes, <String>{
      'transform3d',
      'transform3d,transform3d,vector2,int\$',
      'transform3d,int\$,vector3',
      'vector3,vector3,vector3,vector2,bool\$,bool\$',
    });
    // Addressed by path rather than by a spawn id: the BulletCache
    // synchronizers, and -- the one that is easy to get wrong -- a client's
    // own InputSynchronizer. The same config appears in both sets, and that is
    // not a duplicate: the host's player is listed in a SPAWN, so its input
    // crosses under a spawn id, while a joining client's input synchronizer is
    // something only that client owns, announced with SIMPLIFY_PATH and sent
    // the other way down the wire.
    expect(pathShapes, <String>{
      'transform3d',
      'vector3,vector3,vector3,vector2,bool\$,bool\$',
    });
  });

  test('a record whose length overruns the packet is refused', () {
    // The failure that matters is silent: a length one too long reads the next
    // record's header as a field, and every value after it is plausible.
    final SyncPacket packet = SyncPacket(
      counter: 1,
      records: <SyncRecord>[
        SyncRecord(
          netId: 2,
          fields: <SyncField>[(type: VariantType.int$, value: 7)],
        ),
      ],
    );
    final Uint8List good = encodeSync(packet);
    final Uint8List bad = Uint8List.fromList(good)
      ..[syncHeaderLength + 4] = 0xFF;
    expect(() => parseSync(bad), throwsFormatException);
  });
}
