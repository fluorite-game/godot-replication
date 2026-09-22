//! Every Variant type, against bytes the engine wrote.
//!
//! `tests/fixtures/variant_types.hex` is the output of `oracle/variant_oracle.gd`
//! run under Godot 4.5.2: two samples per type, one at its zero and one whose
//! every field is distinct. A codec written against zeroed samples passes by
//! returning zeroes, and one written against symmetric values passes with its
//! axes swapped; both have happened in this protocol.
//!
//! The assertions are deliberately blunt. Decoding has to consume exactly the
//! bytes the engine produced -- not fewer, which would leave a tail that the
//! next field in a packet would read as its own header -- and re-encoding has
//! to reproduce them.

use godot_replication::variant::{decode, encode, VariantType};

/// One line of the fixture: what it was called, what the engine said its type
/// id was, and the bytes.
struct Sample {
    label: String,
    id: u16,
    bytes: Vec<u8>,
}

fn samples() -> Vec<Sample> {
    include_str!("fixtures/variant_types.hex")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.split('\t');
            let label = parts.next().expect("label").to_string();
            let id = parts.next().expect("id").parse().expect("id is a number");
            let hex = parts.next().expect("hex");
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
                .collect();
            Sample { label, id, bytes }
        })
        .collect()
}

#[test]
fn the_oracle_covers_every_type_the_crate_claims() {
    let mut seen: Vec<u16> = samples().iter().map(|s| s.id).collect();
    seen.sort_unstable();
    seen.dedup();
    // Walk the id space rather than the enum, so a type added to `from_id`
    // without a sample fails here instead of shipping untested.
    for id in 0..=38u16 {
        let known = VariantType::from_id(id).is_some();
        let covered = seen.contains(&id);
        assert_eq!(
            known,
            covered,
            "id {id} is {} by the crate and {} by the oracle",
            if known { "handled" } else { "refused" },
            if covered { "sampled" } else { "unsampled" },
        );
    }
}

#[test]
fn every_sample_decodes_to_the_type_the_engine_labelled_it() {
    for s in samples() {
        let (value, _) = decode(&s.bytes, 0).unwrap_or_else(|e| panic!("{}: {e:?}", s.label));
        assert_eq!(
            value.variant_type().id(),
            s.id,
            "{}: decoded as {:?}",
            s.label,
            value.variant_type()
        );
    }
}

#[test]
fn decoding_consumes_exactly_what_the_engine_wrote() {
    // A decoder that stops short leaves a tail, and in a packet the next
    // field's header is read from wherever this one stopped. Short by four
    // bytes is not a truncated value, it is a corrupted packet with no error
    // anywhere in it.
    for s in samples() {
        let (_, end) = decode(&s.bytes, 0).unwrap_or_else(|e| panic!("{}: {e:?}", s.label));
        assert_eq!(end, s.bytes.len(), "{}: consumed {end}", s.label);
    }
}

#[test]
fn re_encoding_reproduces_the_engine_bytes() {
    // The one documented exception is padding. Godot pads strings to four
    // bytes from whatever the buffer already held rather than zeroing -- the
    // captured NodePath in the fixture pads "Level" with `30 30 30` -- so a
    // sample whose padding is not zero cannot be reproduced byte for byte and
    // is checked by decoding the re-encoding instead.
    for s in samples() {
        let (value, _) = decode(&s.bytes, 0).unwrap_or_else(|e| panic!("{}: {e:?}", s.label));
        let again = encode(&value);
        if again == s.bytes {
            continue;
        }
        let (round, _) = decode(&again, 0).unwrap_or_else(|e| panic!("{}: {e:?}", s.label));
        assert_eq!(
            round, value,
            "{}: re-encoding changed the value, not just its padding\n  was {:02x?}\n  now {:02x?}",
            s.label, s.bytes, again
        );
        assert_eq!(
            again.len(),
            s.bytes.len(),
            "{}: re-encoding changed the length",
            s.label
        );
    }
}

#[test]
fn a_zero_sample_and_a_distinctive_one_do_not_decode_alike() {
    // The test that catches a decoder returning a default. Every type has two
    // samples in the fixture and they are never the same value.
    let all = samples();
    for pair in all.chunks(2) {
        let [a, b] = pair else { continue };
        if a.id != b.id {
            continue;
        }
        let (va, _) = decode(&a.bytes, 0).expect("a");
        let (vb, _) = decode(&b.bytes, 0).expect("b");
        assert_ne!(va, vb, "{} and {} decode the same", a.label, b.label);
    }
}

#[test]
fn a_type_the_crate_refuses_says_so_rather_than_guessing() {
    // 24 is Object. An unknown type has an unknown length, so there is no next
    // field to find and continuing would turn one unreadable value into an
    // unreadable packet.
    let bytes = [24u8, 0, 0, 0, 1, 2, 3, 4];
    assert!(decode(&bytes, 0).is_err());
}
