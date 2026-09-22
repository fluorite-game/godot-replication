// SPDX-License-Identifier: Apache-2.0
//
// One ENet connection: sequencing, acknowledgement and retransmission
// (plan.md DR-7, DR-7b).
//
// ## What is measured here, and what is not
//
// The framing is measured -- `tools/enet_handshake_report.py` and the
// datagram fixture say what the bytes are, and the tests hold this to them.
// The *timing* is not, and cannot be: a capture shows that a packet was resent,
// not the rule that decided when. So the retransmission policy below is this
// implementation's own, stated as such, with ENet's shape but not its
// constants. What matters for interoperating is that a peer resends what was
// not acknowledged and acknowledges what it received; when it does so is a
// quality-of-implementation question that the far side cannot see.
//
// ## What the capture did settle
//
//   * The 0x80 flag on a command means *this must be acknowledged*, not "this
//     is an acknowledgement". It is set on connect, on ping and on every
//     reliable send; the acknowledgement itself is a command of its own.
//   * Reliable sequence numbers are per channel, and start at 1. In the join,
//     channel 255 runs 1, 2 for the connect and the first ping while channel 0
//     independently runs 1 through 9 for the replication commands.
//   * An unsequenced command carries sequence number 0 and a group number of
//     its own, and its datagram has no sent time -- the sent time is there for
//     round-trip measurement, and nothing measures a packet nobody acks.
//   * An acknowledgement carries the sequence number it answers and the sent
//     time it was told, which is what lets the sender measure a round trip
//     without keeping a clock per packet.

import 'dart:math' as math;
import 'dart:typed_data';

import 'protocol.dart';

/// Godot's two channels, and ENet's own.
const int channelReliable = 0;
const int channelUnsequenced = 1;
const int channelControl = 255;

/// The id a peer uses before the other end has given it one.
const int unassignedPeerId = 0x0FFF;

/// A payload that arrived, with the channel it arrived on.
typedef EnetDelivery = ({int channel, Uint8List payload});

/// One reliable command waiting to be acknowledged.
class _Unacked {
  _Unacked({
    required this.command,
    required this.channel,
    required this.sequence,
    required this.payload,
    required this.sentAt,
  });

  final EnetCommand command;
  final int channel;
  final int sequence;
  final Uint8List? payload;

  int sentAt;
  int attempts = 1;
}

/// Per-channel sequencing state.
class _Channel {
  /// The next reliable sequence number to hand out. ENet starts at 1, which
  /// the capture's channel 0 confirms.
  int outgoing = 1;

  /// The highest reliable sequence number delivered to the application.
  int incoming = 0;

  /// Arrived out of order, waiting for the gap ahead of them to fill.
  final Map<int, Uint8List> held = <int, Uint8List>{};
}

/// One connection to one peer.
///
/// Deliberately free of sockets and of the clock: time arrives as a parameter
/// and datagrams leave in a list. That is what lets the tests run a connection
/// against a link that drops, reorders and duplicates on demand, deterministic
/// and without waiting for anything.
class EnetConnection {
  EnetConnection({
    required this.outgoingPeerId,
    this.minimumTimeout = 200,
    this.maximumAttempts = 8,
  });

  /// The id to put in the header: what the *far side* calls this connection.
  int outgoingPeerId;

  /// The floor on a retransmission timeout, in milliseconds.
  ///
  /// This implementation's policy, not ENet's and not measurable from a
  /// capture. 200 ms is twelve frames at the demo's tick rate -- long enough
  /// not to resend a packet that is merely in flight on a slow link, short
  /// enough that a spawn nobody received does not hold up the world for a
  /// noticeable time.
  final int minimumTimeout;

  /// How many times a command is resent before the connection is given up on.
  final int maximumAttempts;

  final Map<int, _Channel> _channels = <int, _Channel>{};
  final List<_Unacked> _unacked = <_Unacked>[];
  final List<({int channel, int sequence, int sentTime})> _pendingAcks =
      <({int channel, int sequence, int sentTime})>[];

  /// Queued and not yet put in a datagram.
  final List<EnetPacketCommand> _queued = <EnetPacketCommand>[];

  /// The group number on outgoing unsequenced commands. ENet numbers them so a
  /// receiver can discard duplicates; it increments per packet, per peer.
  int _unsequencedGroup = 0;

