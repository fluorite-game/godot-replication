//! The path cache against packets Godot actually sent.
//!
//! Same discipline as `corpus.rs`: parse every captured packet, then re-encode
//! it and require the bytes back identically. The fixture mixes
//! `SIMPLIFY_PATH`, `CONFIRM_PATH` and `DESPAWN` deliberately -- a decoder dispatches on the first
//! byte, so it should be handed all three together rather than one kind at a
//! time.

use godot_replication::path::{
    encode_despawn, parse_despawn, ConfirmPath, SimplifyPath, COMMAND_CONFIRM_PATH,
    COMMAND_DESPAWN, COMMAND_SIMPLIFY_PATH,
};
use godot_replication::rpc::RpcConfig;

fn fixture() -> Vec<Vec<u8>> {
    include_str!("fixtures/path_packets.hex")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
}

#[test]
fn every_packet_parses_and_re_encodes_identically() {
    let packets = fixture();
    assert!(packets.len() > 50, "fixture present");
    let mut counts = [0usize; 3];
    for (index, packet) in packets.iter().enumerate() {
        let again = match packet[0] {
            COMMAND_SIMPLIFY_PATH => {
                counts[0] += 1;
                SimplifyPath::parse(packet)
                    .unwrap_or_else(|e| panic!("packet {index}: {e:?}"))
                    .encode()
            }
            COMMAND_CONFIRM_PATH => {
                counts[1] += 1;
                ConfirmPath::parse(packet)
                    .unwrap_or_else(|e| panic!("packet {index}: {e:?}"))
                    .encode()
            }
            COMMAND_DESPAWN => {
                counts[2] += 1;
                encode_despawn(
                    parse_despawn(packet).unwrap_or_else(|e| panic!("packet {index}: {e:?}")),
                )
            }
            other => panic!("packet {index} has command {other:#04x}"),
        };
        assert_eq!(
            &again, packet,
            "packet {index} did not re-encode identically"
        );
    }
    assert!(
        counts.iter().all(|&n| n > 0),
        "all three kinds present: {counts:?}"
    );
}

#[test]
fn each_cached_path_hashes_to_the_script_that_node_runs() {
    // The strongest cross-check between the two halves of this crate. `rpc`
    // computes a hash from a list of method names taken out of the engine;
    // these packets carry a hash Godot computed for a node it named. They
    // should agree, node by node -- and they do:
    //
    //     466750851 (a player, named after its peer id)  player.gd
    //     Bullet, Bullet2, Bullet3, Bullet4, BulletCache bullet.gd
    //     InputSynchronizer                              player_input.gd
    //     MultiplayerSpawner, MultiplayerSynchronizer    no RPCs at all
    //
    // Four of the five predicted hashes are confirmed here against live
    // traffic *with the node that carries the script*, which is a good deal
    // stronger than finding the digest somewhere in a capture. The fifth,
    // part.gd's, needs a robot to die.
    let player = RpcConfig::new(["jump", "land", "shoot", "hit", "add_camera_shake_trauma"]).hash();
    let player_input = RpcConfig::new(["jump"]).hash();
    let bullet = RpcConfig::new(["explode"]).hash();
    let none = RpcConfig::default().hash();

    let mut seen = std::collections::BTreeSet::new();
    for packet in fixture() {
        if packet[0] != COMMAND_SIMPLIFY_PATH {
            continue;
        }
        let announced = SimplifyPath::parse(&packet).expect("parses");
        assert!(
            announced.path.starts_with("main/Level"),
            "every cached path is in the level: {}",
            announced.path
        );
        let leaf = announced.path.rsplit('/').next().expect("non-empty");
        let expected = if leaf.starts_with("Bullet") {
            &bullet
        } else if leaf == "InputSynchronizer" {
            &player_input
        } else if leaf.chars().all(|c| c.is_ascii_digit()) {
            // level.gd:112 names a spawned player after its peer id.
            &player
        } else {
            // MultiplayerSpawner and MultiplayerSynchronizer carry no script.
            &none
        };
        assert_eq!(
            &announced.rpc_hash, expected,
            "{} hashed to something its script does not explain",
            announced.path
        );
        seen.insert(announced.rpc_hash.clone());
    }
    assert_eq!(seen.len(), 4, "four distinct configs appear: {seen:?}");
}

#[test]
fn ids_are_handed_out_in_order_from_one() {
    let mut ids: Vec<u32> = fixture()
        .iter()
        .filter(|p| p[0] == COMMAND_SIMPLIFY_PATH)
        .map(|p| SimplifyPath::parse(p).expect("parses").id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.first(), Some(&1), "the first id is 1, not 0");
    // Contiguous: the sender allocates the next integer each time, which is
    // what lets a receiver size its table by the largest id it has seen.
    assert_eq!(
        ids.last().copied(),
        u32::try_from(ids.len()).ok(),
        "ids run 1..=n with no gaps"
    );
}

#[test]
fn a_wrong_command_byte_is_refused() {
    // Dispatching on the first byte is the caller's job, and a parser that
    // accepted anything would turn a misrouted packet into plausible nonsense.
    assert!(SimplifyPath::parse(&[COMMAND_DESPAWN, 0, 0, 0, 0]).is_err());
    assert!(ConfirmPath::parse(&[COMMAND_SIMPLIFY_PATH]).is_err());
    assert!(parse_despawn(&[COMMAND_CONFIRM_PATH, 1, 0, 0, 0, 0]).is_err());
}
