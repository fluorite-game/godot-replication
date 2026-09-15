//! Godot's `Variant` encoding, in both forms it appears in on the wire.
//!
//! # Two forms, and the difference is load-bearing
//!
//! `encode_variant` writes a four-byte header: the type in the low sixteen
//! bits, flags in the high sixteen. That is what `var_to_bytes()` produces in
//! `GDScript`, and what RPC arguments carry.
//!
//! `encode_and_compress_variant` is a *different function*, and it is what
//! SYNC packets carry. It gives `bool` and `int` a **one-byte** header and
//! leaves every other type in the plain form. Assuming the four-byte header
//! everywhere is what defeated two attempts to parse a SYNC packet before the
//! difference was measured.
//!
//! # Measured values
//!
//! Plain form, from `tools/variant_wire_oracle.gd` calling `var_to_bytes()`:
//!
//! ```text
//! bool false      0100000000000000
//! int 5           0200000005000000
//! float 0.5       030000000000003f      32-bit: lossless
//! float -6.3808   030001006744696ff08519c0    64-bit: flag bit 0
//! Vector2 (1,-1)  050000000000803f000080bf
//! Vector3         09000000702e8f42832fccc02b183942
//! Transform3D     12000000 + 12 floats
//! ```
//!
//! Compact form, from `tools/capture_sync_probe.sh` replicating one property:
//!
//! ```text
//! bool false      01            bit 7 is the value
//! bool true       81
//! int 0           02 00         bits 7-6 are a width code
//! int 300         42 2c01       predicted from the first two, then captured
//! int 0x11223344  82 44332211
//! int 2^33        c2 + 8 bytes  predicted, then captured
//! ```

use core::convert::TryInto;

/// A decode failure. Never guessed past: an unknown type has an unknown
/// length, so there is no next field to find, and continuing turns one
/// unreadable value into an unreadable packet.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum VariantError {
    /// The buffer ended inside a value.
    Truncated {
        /// Where the value began.
        at: usize,
    },
    /// A type id this crate does not encode or decode.
    UnknownType {
        /// The id read from the header.
        id: u16,
        /// Where the header began.
        at: usize,
    },
}

/// The `Variant::Type` ids this demo puts on the wire.
///
/// Six, across all five of its `SceneReplicationConfig` blocks. Deliberately
/// not the whole of Godot's type list: a type nothing sends cannot be checked
/// against a capture, and an unchecked codec path is a guess with tests around
/// it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum VariantType {
    /// `bool`
    Bool,
    /// `int`
    Int,
    /// `float`
    Float,
    /// `Vector2`
    Vector2,
    /// `Vector3`
    Vector3,
    /// `Transform3D`
    Transform3D,
}

impl VariantType {
    /// Godot's own id for this type.
    #[must_use]
    pub const fn id(self) -> u16 {
        match self {
            Self::Bool => 1,
            Self::Int => 2,
            Self::Float => 3,
            Self::Vector2 => 5,
            Self::Vector3 => 9,
            Self::Transform3D => 18,
        }
    }

    /// The type an id names, or `None` for one this crate does not handle.
    #[must_use]
    pub const fn from_id(id: u16) -> Option<Self> {
        match id {
            1 => Some(Self::Bool),
            2 => Some(Self::Int),
            3 => Some(Self::Float),
            5 => Some(Self::Vector2),
            9 => Some(Self::Vector3),
            18 => Some(Self::Transform3D),
            _ => None,
        }
    }
}

/// A decoded value.
#[derive(Debug, PartialEq, Clone)]
pub enum Value {
    /// `bool`
    Bool(bool),
    /// `int`, always widened to 64 bits here regardless of how it travelled.
    Int(i64),
    /// `float`, always widened to 64 bits here.
    Float(f64),
    /// `Vector2`
    Vector2([f32; 2]),
    /// `Vector3`
    Vector3([f32; 3]),
    /// `Transform3D`: nine basis floats row-major, then three of origin.
    ///
    /// Row-major is worth stating, because Godot's `Basis(x, y, z)`
    /// constructor takes *columns*: a basis built from an x-axis of
    /// `(0.843905, 0, -0.536493)` encodes its first three floats as
    /// `(0.843905, 0, 0.536493)`, which is row zero. Transposed, the rotation
    /// is wrong about one axis only, which reads as a tuning problem rather
    /// than an encoding one.
    Transform3D([f32; 12]),
}

