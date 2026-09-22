// SPDX-License-Identifier: Apache-2.0
//
// How Godot numbers a node's RPCs, and what it hashes to agree on that
// (plan.md DR-7).
//
// ## The problem this solves
//
// A remote call crosses the wire as a method *id*, not a name. Both ends have
// to derive the same id from the same node, and they check that they have by
// exchanging an MD5 of the node's RPC config in the `SIMPLIFY_PATH` packet.
// Get either wrong and every call lands on the wrong method -- silently, since
// a wrong id is still a valid id.
//
// ## Both rules are measured, not read
//
// **The id is the index in *sorted* order, not declaration order.** Measured
// three ways in one capture (`tools/capture_net.sh --shoot`, 13901 packets):
//
//   * `player.gd` declares jump, land, shoot, hit, add_camera_shake_trauma.
//     A session where the client holds the trigger puts method id **4** on the
//     wire 106 times, once per shot. Sorted, `shoot` is index 4; declared, it
//     is 2.
//   * The same session carries method id **3** exactly once, addressed to
//     `main/Level/SpawnedNodes/466750851` -- the client's own player, just
//     after spawn. Sorted, index 3 is `land`, which a character does once when
//     it first touches the floor. Declared, index 3 is `hit`, which nothing
//     hit it with.
//   * 107 calls of method id **0** to bullet nodes, whose script declares one
//     RPC, `explode` -- one per shot, plus one still in flight.
//
// **The hash is the MD5 of the sorted names concatenated, UTF-8, and nothing
// else.** `bullet.gd`'s config is one method with `rpc_mode` 2 and
// `call_local` true, and its hash on the wire is
// `f821b5159d85278da0badf5d32ffe210`, which is exactly `md5("explode")` -- the
// mode and the flag are not in it. Nodes with no RPCs at all hash to
// `d41d8cd98f00b204e9800998ecf8427e`, which is `md5("")`.
//
// That was then made falsifiable: the hashes for all five of this demo's
// scripts were predicted from the rule and searched for across three separate
// captures. Every hash present in every capture is one of the predicted set,
// and there are no unmatched hashes.
//
// `tools/rpc_config_oracle.gd` prints the configs this is derived from, by
// asking `Script.get_rpc_config()` -- the only accessor of the three
// candidates that answers in 4.5.2.

import 'dart:convert';

import 'package:crypto/crypto.dart';

/// The MD5 Godot puts in `SIMPLIFY_PATH` for a node with these RPC methods.
///
/// [methods] is taken in any order; the sort is part of the rule.
String rpcConfigHash(final Iterable<String> methods) {
  final List<String> sorted = methods.toList()..sort();
  return md5.convert(utf8.encode(sorted.join())).toString();
}

/// The id [method] travels under, or null if this node does not declare it.
int? rpcMethodId(final Iterable<String> methods, final String method) {
  final List<String> sorted = methods.toList()..sort();
  final int index = sorted.indexOf(method);
  return index < 0 ? null : index;
}

/// The method a given id names, or null if the id is out of range.
///
/// A wrong id is still a valid id at the far end, so a decoder that cannot
/// answer this has no way to notice it is desynchronized -- which is the whole
/// reason the hash is exchanged.
String? rpcMethodName(final Iterable<String> methods, final int id) {
  final List<String> sorted = methods.toList()..sort();
  return id >= 0 && id < sorted.length ? sorted[id] : null;
}

/// The demo's own RPC configs, by script.
///
/// Read out of the shipping game by `tools/rpc_config_oracle.gd` rather than
/// transcribed from the `.gd` files: the annotation says which methods are
/// RPCs, and only the engine says what the resulting config is.
const Map<String, List<String>> demoRpcConfigs = <String, List<String>>{
  'player.gd': <String>[
    'jump',
    'land',
    'shoot',
    'hit',
    'add_camera_shake_trauma',
  ],
  'player_input.gd': <String>['jump'],
  'bullet.gd': <String>['explode'],
  'red_robot.gd': <String>['hit', 'play_shoot'],
  'part.gd': <String>['destroy'],
};
