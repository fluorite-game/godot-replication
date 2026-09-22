// SPDX-License-Identifier: Apache-2.0
//
// ENet's framing against datagrams Godot actually sent.
//
// The same bar the replication codec is held to, one layer down: every
// captured datagram parses, and re-encodes to the exact byte. Parsing alone
// would prove little here -- ENet packs commands back to back with no
// separator, so a command read at the wrong size swallows the next one's
// header and everything after it decodes to something plausible.

import 'dart:io';
import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:godot_replication/src/enet/protocol.dart';

/// The captured datagrams, with the port they came from.
List<({int source, Uint8List bytes})> capturedDatagrams() {
  final File file = File('test/enet_datagrams.hex');
  if (!file.existsSync()) throw StateError('missing ${file.path}');
  return <({int source, Uint8List bytes})>[
    for (final String line in file.readAsLinesSync())
      if (!line.startsWith('#') && line.trim().isNotEmpty)
        (
          source: int.parse(line.split(' ').first),
          bytes: Uint8List.fromList(<int>[
            for (final String hex in <String>[line.split(' ')[1]])
              for (int i = 0; i < hex.length; i += 2)
                int.parse(hex.substring(i, i + 2), radix: 16),
          ]),
        ),
  ];
}

void main() {
  test('every captured datagram round-trips to the exact byte', () {
    final List<({int source, Uint8List bytes})> captured = capturedDatagrams();
    expect(captured.length, greaterThan(100));
    for (int i = 0; i < captured.length; i++) {
      final EnetDatagram datagram = parseDatagram(captured[i].bytes);
      expect(encodeDatagram(datagram), captured[i].bytes, reason: 'datagram $i');
    }
  });

  test('the join is a connect answered by a verify connect', () {
    final List<({int source, Uint8List bytes})> captured = capturedDatagrams();
    final EnetPacketCommand connect =
        parseDatagram(captured.first.bytes).commands.single;
    expect(connect.command, EnetCommand.connect);
    expect(connect.channel, 0xFF, reason: 'ENets own channel');

    final EnetPacketCommand verify =
        parseDatagram(captured[1].bytes).commands.first;
    expect(verify.command, EnetCommand.verifyConnect);
    expect(captured[1].source, 4383, reason: 'the server answers');
  });

  test('the connects last field pair is the joining peers id', () {
    // Measured by `tools/enet_handshake_report.py`: the u32 at the end of a
    // CONNECT is the id Godot then names the client's player after. Here it
    // arrives as the last two u16 fields, because every field in an ENet
    // command is a u16.
    final EnetPacketCommand connect =
        parseDatagram(capturedDatagrams().first.bytes).commands.single;
    final int data =
        (connect.fields[connect.fields.length - 2] << 16) | connect.fields.last;
    expect(data, 466750851, reason: 'the id in this capture');
  });

  test('the two channels carry what the captures say they do', () {
    // Channel 0 reliable, channel 1 unsequenced, 255 ENet's own -- and
    // acknowledgements on *whichever channel they acknowledge*, which is the
    // part worth having measured. An ack is not traffic on a control channel;
    // it names the sequence number of a packet on a particular channel, so it
    // travels with that channel's id. Channel 1 has none, because an
    // unsequenced packet is never acknowledged.
    final Map<int, Set<String>> byChannel = <int, Set<String>>{};
    for (final ({int source, Uint8List bytes}) row in capturedDatagrams()) {
      for (final EnetPacketCommand command in parseDatagram(row.bytes).commands) {
        byChannel
            .putIfAbsent(command.channel, () => <String>{})
            .add(command.command.name);
      }
    }
    expect(byChannel[0], <String>{'sendReliable', 'acknowledge'});
    expect(byChannel[1], <String>{'sendUnsequenced'},
        reason: 'nothing acknowledges an unsequenced packet');
    expect(byChannel[255],
        <String>{'connect', 'verifyConnect', 'acknowledge', 'ping'});
  });

  test('an unsequenced command carries the unsequenced flag', () {
    // The flag and the command are separate things -- 0x40 on the command
    // byte, and the command id underneath it -- and a reader that masked only
    // the low nibble without noticing the flag would still decode, wrongly, as
    // a sequenced packet.
    bool seen = false;
    for (final ({int source, Uint8List bytes}) row in capturedDatagrams()) {
      for (final EnetPacketCommand command in parseDatagram(row.bytes).commands) {
        if (command.command != EnetCommand.sendUnsequenced) continue;
        expect(command.flags & commandFlagUnsequenced, commandFlagUnsequenced);
        seen = true;
      }
    }
    expect(seen, isTrue);
  });

  test('a compressed datagram is refused rather than guessed at', () {
    // No captured packet sets this flag, so there is nothing to check an
    // implementation of the range coder against. Refusing says so; decoding it
    // as uncompressed would produce commands out of noise.
    final Uint8List bytes = Uint8List.fromList(<int>[0x40, 0x01, 0x00, 0x00]);
    expect(() => parseDatagram(bytes), throwsFormatException);
  });

  test('a command claiming more payload than is present is refused', () {
    final Uint8List good = encodeDatagram(EnetDatagram(
      peerId: 1,
      commands: <EnetPacketCommand>[
        EnetPacketCommand(
          command: EnetCommand.sendReliable,
          flags: 0,
          channel: 0,
          reliableSequenceNumber: 1,
          fields: <int>[3],
          payload: Uint8List.fromList(<int>[1, 2, 3]),
        ),
      ],
    ));
    expect(parseDatagram(good).commands.single.payload, hasLength(3));

    final Uint8List bad = Uint8List.fromList(good)..[good.length - 4] = 0xFF;
    expect(() => parseDatagram(bad), throwsFormatException);
  });
}
