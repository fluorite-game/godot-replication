// SPDX-License-Identifier: Apache-2.0
//
// `SPAWN`: a node coming into existence on the far side (plan.md DR-7, DR-7a).
//
// ## Layout, measured over 112 captured packets
//
//     [u8 0x04][u8 scene][u32 spawner][u32 net_id][u32 sync_count]
//     [u32 name_len][u32 sync_net_id x sync_count][name, NUL included][state]
//
// - `scene` indexes the spawner's `_spawnable_scenes` (`level.tscn:75`, three
//   entries): 0 is a player, 1 a robot, 2 a bullet.
// - `spawner` is the path cache id of `main/Level/MultiplayerSpawner`, which
//   is why that path has to be announced before any spawn that names it.
// - the synchronizer ids are always `net_id + 1 ..= net_id + sync_count`: the
//   node and each synchronizer under it take consecutive ids.
//
// ## The third word is a count, not a kind
//
// Worth repeating here because the Rust side got it wrong first and the
// mistake was invisible: in 111 of 112 packets that word is 1, and the 112th
// is the host's own player `"1"` -- the only spawned node whose server owns
// two synchronizers, `ServerSynchronizer` and `InputSynchronizer`. Reading it
// as a kind made that packet's second synchronizer id look like a two-byte
// name and its state undecodable. Nothing about the packet was unusual; the
// reading was.
//
// ## The state is compact Variants
//
// Every `spawn = true` property of every listed synchronizer, in order, in the
// same encoding SYNC uses. A bullet's is one Transform3D; a robot's is the
// five its scene lists, `health` and `dead` among them -- which is how those
// two cross exactly once and never stream.

import 'dart:convert';
import 'dart:typed_data';

import 'sync.dart';
import 'variant.dart';

/// The `SceneMultiplayer` command byte for a spawn.
const int commandSpawn = 0x04;

/// Fixed words before the synchronizer ids: command, scene, spawner, net id,
/// sync count, name length.
const int spawnHeaderLength = 18;

/// A node arriving.
class Spawn {
  const Spawn({
    required this.scene,
    required this.spawner,
    required this.netId,
    required this.syncIds,
    required this.name,
    required this.state,
    this.stateBytes,
  });

  /// A spawn whose state has already been written, by the same sink the
  /// streamed fields go through. See [SyncRecord.encoded].
  const Spawn.encoded({
    required this.scene,
    required this.spawner,
    required this.netId,
    required this.syncIds,
    required this.name,
    required Uint8List this.stateBytes,
  }) : state = const <SyncField>[];

  /// Index into the spawner's spawnable scene list.
  final int scene;

  /// The path cache id of the MultiplayerSpawner doing the spawning.
  final int spawner;

  /// The object id. DESPAWN names this one.
  final int netId;

  /// One per synchronizer the sender owns on this node.
  final List<int> syncIds;

  /// The node's name, which is its identity in every path built from it.
  final String name;

  /// The `spawn = true` properties, in order, compact. Empty on a spawn built
  /// from bytes.
  final List<SyncField> state;

  /// The encoded state, when this spawn was built from one.
  final Uint8List? stateBytes;

  static Spawn parse(final Uint8List packet) {
    if (packet.isEmpty || packet[0] != commandSpawn) {
      throw const FormatException('not a SPAWN packet');
    }
    if (packet.length < spawnHeaderLength) {
      throw const FormatException('SPAWN is cut short');
    }
    final ByteData view = ByteData.sublistView(packet);
    final int scene = packet[1];
    final int spawner = view.getUint32(2, Endian.little);
    final int netId = view.getUint32(6, Endian.little);
    final int syncCount = view.getUint32(10, Endian.little);
    final int nameLength = view.getUint32(14, Endian.little);

    final int idsEnd = spawnHeaderLength + 4 * syncCount;
    if (idsEnd + nameLength > packet.length) {
      throw FormatException(
        'SPAWN claims $syncCount ids and a $nameLength byte name, '
        '${packet.length - spawnHeaderLength} bytes left',
      );
    }
    final List<int> syncIds = <int>[
      for (int i = 0; i < syncCount; i++)
        view.getUint32(spawnHeaderLength + 4 * i, Endian.little),
    ];
    if (nameLength == 0) {
      throw const FormatException('SPAWN has no name');
    }
    // The length counts the NUL.
    final String name =
        utf8.decode(packet.sublist(idsEnd, idsEnd + nameLength - 1));

    final List<SyncField> state = <SyncField>[];
    final int stateAt = idsEnd + nameLength;
    int at = stateAt;
    while (at < packet.length) {
      final DecodedVariant read = decodeCompactVariant(packet, at);
      state.add((type: read.type, value: read.value));
      at = read.next;
    }
    return Spawn(
      scene: scene,
      spawner: spawner,
      netId: netId,
      syncIds: syncIds,
      name: name,
      state: state,
      // Kept as well as decoded: applying a spawn means handing these bytes to
      // the same reader a streamed record goes through, and writing the values
      // back out to get them would be work done twice.
      stateBytes: Uint8List.sublistView(packet, stateAt),
    );
  }

  Uint8List encode() {
    final Uint8List nameBytes = utf8.encode(name);
    final ByteData head = ByteData(spawnHeaderLength)
      ..setUint8(0, commandSpawn)
      ..setUint8(1, scene)
      ..setUint32(2, spawner, Endian.little)
      ..setUint32(6, netId, Endian.little)
      ..setUint32(10, syncIds.length, Endian.little)
      ..setUint32(14, nameBytes.length + 1, Endian.little);

    final BytesBuilder out = BytesBuilder(copy: false)
      ..add(head.buffer.asUint8List());
    for (final int id in syncIds) {
      final ByteData word = ByteData(4)..setUint32(0, id, Endian.little);
      out.add(word.buffer.asUint8List());
    }
    out
      ..add(nameBytes)
      ..addByte(0);
    if (stateBytes != null) {
      out.add(stateBytes!);
    } else {
      for (final SyncField field in state) {
        out.add(encodeCompactField(field.type, field.value));
      }
    }
    return out.takeBytes();
  }
}
