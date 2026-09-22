//! RPC calls that carry arguments, against bytes a stock engine produced.
//!
//! `tests/fixtures/rpc_arguments.hex` is the output of `oracle/rpc_oracle.gd`:
//! a `MultiplayerPeerExtension` installed under a stock `SceneMultiplayer`, so
//! the engine encodes each call exactly as it would for a socket and hands the
//! bytes to the oracle instead of sending them.
//!
//! No captured packet carries an argument list -- the game this protocol was
//! measured from never calls one -- so without the oracle every assertion here
//! would be checking the implementation against itself.

use godot_replication::rpc::{CallError, RemoteCall};
use godot_replication::variant::Value;

struct Sample {
    label: String,
    bytes: Vec<u8>,
}

fn samples() -> Vec<Sample> {
    include_str!("fixtures/rpc_arguments.hex")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let (label, hex) = line.split_once('\t').expect("label and hex");
            Sample {
                label: label.to_string(),
                bytes: (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                    .collect(),
            }
        })
        .collect()
}

/// The fixture opens with the `SIMPLIFY_PATH` the engine sent before it could
/// address anything, which is a path packet and not a call.
fn calls() -> Vec<Sample> {
    samples()
        .into_iter()
        .filter(|s| s.bytes.first() != Some(&0x01))
        .collect()
}

#[test]
fn every_call_the_engine_encoded_parses() {
    for s in calls() {
        RemoteCall::parse(&s.bytes).unwrap_or_else(|e| panic!("{}: {e:?}", s.label));
    }
}

#[test]
fn re_encoding_reproduces_the_engine_bytes() {
    for s in calls() {
        let call = RemoteCall::parse(&s.bytes).expect("parses");
        assert_eq!(
            call.encode(),
            s.bytes,
            "{}: re-encoded differently\n  was {:02x?}",
            s.label,
            s.bytes
        );
    }
}

#[test]
fn the_lead_byte_is_two_flags_over_one_command() {
    // 0x20 says the target travels as a path, 0x80 says there are no
    // arguments. Asserted against the fixture rather than the enum, because
    // the whole point is that the engine chose these and this crate did not.
    for s in calls() {
        let lead = s.bytes[0];
        let call = RemoteCall::parse(&s.bytes).expect("parses");
        let by_path = matches!(call, RemoteCall::Path { .. });
        let no_args = match &call {
            RemoteCall::Path { args, .. } | RemoteCall::Cached { args, .. } => args.is_empty(),
        };
        assert_eq!(by_path, lead & 0x20 != 0, "{}: path bit", s.label);
        assert_eq!(no_args, lead & 0x80 != 0, "{}: no-args bit", s.label);
    }
}

#[test]
fn the_long_forms_offset_is_where_the_path_actually_starts() {
    // The field exists because the path sits after the arguments, so its
    // value moves with them. A decoder that kept the no-argument constant
    // would read the path out of the middle of an argument.
    for s in calls() {
        if s.bytes[0] & 0x20 == 0 {
            continue;
        }
        let field = u32::from_le_bytes(s.bytes[1..5].try_into().expect("four bytes"));
        let offset = (field & 0x7fff_ffff) as usize;
        let tail = &s.bytes[offset..];
        let end = tail.iter().position(|&b| b == 0).expect("terminated");
        let path = std::str::from_utf8(&tail[..end]).expect("utf8");
        assert_eq!(path, "RpcOracle", "{}: path at offset {offset}", s.label);
    }
}

#[test]
fn the_arguments_are_the_values_that_were_passed() {
    let by_label: std::collections::HashMap<String, RemoteCall> = calls()
        .into_iter()
        .map(|s| (s.label, RemoteCall::parse(&s.bytes).expect("parses")))
        .collect();
    let args = |label: &str| -> Vec<Value> {
        match by_label
            .get(label)
            .unwrap_or_else(|| panic!("{label} missing"))
        {
            RemoteCall::Path { args, .. } | RemoteCall::Cached { args, .. } => args.clone(),
        }
    };

    assert_eq!(args("no_args"), vec![]);
    assert_eq!(args("one_int 0"), vec![Value::Int(0)]);
    assert_eq!(args("one_int -1"), vec![Value::Int(-1)]);
    // Wide enough that the compact form has to widen with it.
    assert_eq!(args("one_int 2^33"), vec![Value::Int(8_589_934_592)]);
    assert_eq!(
        args("one_string ascii"),
        vec![Value::Str {
            kind: godot_replication::variant::VariantType::String,
            text: "shoot".to_string(),
        }]
    );
    assert_eq!(
        args("two_vectors"),
        vec![
            Value::Vector3([1.0, -2.0, 3.0]),
            Value::Vector3([-4.0, 5.0, -6.0]),
        ]
    );
    assert_eq!(
        args("three_mixed"),
        vec![
            Value::Int(5),
            Value::Str {
                kind: godot_replication::variant::VariantType::String,
                text: "hit".to_string(),
            },
            Value::Bool(true),
        ]
    );
    // The same call once the receiver confirmed the path cache: a different
    // lead byte and no path, and the arguments have to survive that.
    assert_eq!(args("cached three_mixed"), args("three_mixed"));
}

#[test]
fn a_three_hundred_byte_argument_needs_the_offset_to_be_a_word() {
    // The reason the offset is four bytes. One argument here is longer than a
    // byte can address, so a decoder that truncated the field would look for
    // the path 256 bytes early.
    let s = calls()
        .into_iter()
        .find(|s| s.label == "one_string 300 bytes")
        .expect("sample");
    let call = RemoteCall::parse(&s.bytes).expect("parses");
    let RemoteCall::Path { args, path, .. } = &call else {
        panic!("expected the long form");
    };
    assert_eq!(path, "RpcOracle");
    let [Value::Str { text, .. }] = args.as_slice() else {
        panic!("expected one string");
    };
    assert_eq!(text.len(), 300);
}

#[test]
fn an_offset_that_disagrees_with_the_arguments_is_refused() {
    // Reading the path from the offset regardless would turn a disagreement
    // between the two halves into a call with the wrong arguments, which is
    // the failure this protocol is worst at reporting.
    let mut bytes = calls()
        .into_iter()
        .find(|s| s.label == "three_mixed")
        .expect("sample")
        .bytes;
    bytes[1] = bytes[1].wrapping_add(1);
    assert!(matches!(
        RemoteCall::parse(&bytes),
        Err(CallError::UnexpectedOffset { .. })
    ));
}
