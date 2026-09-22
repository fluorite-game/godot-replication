// SPDX-License-Identifier: Apache-2.0
//
// ENet's own framing, in Dart (plan.md DR-7, DR-7b).
//
// ## Why this is here rather than bound
//
// DR-7 said to bind upstream ENet rather than reimplement it, on the grounds
// that the transport would then be compatible by construction. DR-7b reverses
// that, because the captures narrowed the job to a fraction of what ENet is:
//
//   * nothing is compressed -- the COMPRESSED header flag is set on 0 of
//     17266 captured packets, so the range coder is not in the picture;
//   * nothing is fragmented -- the largest packet the demo sends is 1057
//     bytes against an MTU of 1392, so reassembly is not either;
//   * two channels are used, 0 reliable and 1 unsequenced;
//   * the handshake is one command carrying one number.
//
// The parts of ENet that are genuinely difficult are the parts this demo
// provably never exercises. What is left -- a connect exchange, reliable
// ordered delivery with acknowledgements, and unsequenced delivery -- is a
// bounded protocol that can be measured from the same corpus as everything
// above it, and keeping it in Dart is what keeps the port buildable for every
// target without a per-platform artifact.
//
// The library is still useful: `libenet` on a development machine is an
// *oracle* to test against, in the same way stock Godot is the oracle for the
// replication protocol. It is not a dependency of anything that ships.
//
// ## The datagram
//
//     [u16 peer id | flags][u16 sent time, if the flag says so][command...]
//
// The peer id's top two bits are flags -- 0x8000 a sent time follows, 0x4000
// compressed -- and bits 12-13 are a session id. Each command begins with a
// one-byte command, whose own top bits are flags (0x80 acknowledge, 0x40
// unsequenced), a channel, and a reliable sequence number.
//
// Every field is **big-endian**, which is worth stating once: the payloads
// these commands carry are Godot's, and those are little-endian throughout.

import 'dart:typed_data';

/// The ENet commands this demo uses. The others exist; none of them appear.
enum EnetCommand {
  none(0, 0),
  acknowledge(1, 8),
  connect(2, 48),
  verifyConnect(3, 44),
  disconnect(4, 8),
  ping(5, 4),
  sendReliable(6, 6),
  sendUnreliable(7, 8),
  sendFragment(8, 24),
  sendUnsequenced(9, 8),
  bandwidthLimit(10, 12),
  throttleConfigure(11, 12),
  sendUnreliableFragment(12, 24);

  const EnetCommand(this.id, this.size);

  /// The wire value, in the low four bits of the command byte.
  final int id;

  /// How many bytes the command's own fields take, its header included.
  final int size;

  static EnetCommand? byId(final int id) {
    for (final EnetCommand command in EnetCommand.values) {
      if (command.id == id) return command;
    }
    return null;
  }

  /// Whether a length and a payload follow the command's fields.
  bool get carriesData => const <EnetCommand>{
        EnetCommand.sendReliable,
        EnetCommand.sendUnreliable,
        EnetCommand.sendFragment,
        EnetCommand.sendUnsequenced,
        EnetCommand.sendUnreliableFragment,
      }.contains(this);
}

/// A sent time follows the peer id.
const int headerFlagSentTime = 0x8000;

/// The datagram is range-coded. Never set in any captured packet, and refused
/// here rather than silently mis-parsed.
const int headerFlagCompressed = 0x4000;

const int headerSessionMask = 0x3000;
const int headerSessionShift = 12;

/// This command is an acknowledgement.
const int commandFlagAcknowledge = 0x80;

/// This command is unsequenced.
const int commandFlagUnsequenced = 0x40;

/// One command out of a datagram.
class EnetPacketCommand {
  const EnetPacketCommand({
    required this.command,
    required this.flags,
    required this.channel,
    required this.reliableSequenceNumber,
    this.fields = const <int>[],
    this.payload,
  });

  final EnetCommand command;

  /// [commandFlagAcknowledge] and [commandFlagUnsequenced].
  final int flags;

  final int channel;
  final int reliableSequenceNumber;

  /// The command's own u16 fields, in order, after the four-byte header.
  ///
  /// Kept as numbers rather than a struct per command because every command
  /// this demo uses is a short run of `u16`s and one optional length -- naming
  /// thirteen structs to hold at most five numbers each would be more code
  /// saying less.
  final List<int> fields;

  /// What a data-carrying command carries.
  final Uint8List? payload;
}

