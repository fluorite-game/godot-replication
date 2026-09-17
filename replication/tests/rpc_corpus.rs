//! Remote calls against packets Godot actually sent.

use godot_replication::rpc::{RemoteCall, RpcConfig, LEAD_CACHED, LEAD_PATH};

fn fixture() -> Vec<Vec<u8>> {
    include_str!("fixtures/rpc_packets.hex")
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
fn every_call_re_encodes_identically() {
    let packets = fixture();
    assert_eq!(packets.len(), 214);
    for (index, packet) in packets.iter().enumerate() {
        let call = RemoteCall::parse(packet).unwrap_or_else(|e| panic!("packet {index}: {e:?}"));
        assert_eq!(&call.encode(), packet, "packet {index} changed");
    }
}

#[test]
fn the_calls_name_the_methods_the_capture_should_contain() {
    // The whole crate meeting in one assertion. `rpc`'s sorted-name rule turns
    // a method id into a name; the path cache says which node each id means;
    // and the counts have to match what a session of holding the trigger
    // actually does.
    let player = RpcConfig::new(["jump", "land", "shoot", "hit", "add_camera_shake_trauma"]);
    let bullet = RpcConfig::new(["explode"]);

    let mut named: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for packet in fixture() {
        match RemoteCall::parse(&packet).expect("parses") {
            // Cache id 5 is `main/Level/SpawnedNodes/466750851` -- the client's
            // own player -- as SIMPLIFY_PATH announced it.
            RemoteCall::Cached { cache_id, method } => {
                assert_eq!(cache_id, 5);
                *named
                    .entry(player.method_of(method.into()).expect("a player method"))
                    .or_default() += 1;
            }
            RemoteCall::Path { method, path, .. } => {
                // By leaf, not by an exact name. The long form addresses the
                // bullet *nodes* -- `Bullet`, `Bullet2`, `Bullet3`, `Bullet4`
                // -- far more often than the `BulletCache` they come from,
                // because each new bullet needs its own path announced. All
                // of them run `bullet.gd`.
                let leaf = path.rsplit('/').next().unwrap_or("");
                let config = if leaf.starts_with("Bullet") {
                    &bullet
                } else {
                    &player
                };
                *named
                    .entry(config.method_of(method.into()).expect("a known method"))
                    .or_default() += 1;
            }
        }
    }
    // One `shoot` per shot, one `explode` per bullet that landed (plus one
    // still in flight when the capture ended), and a single `land` from the
    // client's character touching the floor after it spawned.
    assert_eq!(named.get("shoot"), Some(&106));
    assert_eq!(named.get("explode"), Some(&107));
    assert_eq!(named.get("land"), Some(&1));
    assert_eq!(named.len(), 3, "no other method was called: {named:?}");
}

#[test]
fn both_addressing_forms_appear_and_are_told_apart_by_the_lead_byte() {
    let packets = fixture();
    let cached = packets.iter().filter(|p| p[0] == LEAD_CACHED).count();
    let with_path = packets.iter().filter(|p| p[0] == LEAD_PATH).count();
    assert_eq!((cached, with_path), (106, 108));
    // The long form is used while a path is still being announced and the
    // short one after, so a session that spawns and destroys bullets
    // constantly uses both in almost equal measure.
    assert!(packets
        .iter()
        .all(|p| p[0] == LEAD_CACHED || p[0] == LEAD_PATH));
}

#[test]
fn trailing_bytes_are_refused_rather_than_ignored() {
    // Nothing in this demo sends RPC arguments -- every method called with
    // `.rpc()` takes none, and the one that takes a float is only ever called
    // as a plain method. So argument encoding is unimplemented and untestable
    // here, and a packet carrying some must fail loudly rather than be
    // delivered as a call with the arguments dropped.
    assert!(RemoteCall::parse(&[LEAD_CACHED, 5, 4, 0xff]).is_err());
    let mut with_args = vec![LEAD_PATH, 6, 0, 0, 0x80, 0];
    with_args.extend_from_slice(b"main/Level\0");
    assert!(RemoteCall::parse(&with_args).is_ok());
    with_args.push(0xff);
    assert!(RemoteCall::parse(&with_args).is_err());
}

#[test]
fn the_long_forms_field_is_the_path_offset() {
    // 0x80000006 in every captured long call, whatever the target. A path id
    // would vary between the six different nodes these calls address.
    let mut paths = std::collections::BTreeSet::new();
    for packet in fixture().iter().filter(|p| p[0] == LEAD_PATH) {
        assert_eq!(&packet[1..5], &[6, 0, 0, 0x80]);
        if let RemoteCall::Path { path, .. } = RemoteCall::parse(packet).expect("parses") {
            // And the path really does start at byte 6.
            assert_eq!(&packet[6..6 + path.len()], path.as_bytes());
            paths.insert(path);
        }
    }
    assert_eq!(paths.len(), 6);
    // A different offset would mean something sits between method and path.
    let mut moved = vec![LEAD_PATH, 7, 0, 0, 0x80, 0, 0xff];
    moved.extend_from_slice(b"main/Level\0");
    assert!(RemoteCall::parse(&moved).is_err());
}