impl Value {
    /// The type this value encodes as.
    #[must_use]
    pub const fn variant_type(&self) -> VariantType {
        match self {
            Self::Bool(_) => VariantType::Bool,
            Self::Int(_) => VariantType::Int,
            Self::Float(_) => VariantType::Float,
            Self::Vector2(_) => VariantType::Vector2,
            Self::Vector3(_) => VariantType::Vector3,
            Self::Transform3D(_) => VariantType::Transform3D,
        }
    }
}

/// The plain header's bit 0: this int or float travelled as 64 bits.
pub const FLAG_64: u16 = 0x0001;

/// Bytes each compact int width code carries, indexed by the code.
///
/// The code is bits 7-6 of the compact header byte. Codes 1 and 3 were
/// predicted from codes 0 and 2 and then captured, rather than read off and
/// rationalised afterwards.
pub const COMPACT_INT_WIDTHS: [usize; 4] = [1, 2, 4, 8];

fn read_f32(bytes: &[u8], at: usize) -> Result<f32, VariantError> {
    bytes
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(f32::from_le_bytes)
        .ok_or(VariantError::Truncated { at })
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Encodes one value in the plain four-byte-header form.
///
/// Ints and floats are written narrow whenever that is lossless, which is
/// Godot's own rule -- `0.5` is four bytes and `-6.3808` is eight. A decoder
/// that assumes the wide form reads the next field's header as this value's
/// tail.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    let header = |flags: u16, out: &mut Vec<u8>| {
        let word = u32::from(value.variant_type().id()) | (u32::from(flags) << 16);
        out.extend_from_slice(&word.to_le_bytes());
    };
    match *value {
        Value::Bool(v) => {
            header(0, &mut out);
            out.extend_from_slice(&u32::from(v).to_le_bytes());
        }
        Value::Int(v) => {
            let wide = i32::try_from(v).is_err();
            header(if wide { FLAG_64 } else { 0 }, &mut out);
            if wide {
                out.extend_from_slice(&v.to_le_bytes());
            } else {
                out.extend_from_slice(&(v as i32).to_le_bytes());
            }
        }
        Value::Float(v) => {
            // Exact equality on purpose: the question is whether this value
            // survives a 32-bit round trip unchanged, which is precisely
            // Godot's own rule for choosing the narrow form. An epsilon here
            // would encode narrow for values the engine widens, and the two
            // sides would then disagree by a byte count.
            #[allow(clippy::float_cmp)]
            let narrow = f64::from(v as f32) == v;
            header(if narrow { 0 } else { FLAG_64 }, &mut out);
            if narrow {
                push_f32(&mut out, v as f32);
            } else {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Value::Vector2(v) => {
            header(0, &mut out);
            for f in v {
                push_f32(&mut out, f);
            }
        }
        Value::Vector3(v) => {
            header(0, &mut out);
            for f in v {
                push_f32(&mut out, f);
            }
        }
        Value::Transform3D(v) => {
            header(0, &mut out);
            for f in v {
                push_f32(&mut out, f);
            }
        }
    }
    out
}

/// The compact form's tag byte carries the type in its low six bits, so only
/// types whose id fits there can be compacted. Both that do are single digits.
const COMPACT_BOOL_TAG: u8 = 1;
const COMPACT_INT_TAG: u8 = 2;

/// Encodes one value as SYNC carries it.
///
/// `bool` and `int` become compact; everything else is [`encode`] unchanged.
#[must_use]
pub fn encode_compact(value: &Value) -> Vec<u8> {
    debug_assert_eq!(u16::from(COMPACT_BOOL_TAG), VariantType::Bool.id());
    debug_assert_eq!(u16::from(COMPACT_INT_TAG), VariantType::Int.id());
    match *value {
        Value::Bool(v) => vec![COMPACT_BOOL_TAG | if v { 0x80 } else { 0 }],
        Value::Int(v) => {
            let code = compact_width_code(v);
            let mut out = vec![COMPACT_INT_TAG | (code << 6)];
            let bytes = v.to_le_bytes();
            out.extend_from_slice(&bytes[..COMPACT_INT_WIDTHS[code as usize]]);
            out
        }
        _ => encode(value),
    }
}

/// The narrowest width code that holds `value`.
#[must_use]
pub const fn compact_width_code(value: i64) -> u8 {
    if value >= -128 && value <= 127 {
        0
    } else if value >= -32768 && value <= 32767 {
        1
    } else if value >= -2_147_483_648 && value <= 2_147_483_647 {
        2
    } else {
        3
    }
}

/// Reads one value in the plain form, returning it and the next offset.
///
/// # Errors
///
/// [`VariantError::UnknownType`] for a type this crate does not handle, and
/// [`VariantError::Truncated`] when the buffer ends inside the value.
///
/// # Panics
///
/// Does not: the `expect` calls below are on slices whose length was just
/// checked by the enclosing `take`, and are there to convert a slice into a
/// fixed-size array.
pub fn decode(bytes: &[u8], at: usize) -> Result<(Value, usize), VariantError> {
    let head = bytes
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(VariantError::Truncated { at })?;
    #[allow(clippy::cast_possible_truncation)]
    let id = (head & 0xFFFF) as u16;
    #[allow(clippy::cast_possible_truncation)]
    let flags = (head >> 16) as u16;
    let kind = VariantType::from_id(id).ok_or(VariantError::UnknownType { id, at })?;
    let body = at + 4;
    let wide = flags & FLAG_64 != 0;

    let take = |n: usize| -> Result<&[u8], VariantError> {
        bytes
            .get(body..body + n)
            .ok_or(VariantError::Truncated { at })
    };

    Ok(match kind {
        VariantType::Bool => {
            let raw: [u8; 4] = take(4)?.try_into().expect("checked");
            (Value::Bool(u32::from_le_bytes(raw) != 0), body + 4)
        }
        VariantType::Int if wide => {
            let raw: [u8; 8] = take(8)?.try_into().expect("checked");
            (Value::Int(i64::from_le_bytes(raw)), body + 8)
        }
        VariantType::Int => {
            let raw: [u8; 4] = take(4)?.try_into().expect("checked");
            (Value::Int(i64::from(i32::from_le_bytes(raw))), body + 4)
        }
        VariantType::Float if wide => {
            let raw: [u8; 8] = take(8)?.try_into().expect("checked");
            (Value::Float(f64::from_le_bytes(raw)), body + 8)
        }
        VariantType::Float => {
            let raw: [u8; 4] = take(4)?.try_into().expect("checked");
            (Value::Float(f64::from(f32::from_le_bytes(raw))), body + 4)
        }
        VariantType::Vector2 => {
            take(8)?;
            (
                Value::Vector2([read_f32(bytes, body)?, read_f32(bytes, body + 4)?]),
                body + 8,
            )
        }
        VariantType::Vector3 => {
            take(12)?;
            let mut v = [0f32; 3];
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = read_f32(bytes, body + i * 4)?;
            }
            (Value::Vector3(v), body + 12)
        }
        VariantType::Transform3D => {
            take(48)?;
            let mut v = [0f32; 12];
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = read_f32(bytes, body + i * 4)?;
            }
            (Value::Transform3D(v), body + 48)
        }
    })
}

