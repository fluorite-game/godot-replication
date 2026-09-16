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
fn every_spawn_re_encodes_identically_including_the_one_not_understood() {
    // The raw variant has to survive this too. A form the crate cannot read is
    // still a form it must not corrupt -- a relay that mangles what it does
    // not understand is worse than one that refuses it.
    let packets = fixture();
    assert_eq!(packets.len(), 112);
    for (index, packet) in packets.iter().enumerate() {
        let parsed = Spawn::parse(packet).unwrap_or_else(|e| panic!("packet {index}: {e:?}"));
        assert_eq!(&parsed.encode(), packet, "packet {index} changed");
    }
}

#[test]
fn the_names_and_scenes_line_up_with_the_spawnable_list() {
    // `level.tscn:75` gives the spawner three scenes, and the scene byte
    // indexes that array. Which index is which was read off the names.
    let mut by_scene: std::collections::BTreeMap<u8, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut raw = 0;
    for packet in fixture() {
        match Spawn::parse(&packet).expect("parses") {
            Spawn::Named {
                scene,
                spawner,
                name,
                ..
            } => {
                // Every spawn comes from the same spawner, whose path-cache id
                // SIMPLIFY_PATH assigned as 1.
                assert_eq!(spawner, 1);
                by_scene.entry(scene).or_default().push(name);
            }
            Spawn::Raw { .. } => raw += 1,
        }
    }
    assert_eq!(raw, 1, "exactly one packet is the undecoded form");
    assert!(by_scene[&1].iter().all(|n| n.starts_with("RedRobot")));
    assert!(by_scene[&2].iter().all(|n| n.starts_with("Bullet")));
    // Scene 0 is the player, named after its peer id by level.gd:112.
    assert!(by_scene[&0]
        .iter()
        .all(|n| n.chars().all(|c| c.is_ascii_digit())));
}

#[test]
fn the_state_size_is_the_spawn_properties_of_that_scene() {
    // The check that says the state really is compact Variants of the
    // `spawn = true` list, without decoding it: each scene's blob is a fixed
    // size, and that size is what the field list predicts.
    //
    //   robot   transform 52 + health 2 + state 2 + target 16 + dead 1 = 73
    //   bullet  transform 52                                          = 52
    //   player  transform 52 + id 5 + model 52 + motion 12 + anim 2    = 123
    //
    // health and dead are here and never in a SYNC packet, which is what
    // replication mode 0 means seen from this side.
    let expected = [(0u8, 123usize), (1, 73), (2, 52)];
    for packet in fixture() {
        if let Spawn::Named { scene, state, .. } = Spawn::parse(&packet).expect("parses") {
            let want = expected
                .iter()
                .find(|(s, _)| *s == scene)
                .map(|(_, n)| *n)
                .expect("a known scene");
            assert_eq!(state.len(), want, "scene {scene} state size");
        }
    }
}