  /// The highest group seen from the far side. Anything at or below it has
  /// been seen, so a duplicate is dropped rather than applied twice.
  int _incomingUnsequencedGroup = 0;

  /// Round-trip estimate and its variance, in milliseconds.
  int roundTripTime = 500;
  int roundTripVariance = 0;

  /// True once the far side has stopped answering.
  bool get lost => _lost;
  bool _lost = false;

  _Channel _channel(final int id) =>
      _channels.putIfAbsent(id, () => _Channel());

  /// The current retransmission timeout, in milliseconds.
  int get _timeout =>
      math.max(minimumTimeout, roundTripTime + 4 * roundTripVariance);

  /// Queue a payload for reliable, ordered delivery on [channel].
  void sendReliable(final int channel, final Uint8List payload) {
    final _Channel state = _channel(channel);
    final int sequence = state.outgoing++;
    _queued.add(EnetPacketCommand(
      command: EnetCommand.sendReliable,
      flags: commandFlagAcknowledge,
      channel: channel,
      reliableSequenceNumber: sequence,
      fields: <int>[payload.length],
      payload: payload,
    ));
  }

  /// Queue a payload for unsequenced delivery on [channel].
  ///
  /// No sequence number, no acknowledgement, no retransmission: it arrives
  /// when it arrives or not at all. For state that is resent every tick that
  /// is the right trade, and it is the channel Godot puts SYNC on.
  void sendUnsequenced(final int channel, final Uint8List payload) {
    _unsequencedGroup++;
    _queued.add(EnetPacketCommand(
      command: EnetCommand.sendUnsequenced,
      flags: commandFlagUnsequenced,
      channel: channel,
      reliableSequenceNumber: 0,
      fields: <int>[_unsequencedGroup, payload.length],
      payload: payload,
    ));
  }

  /// Queue a raw command, for the handshake and for pings.
  void sendCommand(final EnetPacketCommand command) => _queued.add(command);

  /// Everything this connection wants to put on the wire now.
  ///
  /// Acknowledgements first: they are what stops the far side resending, and
  /// holding them behind a queue of new data is how a link that is merely busy
  /// starts looking like a link that is failing.
  List<Uint8List> takeOutgoing(final int nowMs) {
    final List<EnetPacketCommand> commands = <EnetPacketCommand>[];

    for (final ({int channel, int sequence, int sentTime}) ack in _pendingAcks) {
      commands.add(EnetPacketCommand(
        command: EnetCommand.acknowledge,
        flags: 0,
        channel: ack.channel,
        reliableSequenceNumber: ack.sequence,
        fields: <int>[ack.sequence, ack.sentTime],
      ));
    }
    _pendingAcks.clear();

    // Anything unacknowledged for longer than the timeout goes again, under
    // its original sequence number -- a retransmission is the same packet, not
    // a new one, or the far side would see a gap it can never fill.
    for (final _Unacked pending in _unacked) {
      if (nowMs - pending.sentAt < _timeout * pending.attempts) continue;
      if (pending.attempts >= maximumAttempts) {
        _lost = true;
        continue;
      }
      pending.attempts++;
      pending.sentAt = nowMs;
      commands.add(EnetPacketCommand(
        command: pending.command,
        flags: commandFlagAcknowledge,
        channel: pending.channel,
        reliableSequenceNumber: pending.sequence,
        fields: <int>[if (pending.payload != null) pending.payload!.length],
        payload: pending.payload,
      ));
    }

    for (final EnetPacketCommand command in _queued) {
      commands.add(command);
      if (command.flags & commandFlagAcknowledge != 0) {
        _unacked.add(_Unacked(
          command: command.command,
          channel: command.channel,
          sequence: command.reliableSequenceNumber,
          payload: command.payload,
          sentAt: nowMs,
        ));
      }
    }
    _queued.clear();

    if (commands.isEmpty) return const <Uint8List>[];
    // One datagram per call is enough for this demo's rate: the largest thing
    // it sends is 1057 bytes against an MTU of 1392, so nothing here needs
    // splitting, and the fragment commands are deliberately not implemented.
    final bool needsTime = commands.any(
      (final EnetPacketCommand c) => c.flags & commandFlagAcknowledge != 0,
    );
    return <Uint8List>[
      encodeDatagram(EnetDatagram(
        peerId: outgoingPeerId,
        // Only when something needs acknowledging: the sent time is what a
        // round trip is measured from, and the capture leaves it off
        // unsequenced datagrams for exactly that reason.
        sentTime: needsTime ? nowMs & 0xFFFF : null,
        commands: commands,
      )),
    ];
  }

