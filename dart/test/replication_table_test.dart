// SPDX-License-Identifier: Apache-2.0
//
// Reading a `SceneReplicationConfig` out of scene text.
//
// The block below is the shape Godot writes: flat `properties/N/key = value`
// assignments under a `[sub_resource]` header, with the node and the property
// joined in one `NodePath` that has to be split on the colon. It is written
// out here rather than pointed at a game's scene, so this package's tests need
// nothing but the package.
//
// A game's real scenes are the better test of whether the *field lists* are
// right, and the game that has them tests that. What is checked here is that
// the text parses into the shape the protocol then sends.

import 'dart:io';

import 'package:godot_replication/src/replication_table.dart';
import 'package:test/test.dart';

/// Two configs, one of them shared by more than one synchronizer -- which is
/// the case that makes the sub-resource id matter rather than the node.
const String _scene = '''
[gd_scene load_steps=2 format=3]

[sub_resource type="SceneReplicationConfig" id="SceneReplicationConfig_aaaaa"]
properties/0/path = NodePath(".:position")
properties/0/spawn = true
properties/0/replication_mode = 1
properties/1/path = NodePath("Camera:rotation")
properties/1/spawn = false
properties/1/replication_mode = 2

[sub_resource type="SceneReplicationConfig" id="SceneReplicationConfig_bbbbb"]
properties/0/path = NodePath(".:health")
properties/0/spawn = true
properties/0/replication_mode = 0

[node name="Root" type="Node3D"]
''';

void main() {
  late Directory dir;
  late String path;

  setUp(() {
    dir = Directory.systemTemp.createTempSync('replication_table');
    path = '${dir.path}/scene.tscn';
    File(path).writeAsStringSync(_scene);
  });

  tearDown(() => dir.deleteSync(recursive: true));

  test('every config in the file comes back, keyed by sub-resource id', () {
    final Map<String, ReplicationConfig> configs = readReplicationConfigs(path);
    expect(configs.keys, <String>{
      'SceneReplicationConfig_aaaaa',
      'SceneReplicationConfig_bbbbb',
    });
    expect(configs['SceneReplicationConfig_aaaaa']!.fields, hasLength(2));
    expect(configs['SceneReplicationConfig_bbbbb']!.fields, hasLength(1));
  });

  test('the node and the property are split on the colon', () {
    // `.:position` is a property of the synchronizer's own node and
    // `Camera:rotation` one of a child. Sent as one path and read as two, and
    // a reader that kept them joined would ask the wrong object for the value.
    //
    // The self node comes back empty rather than as ".": Godot's self path
    // carries no information once the two halves are separated, and leaving
    // the dot in would make every consumer strip it.
    final ReplicationConfig config =
        readReplicationConfigs(path)['SceneReplicationConfig_aaaaa']!;
    expect(config.fields[0].node, '');
    expect(config.fields[0].property, 'position');
    expect(config.fields[1].node, 'Camera');
    expect(config.fields[1].property, 'rotation');
  });

  test('spawn and mode come across as written, not as defaults', () {
    // The mode decides whether a field is sent every tick or only when it
    // changes, and a reader that defaulted them would send a never-field
    // forever -- which is a bug the wire shows and the game does not.
    final List<ReplicatedField> a =
        readReplicationConfigs(path)['SceneReplicationConfig_aaaaa']!.fields;
    expect(a[0].onSpawn, isTrue);
    expect(a[0].mode, 1);
    expect(a[1].onSpawn, isFalse);
    expect(a[1].mode, 2);
  });

  test('a file with no configs in it is empty rather than an error', () {
    final String bare = '${dir.path}/bare.tscn';
    File(bare).writeAsStringSync('[gd_scene format=3]\n\n[node name="N"]\n');
    expect(readReplicationConfigs(bare), isEmpty);
  });
}
