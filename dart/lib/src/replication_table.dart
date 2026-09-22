// SPDX-License-Identifier: Apache-2.0
//
// What crosses the wire, read out of the scenes that specify it (DR-1, WS-A).
//
// DR-1 says the transport is the port's own and the *field list* is not: the
// `.tscn` files already answer "which members of which node does a
// MultiplayerSynchronizer send", and answering it a second time by hand is how
// a field goes missing. So this parses `SceneReplicationConfig` sub-resources
// rather than restating them.
//
// ## Why it is worth parsing eleven lines of text
//
// Because reading them is how you find out they do not say what you assumed.
// `red_robot.gd:27` declares `aim_preparing` with `@export`, and `@export` is
// the annotation a synchronized member carries -- but the robot's config lists
// `global_transform`, `health`, `state`, `target_position` and `dead`, and not
// that. `@export` makes a member inspector-visible; it is the `.tscn` that
// decides what replicates, and only these files know.
//
// The consequence is visible in the shipping game: `aim_preparing` starts at
// `AIM_PREPARE_TIME` and is only ever advanced inside the server branch, so on
// a client it holds its initial value forever and
// `clamp(aim_preparing / AIM_PREPARE_TIME, 0, 1)` is permanently 1. A client
// sees every robot with its aim layer fully blended in, alive or idle. That is
// the original's behavior, not a porting decision -- recorded here so that
// whoever wires WS-N knows it is expected rather than a bug they introduced.

import 'dart:io';

/// One replicated member: which node, which property.
///
/// `NodePath(".:transform")` is the node the synchronizer hangs on; a path with
/// a node part -- `NodePath("CameraBase:rotation")` -- reaches a child. Kept
/// split rather than as the raw string, because the encoder needs both halves
/// and splitting it twice is how they stop agreeing.
typedef ReplicatedField = ({String node, String property, bool onSpawn, int mode});

/// Godot's `replication_mode`, in the engine's own order.
///
/// Read out of the shipping binary rather than from memory: the property hint
/// `SceneReplicationConfig` registers is the string `"Never,Always,On Change"`,
/// and the symbols beside it are `REPLICATION_MODE_NEVER`,
/// `REPLICATION_MODE_ALWAYS`, `REPLICATION_MODE_ON_CHANGE`. So 1 is *always*,
/// not "on change" -- a mode-1 field is sent every network tick.
///
/// Only 0 and 1 appear in this demo, and the distinction is load-bearing.
/// `health` and `dead` are mode 0 with `spawn = true`: they cross once, with
/// the spawn, and the synchronizer never sends them again. They stay in step
/// because `hit()` is an `@rpc("call_local")` whose body decrements health on
/// every peer -- which is why that RPC has to carry the whole block and not
/// just its visible end. The transform, `state` and `target_position` are mode
/// 1 and stream.
const int replicationModeNever = 0;
const int replicationModeAlways = 1;
const int replicationModeOnChange = 2;

/// The fields one `MultiplayerSynchronizer` sends, in the scene's own order.
class ReplicationConfig {
  const ReplicationConfig({required this.id, required this.fields});

  /// The sub-resource id, which is how the scene's nodes refer to it. Three
  /// nodes share `SceneReplicationConfig_hqtbc` in `red_robot.tscn` -- the
  /// robot's three death parts -- so the config is not one per synchronizer.
  final String id;

  final List<ReplicatedField> fields;
}

/// Every `SceneReplicationConfig` in [scenePath], keyed by sub-resource id.
///
/// Line-oriented rather than a real `.tscn` parser: these blocks are flat
/// `properties/N/key = value` assignments under a `[sub_resource]` header, and
/// the bake already reads scene text this way.
Map<String, ReplicationConfig> readReplicationConfigs(final String scenePath) {
  final List<String> lines = File(scenePath).readAsLinesSync();
  final Map<String, ReplicationConfig> out = <String, ReplicationConfig>{};

  String? id;
  final Map<int, String> paths = <int, String>{};
  final Map<int, bool> spawns = <int, bool>{};
  final Map<int, int> modes = <int, int>{};

  void flush() {
    final String? current = id;
    if (current == null) return;
    final List<int> indices = paths.keys.toList()..sort();
    out[current] = ReplicationConfig(
      id: current,
      fields: <ReplicatedField>[
        for (final int i in indices)
          _splitPath(paths[i]!, onSpawn: spawns[i] ?? true, mode: modes[i] ?? 0),
      ],
    );
    id = null;
    paths.clear();
    spawns.clear();
    modes.clear();
  }

  for (final String line in lines) {
    if (line.startsWith('[')) {
      // Any new block ends the one being collected, including the next
      // sub_resource -- these are adjacent in `player.tscn`.
      flush();
      final RegExp header = RegExp(
        r'^\[sub_resource type="SceneReplicationConfig" id="([^"]+)"\]',
      );
      final RegExpMatch? m = header.firstMatch(line);
      if (m != null) id = m.group(1);
      continue;
    }
    if (id == null) continue;
    final RegExpMatch? m =
        RegExp(r'^properties/(\d+)/(path|spawn|replication_mode) = (.*)$')
            .firstMatch(line);
    if (m == null) continue;
    final int index = int.parse(m.group(1)!);
    final String value = m.group(3)!.trim();
    switch (m.group(2)) {
      case 'path':
        final RegExpMatch? p =
            RegExp(r'^NodePath\("([^"]*)"\)$').firstMatch(value);
        if (p != null) paths[index] = p.group(1)!;
      case 'spawn':
        spawns[index] = value == 'true';
      case 'replication_mode':
        modes[index] = int.tryParse(value) ?? 0;
    }
  }
  flush();
  return out;
}

ReplicatedField _splitPath(
  final String raw, {
  required final bool onSpawn,
  required final int mode,
}) {
  final int colon = raw.indexOf(':');
  // `".:transform"` is the synchronizer's own node; the leading "." is Godot's
  // self path and carries no information, so it becomes the empty node here and
  // a child path keeps its name.
  final String node = colon < 0 ? raw : raw.substring(0, colon);
  final String property = colon < 0 ? '' : raw.substring(colon + 1);
  return (
    node: node == '.' ? '' : node,
    property: property,
    onSpawn: onSpawn,
    mode: mode,
  );
}
