// SPDX-License-Identifier: Apache-2.0
//
// Two ENet connections over a link that misbehaves on demand.
//
// Reliability bugs hide on loopback, where nothing is ever lost, reordered or
// duplicated -- which is exactly the environment every local test runs in. So
// the link here does all three, on a schedule the test chooses, with a clock
// the test advances by hand. Nothing waits for a timer and nothing depends on
// how busy the machine is: the same run happens every time.
//
// The measured half of ENet is in `enet_protocol_test.dart`, against captured
// bytes. This is the half a capture cannot settle -- what a peer *does* when a
// packet goes missing -- so it is held to behavior rather than to bytes.

import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:godot_replication/src/enet/peer.dart';
import 'package:godot_replication/src/enet/protocol.dart';

/// A link between two connections that can be told to misbehave.
class _Link {
  _Link(this.a, this.b);

  final EnetConnection a;
  final EnetConnection b;

  int now = 0;

  /// Datagrams in flight, as (arrival time, destination, bytes).
  final List<({int at, EnetConnection to, Uint8List bytes})> _flying =
      <({int at, EnetConnection to, Uint8List bytes})>[];

  /// What each side has been handed.
  final Map<EnetConnection, List<EnetDelivery>> delivered =
      <EnetConnection, List<EnetDelivery>>{};

  /// Drop the nth datagram sent, counting from 1.
  final Set<int> dropAt = <int>{};

  /// Deliver the nth datagram twice.
  final Set<int> duplicateAt = <int>{};

  /// Hold the nth datagram back by this many milliseconds, which reorders it
  /// past whatever follows.
  final Map<int, int> delayAt = <int, int>{};

  int _sent = 0;
  int get sent => _sent;

  /// One tick: collect what each side wants to send, then deliver what has
  /// arrived. Advances the clock first, so a connection sees time pass.
  void tick([final int stepMs = 10]) {
    now += stepMs;
    for (final EnetConnection from in <EnetConnection>[a, b]) {
      final EnetConnection to = from == a ? b : a;
      for (final Uint8List bytes in from.takeOutgoing(now)) {
        _sent++;
        if (dropAt.contains(_sent)) continue;
        final int at = now + (delayAt[_sent] ?? 0);
        _flying.add((at: at, to: to, bytes: bytes));
        if (duplicateAt.contains(_sent)) {
          _flying.add((at: at, to: to, bytes: bytes));
        }
      }
    }
    final List<({int at, EnetConnection to, Uint8List bytes})> due = <({
      int at,
      EnetConnection to,
      Uint8List bytes
    })>[
      for (final ({int at, EnetConnection to, Uint8List bytes}) f in _flying)
        if (f.at <= now) f,
    ];
    _flying.removeWhere(
      (final ({int at, EnetConnection to, Uint8List bytes}) f) => f.at <= now,
    );
    for (final ({int at, EnetConnection to, Uint8List bytes}) f in due) {
      delivered
          .putIfAbsent(f.to, () => <EnetDelivery>[])
          .addAll(f.to.receive(parseDatagram(f.bytes), now));
    }
  }

  void run(final int ticks) {
    for (int i = 0; i < ticks; i++) {
      tick();
    }
  }

  List<String> payloadsFor(final EnetConnection side) => <String>[
        for (final EnetDelivery d in delivered[side] ?? <EnetDelivery>[])
          String.fromCharCodes(d.payload),
      ];
}

Uint8List _bytes(final String text) => Uint8List.fromList(text.codeUnits);

