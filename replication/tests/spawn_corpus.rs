//! `SPAWN` against packets Godot actually sent.

use godot_replication::spawn::Spawn;

fn fixture() -> Vec<Vec<u8>> {
    include_str!("fixtures/spawn_packets.hex")
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
fn every_spawn_parses_and_re_encodes_identically() {
    let packets = fixture();
    assert_eq!(packets.len(), 112);
    for (index, packet) in packets.iter().enumerate() {
        let parsed = Spawn::parse(packet).unwrap_or_else(|e| panic!("packet {index}: {e:?}"));
        assert_eq!(&parsed.encode(), packet, "packet {index} changed");
    }
}

#[test]
fn synchronizer_ids_follow_the_node_id() {
    // Every spawned node and each synchronizer under it take consecutive ids.
    for packet in fixture() {
        let spawn = Spawn::parse(&packet).expect("parses");
        let expected: Vec<u32> = (1..=spawn.sync_ids.len())
            .map(|i| spawn.net_id + u32::try_from(i).expect("small"))
            .collect();
        assert_eq!(spawn.sync_ids, expected, "{}", spawn.name);
        // Every spawn comes from the same spawner, whose path-cache id
        // SIMPLIFY_PATH assigned as 1.
        assert_eq!(spawn.spawner, 1);
    }
}

#[test]
fn the_packet_once_stored_raw_is_the_hosts_player() {
    // The first reading of this layout left one packet undecoded. It is the
    // host's own player: the only node with two server-owned synchronizers.
    let hosts: Vec<Spawn> = fixture()
        .iter()
        .map(|p| Spawn::parse(p).expect("parses"))
        .filter(|s| s.sync_ids.len() == 2)
        .collect();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].name, "1");
    assert_eq!(hosts[0].scene, 0);
}

#[test]
fn state_size_is_the_spawn_properties_of_the_listed_synchronizers() {
    // Predicted from the `.tscn` field lists, compact-encoded:
    //   robot   73   transform + health + state + target + dead
    //   bullet  52   transform
    //   client  123  ServerSynchronizer only (id 466750851: 5 bytes)
    //   host    182  ServerSynchronizer with id 1 (2 bytes) = 120,
    //                plus InputSynchronizer 62
    for packet in fixture() {
        let s = Spawn::parse(&packet).expect("parses");
        let want = match (s.scene, s.sync_ids.len()) {
            (1, 1) => 73,
            (2, 1) => 52,
            (0, 1) => 123,
            (0, 2) => 182,
            other => panic!("unexpected spawn shape {other:?} for {}", s.name),
        };
        assert_eq!(s.state.len(), want, "{}", s.name);
    }
}

#[test]
fn scenes_line_up_with_the_spawnable_list() {
    for packet in fixture() {
        let s = Spawn::parse(&packet).expect("parses");
        match s.scene {
            0 => assert!(s.name.chars().all(|c| c.is_ascii_digit()), "{}", s.name),
            1 => assert!(s.name.starts_with("RedRobot"), "{}", s.name),
            2 => assert!(s.name.starts_with("Bullet"), "{}", s.name),
            other => panic!("scene {other}"),
        }
    }
}
