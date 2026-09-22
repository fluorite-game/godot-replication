// SPDX-License-Identifier: Apache-2.0
//
// The RPC numbering and hash, against what Godot put on the wire.
//
// Every figure here was read out of a capture in
// `/mnt/dev/tps-demo-perf/net-corpus`, not out of the engine's source. See
// lib/net/rpc_config.dart for how each was pinned.

import 'package:test/test.dart';
import 'package:godot_replication/src/rpc_config.dart';

void main() {
  test('the id is the index in sorted order, not declaration order', () {
    final List<String> player = demoRpcConfigs['player.gd']!;
    // Declaration order, as `player.gd` writes them.
    expect(player, <String>[
      'jump',
      'land',
      'shoot',
      'hit',
      'add_camera_shake_trauma',
    ]);

    // What the wire said. A session holding the trigger carried method id 4
    // one hundred and six times, once per shot.
    expect(rpcMethodId(player, 'shoot'), 4);
    expect(rpcMethodName(player, 4), 'shoot');
    // And id 3 exactly once, to the client's own player just after it spawned.
    expect(rpcMethodId(player, 'land'), 3);
    expect(rpcMethodName(player, 3), 'land');

    // Declaration order would have put `shoot` at 2 and `hit` at 3, and the
    // capture contains no hit at all -- nothing shot the player.
    expect(player.indexOf('shoot'), 2, reason: 'the order that is not used');
    expect(player.indexOf('hit'), 3);
  });

  test('a bullet has one RPC and it travels as id 0', () {
    final List<String> bullet = demoRpcConfigs['bullet.gd']!;
    expect(bullet, <String>['explode']);
    expect(rpcMethodId(bullet, 'explode'), 0);
    // 107 of these in the shooting capture: one per shot, plus one still in
    // flight when the session ended.
  });

  test('the hash is md5 of the sorted names joined, and nothing else', () {
    // `bullet.gd`'s config is `explode` with rpc_mode 2 and call_local true.
    // The wire carries this hash for every node running that script, and it is
    // md5("explode") -- so neither the mode nor the flag is in the digest.
    expect(rpcConfigHash(<String>['explode']),
        'f821b5159d85278da0badf5d32ffe210');

    // A node with no RPCs, which is what every MultiplayerSpawner and
    // MultiplayerSynchronizer in the demo hashes to.
    expect(rpcConfigHash(<String>[]), 'd41d8cd98f00b204e9800998ecf8427e');

    // Predicted from the rule before being searched for, then found in three
    // separate captures with no unmatched hash left over.
    expect(rpcConfigHash(demoRpcConfigs['player.gd']!),
        'c54b18d512c48639aa169b0e05e68195');
    expect(rpcConfigHash(demoRpcConfigs['player_input.gd']!),
        'ba535ef5a9f7b8bc875812bb081286bb');

    // Not yet seen on a wire: nothing in the captures sent an RPC to a robot
    // or to a death part, so these two are the rule's prediction and are
    // marked as such until a capture with an engagement in it confirms them.
    expect(rpcConfigHash(demoRpcConfigs['red_robot.gd']!),
        '58f7e2b471c71ac7c69d8daef123a6cb');
    expect(rpcConfigHash(demoRpcConfigs['part.gd']!),
        'fb14982288108e1fbd6207ef55f05027');
  });

  test('the sort is over the names, so input order cannot change the hash', () {
    // The demo's configs arrive in declaration order and a reimplementation
    // might hold them in any order at all; the hash has to be the same either
    // way or two correct implementations disagree.
    final List<String> shuffled = demoRpcConfigs['player.gd']!.reversed.toList();
    expect(rpcConfigHash(shuffled), rpcConfigHash(demoRpcConfigs['player.gd']!));
    expect(rpcMethodId(shuffled, 'shoot'), 4);
  });

  test('an id nothing declares is an answerable question', () {
    // A wrong id is still a valid id at the far end. A decoder that cannot say
    // "no such method" has no way to notice it is desynchronized, which is the
    // reason the hash is exchanged at all.
    expect(rpcMethodName(demoRpcConfigs['bullet.gd']!, 4), isNull);
    expect(rpcMethodId(demoRpcConfigs['bullet.gd']!, 'shoot'), isNull);
  });
}