void main() {
  ({_Link link, EnetConnection a, EnetConnection b}) pair({
    final int minimumTimeout = 200,
    final int maximumAttempts = 8,
  }) {
    final EnetConnection a = EnetConnection(
      outgoingPeerId: 0,
      minimumTimeout: minimumTimeout,
      maximumAttempts: maximumAttempts,
    );
    final EnetConnection b = EnetConnection(
      outgoingPeerId: 0,
      minimumTimeout: minimumTimeout,
      maximumAttempts: maximumAttempts,
    );
    return (link: _Link(a, b), a: a, b: b);
  }

  test('reliable payloads arrive, in order, exactly once', () {
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    for (final String text in <String>['one', 'two', 'three']) {
      p.a.sendReliable(channelReliable, _bytes(text));
    }
    p.link.run(4);
    expect(p.link.payloadsFor(p.b), <String>['one', 'two', 'three']);
  });

  test('a dropped reliable payload is resent and still arrives in order', () {
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.link.dropAt.add(1);
    for (final String text in <String>['one', 'two', 'three']) {
      p.a.sendReliable(channelReliable, _bytes(text));
    }
    // Long enough for the retransmission timeout to come round.
    p.link.run(60);
    expect(p.link.payloadsFor(p.b), <String>['one', 'two', 'three']);
  });

  test('a reordered payload waits for the gap ahead of it', () {
    // The property a reliable channel exists for. The second datagram is held
    // back past the third, so `two` arrives after `three` -- and must still be
    // delivered before it.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.a.sendReliable(channelReliable, _bytes('one'));
    p.link.tick();
    p.a.sendReliable(channelReliable, _bytes('two'));
    p.link.delayAt[p.link.sent + 1] = 100;
    p.link.tick();
    p.a.sendReliable(channelReliable, _bytes('three'));
    p.link.run(30);

    expect(p.link.payloadsFor(p.b), <String>['one', 'two', 'three']);
  });

  test('a duplicated reliable payload is delivered once', () {
    // The acknowledgement for a packet can go missing, in which case the far
    // side sends the same packet again. It must not arrive at the application
    // twice -- a spawn applied twice is two nodes.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.link.duplicateAt.add(1);
    p.a.sendReliable(channelReliable, _bytes('spawn'));
    p.link.run(40);
    expect(p.link.payloadsFor(p.b), <String>['spawn']);
  });

  test('an unsequenced payload is not resent when it is lost', () {
    // The other half of the trade: a lost state packet is a tick that did not
    // arrive, and the next one supersedes it. Resending would deliver stale
    // state late, which is worse than not delivering it.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.link.dropAt.add(1);
    p.a.sendUnsequenced(channelUnsequenced, _bytes('tick one'));
    p.link.run(40);
    expect(p.link.payloadsFor(p.b), isEmpty);

    p.a.sendUnsequenced(channelUnsequenced, _bytes('tick two'));
    p.link.run(2);
    expect(p.link.payloadsFor(p.b), <String>['tick two']);
  });

  test('an unsequenced payload that arrives late is dropped, not applied', () {
    // The one thing an unsequenced channel must not do is move the world
    // backwards. A packet overtaken by a newer one is of no use by the time it
    // turns up, and applying it would undo the newer state.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.a.sendUnsequenced(channelUnsequenced, _bytes('older'));
    p.link.delayAt[p.link.sent + 1] = 100;
    p.link.tick();
    p.a.sendUnsequenced(channelUnsequenced, _bytes('newer'));
    p.link.run(30);

    expect(p.link.payloadsFor(p.b), <String>['newer'],
        reason: 'the older packet arrived second and was discarded');
  });

  test('a duplicated unsequenced payload is delivered once', () {
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.link.duplicateAt.add(1);
    p.a.sendUnsequenced(channelUnsequenced, _bytes('tick'));
    p.link.run(4);
    expect(p.link.payloadsFor(p.b), <String>['tick']);
  });

  test('the two channels do not block each other', () {
    // Head-of-line blocking is per channel, and that is the point of having
    // two: a lost spawn must not hold up the state stream behind it. Measured
    // as 2.5x on the wire by `tool/transport_bench.dart`; here it is the
    // mechanism rather than the milliseconds.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    p.link.dropAt.add(1);
    p.a.sendReliable(channelReliable, _bytes('spawn'));
    p.link.tick();
    for (final String tick in <String>['s1', 's2', 's3']) {
      p.a.sendUnsequenced(channelUnsequenced, _bytes(tick));
      p.link.tick();
    }

    // The state arrived while the spawn was still being resent.
    expect(p.link.payloadsFor(p.b), <String>['s1', 's2', 's3']);
    p.link.run(60);
    expect(p.link.payloadsFor(p.b), containsAll(<String>['spawn', 's3']));
  });

  test('a peer that never answers is given up on', () {
    // Not an error to report upward for a while -- a link that drops
    // everything for a second is a link, not a failure -- but it cannot be
    // retried forever either.
    // The timing is this implementation's policy rather than anything the
    // wire says, so the test states it rather than depending on the defaults:
    // three attempts, with a floor of fifty milliseconds between them.
    final ({_Link link, EnetConnection a, EnetConnection b}) p =
        pair(minimumTimeout: 50, maximumAttempts: 3);
    for (int i = 1; i < 400; i++) {
      p.link.dropAt.add(i);
    }
    p.a.sendReliable(channelReliable, _bytes('spawn'));
    expect(p.a.lost, isFalse);
    p.link.run(20);
    expect(p.a.lost, isFalse,
        reason: 'a link that drops for a moment is a link, not a failure');
    // Three attempts, and the wait doubles each time -- starting from the
    // round-trip *guess* of half a second, which is what a connection has
    // before any acknowledgement has told it otherwise.
    p.link.run(400);
    expect(p.a.lost, isTrue);
  });

  test('a round trip is measured from the time the sender stamped', () {
    // An acknowledgement carries back the sent time it was told, so a sender
    // needs no per-packet clock of its own. With the link delivering in the
    // same tick, the estimate should fall well below its starting guess.
    final ({_Link link, EnetConnection a, EnetConnection b}) p = pair();
    final int before = p.a.roundTripTime;
    for (int i = 0; i < 12; i++) {
      p.a.sendReliable(channelReliable, _bytes('ping $i'));
      p.link.run(2);
    }
    expect(p.a.roundTripTime, lessThan(before));
  });
}
