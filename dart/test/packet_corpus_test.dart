// SPDX-License-Identifier: Apache-2.0
//
// Every packet the demo sends that is not a SYNC, against the captured corpus:
// SPAWN, SIMPLIFY_PATH, CONFIRM_PATH, DESPAWN and both forms of a remote call.
//
// The path fixture mixes command types on purpose, so the test dispatches on
// the first byte the way a receiver has to -- one packet arrives, and what it
// is has to be read off it rather than known in advance.
//
// Round-tripping is the assertion throughout, not just parsing. Parsing proves
// the lengths add up; re-encoding proves every width and every constant match
// what Godot wrote, which is the difference between a decoder that works on a
// corpus and an implementation that can take the other side of the wire.

import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:godot_replication/godot_replication.dart';

import 'dart:io';

/// The captured packets, from the corpus both implementations read.
List<Uint8List> fixturePackets(final String name) {
  final File file = File('../replication/tests/fixtures/$name');
  return file
      .readAsLinesSync()
      .where((final String l) => !l.startsWith('#') && l.trim().isNotEmpty)
      .map((final String l) => Uint8List.fromList(<int>[
            for (int i = 0; i < l.length; i += 2)
              int.parse(l.substring(i, i + 2), radix: 16),
          ]))
      .toList();
}

void main() {
  test('every captured SPAWN round-trips to the exact byte', () {
    final List<Uint8List> packets = fixturePackets('spawn_packets.hex');
    expect(packets.length, 112, reason: 'all of them, as captured');
    for (int i = 0; i < packets.length; i++) {
      final Spawn spawn = Spawn.parse(packets[i]);
      expect(spawn.encode(), packets[i], reason: 'packet $i');
    }
  });

  test('the synchronizer ids follow the object id, without a gap', () {
    // The rule that lets a receiver map a SYNC record to a synchronizer
    // without being told: the node takes an id and its synchronizers take the
    // next ones.
    for (final Uint8List bytes in fixturePackets('spawn_packets.hex')) {
      final Spawn spawn = Spawn.parse(bytes);
      expect(
        spawn.syncIds,
        <int>[for (int i = 1; i <= spawn.syncIds.length; i++) spawn.netId + i],
        reason: '${spawn.name} ids run from its own',
      );
    }
  });

  test('the host player is the one spawn with two synchronizers', () {
    // The packet that an earlier reading of this layout could not decode. It
    // is not a special form: the host's player is the only spawned node whose
    // server owns both a ServerSynchronizer and an InputSynchronizer.
    final Map<int, List<String>> byCount = <int, List<String>>{};
    for (final Uint8List bytes in fixturePackets('spawn_packets.hex')) {
      final Spawn spawn = Spawn.parse(bytes);
      byCount.putIfAbsent(spawn.syncIds.length, () => <String>[]).add(spawn.name);
    }
    expect(byCount.keys.toList()..sort(), <int>[1, 2]);
    expect(byCount[2], <String>['1']);
  });

  test('a spawn state decodes to its scene field list', () {
    // Scene 2 is the bullet, whose config lists one `spawn = true` property.
    // Scene 1 is the robot, with five -- `health` and `dead` among them, which
    // is how those two cross once and never stream.
    final Map<int, Set<String>> shapes = <int, Set<String>>{};
    for (final Uint8List bytes in fixturePackets('spawn_packets.hex')) {
      final Spawn spawn = Spawn.parse(bytes);
      shapes.putIfAbsent(spawn.scene, () => <String>{}).add(
            spawn.state.map((final SyncField f) => f.type.name).join(','),
          );
    }
    expect(shapes[2], <String>{'transform3d'});
    expect(shapes[1], <String>{'transform3d,int\$,int\$,vector3,bool\$'});
  });

  test('every captured path packet round-trips, dispatched by command', () {
    final List<Uint8List> packets = fixturePackets('path_packets.hex');
    expect(packets.length, greaterThan(50));

    final Map<int, int> seen = <int, int>{};
    for (int i = 0; i < packets.length; i++) {
      final Uint8List bytes = packets[i];
      seen[bytes[0]] = (seen[bytes[0]] ?? 0) + 1;
      switch (bytes[0]) {
        case commandSimplifyPath:
          expect(SimplifyPath.parse(bytes).encode(), bytes, reason: 'packet $i');
        case commandConfirmPath:
          expect(ConfirmPath.parse(bytes).encode(), bytes, reason: 'packet $i');
        case commandDespawn:
          expect(encodeDespawn(parseDespawn(bytes)), bytes, reason: 'packet $i');
        default:
          fail('packet $i has command ${bytes[0]}, which is not a path packet');
      }
    }
    // All three kinds are present, so none of the branches above is untested.
    expect(seen.keys.toList()..sort(),
        <int>[commandSimplifyPath, commandConfirmPath, commandDespawn]);
  });

  test('announced paths are spelled from the scene root', () {
    // Not `/root/...` and not a leading slash. A path announced with the
    // `/root/` prefix resolved on nobody's tree, and the far side logged a
    // node-not-found for every packet that used the id.
    for (final Uint8List bytes in fixturePackets('path_packets.hex')) {
      if (bytes[0] != commandSimplifyPath) continue;
      final SimplifyPath announced = SimplifyPath.parse(bytes);
      expect(announced.path, startsWith('main/'));
      expect(announced.rpcHash.length, rpcHashLength);
    }
  });

  test('every captured remote call round-trips, in both forms', () {
    final List<Uint8List> packets = fixturePackets('rpc_packets.hex');
    expect(packets.length, greaterThan(100));

    int cached = 0;
    int path = 0;
    for (int i = 0; i < packets.length; i++) {
      final RemoteCall call = RemoteCall.parse(packets[i]);
      expect(call.encode(), packets[i], reason: 'packet $i');
      switch (call) {
        case CachedCall():
          cached++;
        case PathCall():
          path++;
      }
    }
    // Both forms are well represented, so neither branch is untested: a
    // bullet is exploded before its path comes back confirmed, so the long
    // form keeps being used for as long as bullets keep being fired.
    expect(cached, greaterThan(50));
    expect(path, greaterThan(50));
  });

  test('the long form always carries the same offset', () {
    // 0x80000006: the flag, and where the path starts. Reading this as a node
    // id is the mistake that made a stock client log a path missing its first
    // two characters.
    for (final Uint8List bytes in fixturePackets('rpc_packets.hex')) {
      if (bytes[0] != leadPath) continue;
      expect(ByteData.sublistView(bytes).getUint32(1, Endian.little),
          pathOffsetFlag | pathOffset);
      expect((RemoteCall.parse(bytes) as PathCall).path, startsWith('main/'));
    }
  });
}
