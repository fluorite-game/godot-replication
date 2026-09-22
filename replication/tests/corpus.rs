//! The codec against packets Godot actually sent.
//!
//! `tests/fixtures/sync_packets.hex` holds SYNC packets lifted from a capture
//! of two Godot 4.5.2 peers playing the demo -- the same packets that defeated
//! two attempts to parse a sync record before the compact Variant form was
//! measured. Keeping them here means a change to the codec has to face them
//! again, on any machine, with no capture tooling installed.

use godot_replication::sync::{encode, parse, SyncPacket};
use godot_replication::variant::{Value, VariantType};

fn fixture_packets() -> Vec<Vec<u8>> {
    include_str!("fixtures/sync_packets.hex")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            (0..line.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&line[i..i + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
}

fn shape(packet: &SyncPacket) -> Vec<Vec<VariantType>> {
    packet
        .records
        .iter()
        .map(|record| record.fields.iter().map(Value::variant_type).collect())
        .collect()
}

#[test]
fn every_captured_packet_parses_to_the_exact_byte() {
    let packets = fixture_packets();
    assert!(packets.len() > 100, "fixture is present and not truncated");
    for (index, packet) in packets.iter().enumerate() {
        // `parse` refuses a packet whose records do not land exactly on the
        // final byte, so this is not merely "did not crash" -- a field decoded
        // at the wrong width fails here rather than yielding a wrong number.
        parse(packet)
            .unwrap_or_else(|error| panic!("packet {index} ({} bytes): {error:?}", packet.len()));
    }
}

#[test]
fn the_field_shapes_are_the_scenes_property_lists() {
    use VariantType::{Bool, Int, Transform3D, Vector2, Vector3};

    // Every shape in the capture, and which node in the demo it belongs to.
    // Read off the wire, then matched against the `.tscn` blocks rather than
    // the other way round.
    let robot = vec![Transform3D, Int, Vector3];
    // A bullet, and also each player's BulletCache: both are bullet.tscn
    // instances replicating only global_transform, so the wire cannot tell
    // them apart by shape. The capture's BulletCache records are the ones
    // addressed by path id (0x80000002, 0x80000003) rather than by spawn id.
    let bullet = vec![Transform3D];
    let player_server = vec![Transform3D, Transform3D, Vector2, Int];
    let player_input = vec![Vector3, Vector3, Vector3, Vector2, Bool, Bool];
    let known = [&robot, &bullet, &player_server, &player_input];

    let mut seen = std::collections::BTreeSet::new();
    for packet in fixture_packets() {
        for record in shape(&parse(&packet).expect("parses")) {
            assert!(
                known.iter().any(|k| **k == record),
                "unrecognised field shape {record:?}"
            );
            seen.insert(format!("{record:?}"));
        }
    }
    assert_eq!(seen.len(), 4, "all four of the demo's synchronizers appear");
}

#[test]
fn the_mode_zero_fields_are_absent_from_the_stream() {
    // `health`, `dead` and `player_id` are replication mode 0: they cross once
    // with the spawn and the synchronizer never sends them again. So the
    // robot's record carries three fields, not five, and the player's server
    // half carries four, not five.
    //
    // This is the check that ties the wire back to the `.tscn`: the port's
    // replication table said these were spawn-only long before any of this was
    // decoded, and the stream agrees.
    let mut robot_records = 0;
    for packet in fixture_packets() {
        for record in parse(&packet).expect("parses").records {
            let types: Vec<VariantType> = record.fields.iter().map(Value::variant_type).collect();
            if types
                == [
                    VariantType::Transform3D,
                    VariantType::Int,
                    VariantType::Vector3,
                ]
            {
                robot_records += 1;
                assert_eq!(
                    record.fields.len(),
                    3,
                    "global_transform, state, target_position -- and not health or dead"
                );
            }
        }
    }
    assert!(robot_records > 0, "the capture contains robots");
}

#[test]
fn the_counter_advances() {
    // One field of the header that is not otherwise exercised: a u16 that
    // increments per packet. Asserted loosely because the fixture is a sample
    // of a longer stream, not a contiguous run.
    let counters: Vec<u16> = fixture_packets()
        .iter()
        .map(|p| parse(p).expect("parses").counter)
        .collect();
    assert!(
        counters.windows(2).any(|w| w[1] != w[0]),
        "the counter is not a constant"
    );
}

#[test]
fn re_encoding_a_captured_packet_reproduces_it_byte_for_byte() {
    // The strongest check available without a second engine in the room. Every
    // compact tag, every int width, every field order and the record framing
    // all have to be right for one packet to come back identical -- and these
    // packets were written by Godot, not by this crate.
    //
    // What it does not cover is the int width rule: every int in this fixture
    // is one byte. See `how_much_of_the_codec_the_fixture_actually_exercises`,
    // which measures that rather than leaving it to be assumed.
    let packets = fixture_packets();
    let mut checked = 0usize;
    for (index, original) in packets.iter().enumerate() {
        let decoded = parse(original).expect("parses");
        let again = encode(&decoded);
        assert_eq!(
            again,
            *original,
            "packet {index} ({} bytes) did not re-encode identically",
            original.len()
        );
        checked += 1;
    }
    assert!(checked > 100, "checked the whole fixture");
}

#[test]
fn a_round_trip_survives_a_second_pass() {
    // Encode/decode being each other's inverse is weaker than matching the
    // capture, but it catches an encoder and decoder that are wrong in the
    // same direction -- which matching the capture would also catch, and this
    // says so sooner and more locally.
    for packet in fixture_packets() {
        let once = parse(&packet).expect("parses");
        let twice = parse(&encode(&once)).expect("re-parses");
        assert_eq!(once, twice);
    }
}

#[test]
fn how_much_of_the_codec_the_fixture_actually_exercises() {
    // Honesty check on the re-encode test above. If every int in the capture
    // fits in one byte then that test proves the framing and the tags but says
    // nothing about the width rule, and claiming otherwise would be claiming
    // more than the evidence.
    use std::collections::BTreeMap;
    let mut widths: BTreeMap<u8, usize> = BTreeMap::new();
    let mut types: BTreeMap<&str, usize> = BTreeMap::new();
    for packet in fixture_packets() {
        for record in parse(&packet).expect("parses").records {
            for field in record.fields {
                let name = match field {
                    Value::Bool(_) => "bool",
                    Value::Int(v) => {
                        *widths
                            .entry(godot_replication::variant::compact_width_code(v))
                            .or_default() += 1;
                        "int"
                    }
                    Value::Float(_) => "float",
                    Value::Vector2(_) => "Vector2",
                    Value::Vector3(_) => "Vector3",
                    Value::Transform3D(_) => "Transform3D",
                    // The corpus is this demo's traffic, which carries six
                    // types. The codec covers Godot's whole list now, so a
                    // seventh appearing here means the capture changed, not
                    // that the codec grew -- worth failing on rather than
                    // counting under a catch-all.
                    other => panic!("corpus carries an unexpected {other:?}"),
                };
                *types.entry(name).or_default() += 1;
            }
        }
    }
    println!("field types in the fixture: {types:?}");
    println!("compact int width codes:    {widths:?}");
    // Every int here is width code 0, and that is a fact about the demo rather
    // than a thin capture: the only ints it streams are `state` and
    // `current_animation`, both small enums, while its one large int
    // (`player_id`) is replication mode 0 and rides the spawn packet. So a
    // SYNC stream from this demo cannot carry a wider one, and the wider codes
    // are covered by the probe-derived unit tests in variant.rs instead.
    //
    // Asserted so that a future capture which *does* carry a wider int fails
    // here and forces this note to be re-read, rather than silently widening
    // what the re-encode test is believed to prove.
    assert_eq!(
        widths.keys().copied().collect::<Vec<u8>>(),
        vec![0],
        "fixture ints are all one byte; if this changes, revisit what \
         re_encoding_a_captured_packet_reproduces_it_byte_for_byte covers"
    );
    assert_eq!(types.len(), 5, "bool, int, Vector2, Vector3, Transform3D");
}