  /// Takes one datagram apart and returns what should reach the application.
  List<EnetDelivery> receive(final EnetDatagram datagram, final int nowMs) {
    final List<EnetDelivery> delivered = <EnetDelivery>[];
    for (final EnetPacketCommand command in datagram.commands) {
      if (command.flags & commandFlagAcknowledge != 0) {
        // Acknowledged with the sent time we were told, so the far side can
        // measure the round trip without having kept a clock per packet.
        _pendingAcks.add((
          channel: command.channel,
          sequence: command.reliableSequenceNumber,
          sentTime: datagram.sentTime ?? 0,
        ));
      }
      switch (command.command) {
        case EnetCommand.acknowledge:
          _onAcknowledge(command, nowMs);
        case EnetCommand.sendReliable:
          delivered.addAll(_onReliable(command));
        case EnetCommand.sendUnsequenced:
          final EnetDelivery? one = _onUnsequenced(command);
          if (one != null) delivered.add(one);
        case EnetCommand.disconnect:
          _lost = true;
        default:
          // connect, verifyConnect and ping are the host's business, and
          // bandwidth and throttle commands are advisory. Acknowledged above
          // when they ask for it, and otherwise nothing to do.
          break;
      }
    }
    return delivered;
  }

  void _onAcknowledge(final EnetPacketCommand command, final int nowMs) {
    final int sequence = command.fields.isEmpty ? 0 : command.fields.first;
    final int sentTime = command.fields.length > 1 ? command.fields[1] : 0;
    final int before = _unacked.length;
    _unacked.removeWhere((final _Unacked pending) =>
        pending.channel == command.channel && pending.sequence == sequence);
    if (_unacked.length == before) return;

    // The round trip, from the time we stamped on the packet being answered.
    final int sample = (nowMs & 0xFFFF) - sentTime;
    if (sample >= 0 && sample < 30000) {
      // The usual smoothing: a quarter of the new sample, and a variance that
      // widens when samples disagree so a jittery link is not treated as a
      // fast one.
      roundTripVariance =
          (3 * roundTripVariance + (roundTripTime - sample).abs()) ~/ 4;
      roundTripTime = (7 * roundTripTime + sample) ~/ 8;
    }
  }

  List<EnetDelivery> _onReliable(final EnetPacketCommand command) {
    final Uint8List? payload = command.payload;
    if (payload == null) return const <EnetDelivery>[];
    final _Channel state = _channel(command.channel);
    final int sequence = command.reliableSequenceNumber;

    // Already delivered: a retransmission whose acknowledgement went missing.
    // Acknowledged again above, and dropped here rather than delivered twice.
    if (sequence <= state.incoming) return const <EnetDelivery>[];
    if (sequence > state.incoming + 1) {
      // A gap ahead of it. Held rather than delivered, because a reliable
      // channel is ordered and the application is entitled to assume it.
      state.held[sequence] = payload;
      return const <EnetDelivery>[];
    }

    final List<EnetDelivery> out = <EnetDelivery>[
      (channel: command.channel, payload: payload),
    ];
    state.incoming = sequence;
    // The gap is filled, so anything that was waiting behind it can go now.
    while (state.held.containsKey(state.incoming + 1)) {
      state.incoming++;
      out.add((channel: command.channel, payload: state.held.remove(state.incoming)!));
    }
    return out;
  }

  EnetDelivery? _onUnsequenced(final EnetPacketCommand command) {
    final Uint8List? payload = command.payload;
    if (payload == null) return null;
    final int group = command.fields.isEmpty ? 0 : command.fields.first;
    // A duplicate, or one that has been overtaken. Dropped: applying an older
    // state packet after a newer one would move the world backwards, which is
    // the one thing an unsequenced channel must not do.
    if (group <= _incomingUnsequencedGroup) return null;
    _incomingUnsequencedGroup = group;
    return (channel: command.channel, payload: payload);
  }
}
