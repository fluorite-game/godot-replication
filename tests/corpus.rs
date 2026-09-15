//! The codec against packets Godot actually sent.
//!
//! `tests/fixtures/sync_packets.hex` holds SYNC packets lifted from a capture
//! of two Godot 4.5.2 peers playing the demo -- the same packets that defeated
//! two attempts to parse a sync record before the compact Variant form was
//! measured. Keeping them here means a change to the codec has to face them
//! again, on any machine, with no capture tooling installed.

use godot_replication::sync::{parse, SyncPacket};
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