/// One datagram: a header and the commands in it.
class EnetDatagram {
  const EnetDatagram({
    required this.peerId,
    required this.commands,
    this.sentTime,
    this.session = 0,
  });

  /// The id the *receiver* knows this connection by, as told to us in the
  /// connect exchange. Not the Godot peer id, which is a layer up.
  final int peerId;

  /// Present when the header says so. ENet uses it for round-trip timing.
  final int? sentTime;

  final int session;
  final List<EnetPacketCommand> commands;
}

/// Reads one datagram.
///
/// Throws rather than returning what it managed: ENet packs commands
/// back to back with no separator, so a command read at the wrong size runs
/// the next one's header together with its own body, and everything after is
/// plausible and wrong.
EnetDatagram parseDatagram(final Uint8List bytes) {
  if (bytes.length < 2) {
    throw const FormatException('datagram has no header');
  }
  final ByteData view = ByteData.sublistView(bytes);
  final int head = view.getUint16(0, Endian.big);
  if (head & headerFlagCompressed != 0) {
    // Not implemented, and not guessed at either: no captured packet has this
    // set, so there is nothing to check an implementation against.
    throw const FormatException('compressed datagrams are not supported');
  }
  final bool timed = head & headerFlagSentTime != 0;
  int at = 2;
  int? sentTime;
  if (timed) {
    if (bytes.length < 4) {
      throw const FormatException('header claims a sent time and has none');
    }
    sentTime = view.getUint16(2, Endian.big);
    at = 4;
  }

  final List<EnetPacketCommand> commands = <EnetPacketCommand>[];
  while (at < bytes.length) {
    if (at + 4 > bytes.length) {
      throw FormatException('command header at $at is cut short');
    }
    final int lead = bytes[at];
    final EnetCommand? command = EnetCommand.byId(lead & 0x0F);
    if (command == null || command == EnetCommand.none) {
      throw FormatException('unknown ENet command ${lead & 0x0F} at $at');
    }
    if (at + command.size > bytes.length) {
      throw FormatException('${command.name} at $at is cut short');
    }
    final int channel = bytes[at + 1];
    final int sequence = view.getUint16(at + 2, Endian.big);
    // Everything between the header and the payload is u16 fields; a
    // data-carrying command's last one is the payload length.
    final List<int> fields = <int>[
      for (int i = 4; i + 1 < command.size; i += 2)
        view.getUint16(at + i, Endian.big),
    ];
    Uint8List? payload;
    int next = at + command.size;
    if (command.carriesData) {
      final int length = fields.last;
      if (next + length > bytes.length) {
        throw FormatException(
          '${command.name} at $at claims $length bytes, '
          '${bytes.length - next} left',
        );
      }
      payload = Uint8List.sublistView(bytes, next, next + length);
      next += length;
    }
    commands.add(EnetPacketCommand(
      command: command,
      flags: lead & (commandFlagAcknowledge | commandFlagUnsequenced),
      channel: channel,
      reliableSequenceNumber: sequence,
      fields: fields,
      payload: payload,
    ));
    at = next;
  }
  return EnetDatagram(
    peerId: head & ~(headerFlagSentTime | headerFlagCompressed | headerSessionMask),
    sentTime: sentTime,
    session: (head & headerSessionMask) >> headerSessionShift,
    commands: commands,
  );
}

/// Writes one datagram.
Uint8List encodeDatagram(final EnetDatagram datagram) {
  final BytesBuilder out = BytesBuilder(copy: false);
  final int head = datagram.peerId |
      (datagram.session << headerSessionShift) |
      (datagram.sentTime != null ? headerFlagSentTime : 0);
  final ByteData header = ByteData(datagram.sentTime != null ? 4 : 2)
    ..setUint16(0, head, Endian.big);
  if (datagram.sentTime != null) {
    header.setUint16(2, datagram.sentTime!, Endian.big);
  }
  out.add(header.buffer.asUint8List());

  for (final EnetPacketCommand command in datagram.commands) {
    final ByteData body = ByteData(command.command.size)
      ..setUint8(0, command.command.id | command.flags)
      ..setUint8(1, command.channel)
      ..setUint16(2, command.reliableSequenceNumber, Endian.big);
    for (int i = 0; i < command.fields.length; i++) {
      body.setUint16(4 + i * 2, command.fields[i], Endian.big);
    }
    out.add(body.buffer.asUint8List());
    final Uint8List? payload = command.payload;
    if (payload != null) out.add(payload);
  }
  return out.takeBytes();
}