/// Reads one value as SYNC carries it, falling back to the plain form.
///
/// The discriminator is the low six bits of the first byte: `bool` and `int`
/// are compact, everything else wrote a four-byte header.
///
/// # Errors
///
/// As [`decode`].
pub fn decode_compact(bytes: &[u8], at: usize) -> Result<(Value, usize), VariantError> {
    let lead = *bytes.get(at).ok_or(VariantError::Truncated { at })?;
    let id = u16::from(lead & 0x3F);

    if id == VariantType::Bool.id() {
        return Ok((Value::Bool(lead & 0x80 != 0), at + 1));
    }
    if id == VariantType::Int.id() {
        let width = COMPACT_INT_WIDTHS[usize::from(lead >> 6)];
        let raw = bytes
            .get(at + 1..at + 1 + width)
            .ok_or(VariantError::Truncated { at })?;
        // Sign-extend from the width it travelled in: 0xff in one byte is -1,
        // not 255, and a decoder that zero-extends turns every small negative
        // into a large positive.
        let mut buf = if raw[width - 1] & 0x80 != 0 {
            [0xFFu8; 8]
        } else {
            [0u8; 8]
        };
        buf[..width].copy_from_slice(raw);
        return Ok((Value::Int(i64::from_le_bytes(buf)), at + 1 + width));
    }
    decode(bytes, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }

    // Every expected string below was printed by `var_to_bytes()` in
    // tools/variant_wire_oracle.gd, or read out of a capture taken by
    // tools/capture_sync_probe.sh. None was computed by this crate.

    #[test]
    fn plain_form_matches_the_engine() {
        assert_eq!(hex(&encode(&Value::Bool(false))), "0100000000000000");
        assert_eq!(hex(&encode(&Value::Bool(true))), "0100000001000000");
        assert_eq!(hex(&encode(&Value::Int(5))), "0200000005000000");
        assert_eq!(hex(&encode(&Value::Int(-1))), "02000000ffffffff");
        assert_eq!(hex(&encode(&Value::Float(0.5))), "030000000000003f");
        assert_eq!(
            hex(&encode(&Value::Vector2([1.0, -1.0]))),
            "050000000000803f000080bf"
        );
    }

    #[test]
    fn width_follows_the_value_not_the_type() {
        // Godot writes the narrow form when it is lossless. A decoder that
        // assumes the wide one reads the next field's header as this tail.
        assert_eq!(hex(&encode(&Value::Int(2_147_483_647))).len(), 16);
        assert_eq!(hex(&encode(&Value::Int(2_147_483_648))).len(), 24);
        assert_eq!(
            hex(&encode(&Value::Float(-6.3808))),
            "030001006744696ff08519c0"
        );
    }

    #[test]
    fn compact_form_matches_the_wire() {
        assert_eq!(hex(&encode_compact(&Value::Bool(false))), "01");
        assert_eq!(hex(&encode_compact(&Value::Bool(true))), "81");
        assert_eq!(hex(&encode_compact(&Value::Int(0))), "0200");
        assert_eq!(hex(&encode_compact(&Value::Int(5))), "0205");
        // These two were predicted from the others and then captured.
        assert_eq!(hex(&encode_compact(&Value::Int(300))), "422c01");
        assert_eq!(hex(&encode_compact(&Value::Int(287_454_020))), "8244332211");
        assert_eq!(
            hex(&encode_compact(&Value::Int(8_589_934_592))),
            "c20000000002000000"
        );
    }

    #[test]
    fn compact_ints_sign_extend_from_their_width() {
        // 0xff in one byte is -1, not 255. Zero-extending turns every small
        // negative into a large positive, which is exactly the kind of wrong
        // that still looks like a number.
        for value in [-1i64, -128, -129, -32768, -32769, -2_147_483_648, i64::MIN] {
            let bytes = encode_compact(&Value::Int(value));
            let (decoded, next) = decode_compact(&bytes, 0).expect("decodes");
            assert_eq!(decoded, Value::Int(value), "round trip of {value}");
            assert_eq!(next, bytes.len());
        }
    }

    #[test]
    fn only_bool_and_int_are_compacted() {
        // Measured: a float on the wire is `03000100...`, the plain form.
        let (value, next) = decode_compact(&unhex("030001006744696ff08519c0"), 0).expect("decodes");
        assert_eq!(next, 12);
        match value {
            Value::Float(f) => assert!((f - -6.3808).abs() < 1e-12),
            other => panic!("expected a float, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_type_is_refused_rather_than_skipped() {
        assert_eq!(
            decode(&unhex("ff00000000000000"), 0),
            Err(VariantError::UnknownType { id: 0x00ff, at: 0 })
        );
        assert_eq!(
            decode(&unhex("0200"), 0),
            Err(VariantError::Truncated { at: 0 })
        );
    }
}
