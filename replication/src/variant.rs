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
//! Plain form, from a wire oracle run inside Godot calling `var_to_bytes()`:
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
//! Compact form, from the sync probe (two peers, one replicated property) replicating one property:
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

/// Godot's `Variant::Type` ids, as the engine writes them.
///
/// The ids are the engine's own enum and the gaps are real: 23 through 26 are
/// `RID`, `Object`, `Callable` and `Signal`, which this crate refuses on
/// purpose. They encode a pointer or an instance id that means nothing at the
/// far end, `var_to_bytes` writes them only with `full_objects` set, and a
/// replication protocol that accepted them would be handing a remote peer a
/// deserialization primitive.
///
/// Every id below is pinned to bytes the engine produced; see
/// `tests/fixtures/variant_types.hex` and the oracle that generated it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[allow(missing_docs)]
pub enum VariantType {
    Nil,
    Bool,
    Int,
    Float,
    String,
    Vector2,
    Vector2i,
    Rect2,
    Rect2i,
    Vector3,
    Vector3i,
    Transform2D,
    Vector4,
    Vector4i,
    Plane,
    Quaternion,
    Aabb,
    Basis,
    Transform3D,
    Projection,
    Color,
    StringName,
    NodePath,
    Dictionary,
    Array,
    PackedByteArray,
    PackedInt32Array,
    PackedInt64Array,
    PackedFloat32Array,
    PackedFloat64Array,
    PackedStringArray,
    PackedVector2Array,
    PackedVector3Array,
    PackedColorArray,
    PackedVector4Array,
}

impl VariantType {
    /// Godot's own id for this type.
    #[must_use]
    pub const fn id(self) -> u16 {
        match self {
            Self::Nil => 0,
            Self::Bool => 1,
            Self::Int => 2,
            Self::Float => 3,
            Self::String => 4,
            Self::Vector2 => 5,
            Self::Vector2i => 6,
            Self::Rect2 => 7,
            Self::Rect2i => 8,
            Self::Vector3 => 9,
            Self::Vector3i => 10,
            Self::Transform2D => 11,
            Self::Vector4 => 12,
            Self::Vector4i => 13,
            Self::Plane => 14,
            Self::Quaternion => 15,
            Self::Aabb => 16,
            Self::Basis => 17,
            Self::Transform3D => 18,
            Self::Projection => 19,
            Self::Color => 20,
            Self::StringName => 21,
            Self::NodePath => 22,
            Self::Dictionary => 27,
            Self::Array => 28,
            Self::PackedByteArray => 29,
            Self::PackedInt32Array => 30,
            Self::PackedInt64Array => 31,
            Self::PackedFloat32Array => 32,
            Self::PackedFloat64Array => 33,
            Self::PackedStringArray => 34,
            Self::PackedVector2Array => 35,
            Self::PackedVector3Array => 36,
            Self::PackedColorArray => 37,
            Self::PackedVector4Array => 38,
        }
    }

    /// The type an id names, or `None` for one this crate does not handle.
    #[must_use]
    pub const fn from_id(id: u16) -> Option<Self> {
        match id {
            0 => Some(Self::Nil),
            1 => Some(Self::Bool),
            2 => Some(Self::Int),
            3 => Some(Self::Float),
            4 => Some(Self::String),
            5 => Some(Self::Vector2),
            6 => Some(Self::Vector2i),
            7 => Some(Self::Rect2),
            8 => Some(Self::Rect2i),
            9 => Some(Self::Vector3),
            10 => Some(Self::Vector3i),
            11 => Some(Self::Transform2D),
            12 => Some(Self::Vector4),
            13 => Some(Self::Vector4i),
            14 => Some(Self::Plane),
            15 => Some(Self::Quaternion),
            16 => Some(Self::Aabb),
            17 => Some(Self::Basis),
            18 => Some(Self::Transform3D),
            19 => Some(Self::Projection),
            20 => Some(Self::Color),
            21 => Some(Self::StringName),
            22 => Some(Self::NodePath),
            27 => Some(Self::Dictionary),
            28 => Some(Self::Array),
            29 => Some(Self::PackedByteArray),
            30 => Some(Self::PackedInt32Array),
            31 => Some(Self::PackedInt64Array),
            32 => Some(Self::PackedFloat32Array),
            33 => Some(Self::PackedFloat64Array),
            34 => Some(Self::PackedStringArray),
            35 => Some(Self::PackedVector2Array),
            36 => Some(Self::PackedVector3Array),
            37 => Some(Self::PackedColorArray),
            38 => Some(Self::PackedVector4Array),
            _ => None,
        }
    }

    /// How many `f32`s a fixed-size float type carries, if it is one.
    ///
    /// Most of Godot's geometry is a run of little-endian `f32` and nothing
    /// else, so the codec handles them as one case rather than eighteen.
    #[must_use]
    pub const fn float_count(self) -> Option<usize> {
        match self {
            Self::Vector2 => Some(2),
            Self::Vector3 => Some(3),
            Self::Vector4 | Self::Rect2 | Self::Plane | Self::Quaternion | Self::Color => Some(4),
            Self::Transform2D | Self::Aabb => Some(6),
            Self::Basis => Some(9),
            Self::Transform3D => Some(12),
            Self::Projection => Some(16),
            _ => None,
        }
    }

    /// How many `i32`s a fixed-size integer-vector type carries, if it is one.
    #[must_use]
    pub const fn int_count(self) -> Option<usize> {
        match self {
            Self::Vector2i => Some(2),
            Self::Vector3i => Some(3),
            Self::Vector4i | Self::Rect2i => Some(4),
            _ => None,
        }
    }
}

/// A decoded value.
///
/// Types that are a run of `f32` share a representation deliberately: a
/// `Rect2`, a `Plane`, a `Quaternion` and a `Color` are all four floats on the
/// wire, and giving each its own tuple struct would be four ways to say the
/// same thing. The `VariantType` carried alongside is what distinguishes them.
#[derive(Debug, PartialEq, Clone)]
#[allow(missing_docs)]
pub enum Value {
    Nil,
    Bool(bool),
    /// `int`, always widened to 64 bits here regardless of how it travelled.
    Int(i64),
    /// `float`, always widened to 64 bits here.
    Float(f64),
    /// `String`, and `StringName` under its own type id.
    Str {
        /// Which of the two string types this was.
        kind: VariantType,
        /// The text. Godot writes UTF-8 with a byte length, not a character
        /// count.
        text: String,
    },
    /// Two `f32`.
    Vector2([f32; 2]),
    /// Three `f32`.
    Vector3([f32; 3]),
    /// Four `f32`: `Vector4`, `Rect2`, `Plane`, `Quaternion` or `Color`.
    Float4 {
        /// Which of the five this was.
        kind: VariantType,
        /// The components, in the order the engine wrote them.
        v: [f32; 4],
    },
    /// Six `f32`: `Transform2D` or `AABB`.
    Float6 {
        /// Which of the two this was.
        kind: VariantType,
        /// The components, in the order the engine wrote them.
        v: [f32; 6],
    },
    /// Nine `f32`, row-major.
    Basis([f32; 9]),
    /// `Transform3D`: nine basis floats row-major, then three of origin.
    ///
    /// Row-major is worth stating, because Godot's `Basis(x, y, z)`
    /// constructor takes *columns*: a basis built from an x-axis of
    /// `(0.843905, 0, -0.536493)` encodes its first three floats as
    /// `(0.843905, 0, 0.536493)`, which is row zero. Transposed, the rotation
    /// is wrong about one axis only, which reads as a tuning problem rather
    /// than an encoding one.
    Transform3D([f32; 12]),
    /// Sixteen `f32`, four columns of four.
    Projection([f32; 16]),
    /// Two, three or four `i32`: `Vector2i`, `Vector3i`, `Vector4i`, `Rect2i`.
    Ints {
        /// Which of the four this was.
        kind: VariantType,
        /// The components, in the order the engine wrote them.
        v: Vec<i32>,
    },
    /// A node path: names, subnames, and whether it is rooted.
    NodePath(NodePath),
    /// Key/value pairs, each a nested `Value`.
    Dictionary(Vec<(Value, Value)>),
    /// Elements, each a nested `Value`.
    Array(Vec<Value>),
    /// `PackedByteArray`.
    Bytes(Vec<u8>),
    /// `PackedInt32Array`.
    Int32s(Vec<i32>),
    /// `PackedInt64Array`.
    Int64s(Vec<i64>),
    /// `PackedFloat32Array`.
    Float32s(Vec<f32>),
    /// `PackedFloat64Array`.
    Float64s(Vec<f64>),
    /// `PackedStringArray`.
    Strings(Vec<String>),
    /// `PackedVector2Array`, `PackedVector3Array`, `PackedColorArray` or
    /// `PackedVector4Array`: a count, then that many fixed-width runs.
    Vectors {
        /// Which of the four this was, and so how wide each element is.
        kind: VariantType,
        /// The elements, flattened.
        v: Vec<f32>,
    },
}

/// A `NodePath`, which Godot writes as two lists of strings and a flag.
///
/// `Player:position:x` is one name and two subnames; `/root/Level` is two
/// names, absolute. The count field carries `0x8000_0000` set, which is how the
/// engine marks the format it has used since 4.0.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct NodePath {
    /// The node names, outermost first.
    pub names: Vec<String>,
    /// The property path after the `:`, if any.
    pub subnames: Vec<String>,
    /// Whether the path starts at the scene root.
    pub absolute: bool,
}

impl Value {
    /// The type this value encodes as.
    #[must_use]
    pub const fn variant_type(&self) -> VariantType {
        match self {
            Self::Nil => VariantType::Nil,
            Self::Bool(_) => VariantType::Bool,
            Self::Int(_) => VariantType::Int,
            Self::Float(_) => VariantType::Float,
            Self::Str { kind, .. }
            | Self::Float4 { kind, .. }
            | Self::Float6 { kind, .. }
            | Self::Ints { kind, .. }
            | Self::Vectors { kind, .. } => *kind,
            Self::Vector2(_) => VariantType::Vector2,
            Self::Vector3(_) => VariantType::Vector3,
            Self::Basis(_) => VariantType::Basis,
            Self::Transform3D(_) => VariantType::Transform3D,
            Self::Projection(_) => VariantType::Projection,
            Self::NodePath(_) => VariantType::NodePath,
            Self::Dictionary(_) => VariantType::Dictionary,
            Self::Array(_) => VariantType::Array,
            Self::Bytes(_) => VariantType::PackedByteArray,
            Self::Int32s(_) => VariantType::PackedInt32Array,
            Self::Int64s(_) => VariantType::PackedInt64Array,
            Self::Float32s(_) => VariantType::PackedFloat32Array,
            Self::Float64s(_) => VariantType::PackedFloat64Array,
            Self::Strings(_) => VariantType::PackedStringArray,
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
    encode_into(value, &mut out);
    out
}

/// Appends one value's encoding, so containers can nest without reallocating
/// a buffer per element.
///
/// Long because it is one arm per Variant type and the engine has thirty-five
/// of them. Splitting it would put each type's two or three lines of format in
/// a helper called from exactly one place, which is further to look for the
/// answer to "what does a Quaternion look like on the wire", not nearer.
#[allow(clippy::too_many_lines)]
fn encode_into(value: &Value, out: &mut Vec<u8>) {
    let head = |flags: u16, out: &mut Vec<u8>| {
        let word = u32::from(value.variant_type().id()) | (u32::from(flags) << 16);
        out.extend_from_slice(&word.to_le_bytes());
    };
    let count = |n: usize, out: &mut Vec<u8>| {
        out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
    };
    match value {
        Value::Nil => head(0, out),
        Value::Bool(v) => {
            head(0, out);
            out.extend_from_slice(&u32::from(*v).to_le_bytes());
        }
        Value::Int(v) => {
            let wide = i32::try_from(*v).is_err();
            head(if wide { FLAG_64 } else { 0 }, out);
            if wide {
                out.extend_from_slice(&v.to_le_bytes());
            } else {
                #[allow(clippy::cast_possible_truncation)]
                out.extend_from_slice(&(*v as i32).to_le_bytes());
            }
        }
        Value::Float(v) => {
            // Exact equality on purpose: the question is whether this value
            // survives a 32-bit round trip unchanged, which is precisely
            // Godot's own rule for choosing the narrow form. An epsilon here
            // would encode narrow for values the engine widens, and the two
            // sides would then disagree by a byte count.
            #[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
            let narrow = f64::from(*v as f32) == *v;
            head(if narrow { 0 } else { FLAG_64 }, out);
            if narrow {
                #[allow(clippy::cast_possible_truncation)]
                push_f32(out, *v as f32);
            } else {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Value::Str { text, .. } => {
            head(0, out);
            push_string(text, out);
        }
        Value::Vector2(v) => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Vector3(v) => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Float4 { v, .. } => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Float6 { v, .. } => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Basis(v) => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Transform3D(v) => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Projection(v) => {
            head(0, out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Ints { v, .. } => {
            head(0, out);
            for i in v {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        Value::NodePath(path) => {
            head(0, out);
            // The high bit marks the post-4.0 layout. Without it the engine
            // reads the old single-string form and finds garbage.
            let names = u32::try_from(path.names.len()).unwrap_or(0) | 0x8000_0000;
            out.extend_from_slice(&names.to_le_bytes());
            count(path.subnames.len(), out);
            out.extend_from_slice(&u32::from(path.absolute).to_le_bytes());
            for name in path.names.iter().chain(&path.subnames) {
                push_string(name, out);
            }
        }
        Value::Dictionary(pairs) => {
            head(0, out);
            count(pairs.len(), out);
            for (k, v) in pairs {
                encode_into(k, out);
                encode_into(v, out);
            }
        }
        Value::Array(items) => {
            head(0, out);
            count(items.len(), out);
            for item in items {
                encode_into(item, out);
            }
        }
        Value::Bytes(v) => {
            head(0, out);
            count(v.len(), out);
            out.extend_from_slice(v);
            pad_to_4(v.len(), out);
        }
        Value::Int32s(v) => {
            head(0, out);
            count(v.len(), out);
            for i in v {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        Value::Int64s(v) => {
            head(0, out);
            count(v.len(), out);
            for i in v {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        Value::Float32s(v) => {
            head(0, out);
            count(v.len(), out);
            for f in v {
                push_f32(out, *f);
            }
        }
        Value::Float64s(v) => {
            head(0, out);
            count(v.len(), out);
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        Value::Strings(v) => {
            head(0, out);
            count(v.len(), out);
            for text in v {
                push_string(text, out);
            }
        }
        Value::Vectors { kind, v } => {
            head(0, out);
            let width = packed_vector_width(*kind);
            count(v.len().checked_div(width).unwrap_or(0), out);
            for f in v {
                push_f32(out, *f);
            }
        }
    }
}

/// How many `f32` one element of a packed vector array carries.
const fn packed_vector_width(kind: VariantType) -> usize {
    match kind {
        VariantType::PackedVector2Array => 2,
        VariantType::PackedVector3Array => 3,
        VariantType::PackedColorArray | VariantType::PackedVector4Array => 4,
        _ => 0,
    }
}

/// Writes a byte length, the UTF-8, then zeroes up to a four-byte boundary.
///
/// The length is bytes and not characters, which is the whole of why the
/// fixture carries a string with an em dash and a hiragana in it: a decoder
/// that counts characters passes every ASCII sample and fails that one.
fn push_string(text: &str, out: &mut Vec<u8>) {
    let bytes = text.as_bytes();
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(bytes);
    pad_to_4(bytes.len(), out);
}

/// Zero padding to the next four-byte boundary.
///
/// Zeroes here, but do not assume them when reading: the engine pads from
/// whatever the buffer already held, and a captured `NodePath` in the fixture
/// pads `"Level"` with the bytes `30 30 30`. So a decoder skips the padding
/// rather than checking it, and a byte-exact re-encode of someone else's
/// packet is not always possible.
fn pad_to_4(len: usize, out: &mut Vec<u8>) {
    let pad = (4 - (len % 4)) % 4;
    out.extend(core::iter::repeat_n(0u8, pad));
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
#[allow(clippy::too_many_lines)]
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

    // The geometry types are a run of f32 and nothing else, so they share one
    // arm rather than eighteen that differ by a length.
    if let Some(n) = kind.float_count() {
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            v.push(read_f32(bytes, body + i * 4)?);
        }
        let end = body + n * 4;
        return Ok((float_value(kind, &v), end));
    }
    if let Some(n) = kind.int_count() {
        let mut v = Vec::with_capacity(n);
        for i in 0..n {
            v.push(read_i32(bytes, body + i * 4)?);
        }
        return Ok((Value::Ints { kind, v }, body + n * 4));
    }

    let take = |n: usize| -> Result<&[u8], VariantError> {
        bytes
            .get(body..body + n)
            .ok_or(VariantError::Truncated { at })
    };

    Ok(match kind {
        VariantType::Nil => (Value::Nil, body),
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
        VariantType::String | VariantType::StringName => {
            let (text, next) = read_string(bytes, body, at)?;
            (Value::Str { kind, text }, next)
        }
        VariantType::NodePath => {
            let raw = read_u32(bytes, body, at)?;
            let subs = read_u32(bytes, body + 4, at)? as usize;
            let absolute = read_u32(bytes, body + 8, at)? != 0;
            let names = (raw & 0x7FFF_FFFF) as usize;
            let mut path = NodePath {
                absolute,
                ..NodePath::default()
            };
            let mut off = body + 12;
            for _ in 0..names {
                let (text, next) = read_string(bytes, off, at)?;
                path.names.push(text);
                off = next;
            }
            for _ in 0..subs {
                let (text, next) = read_string(bytes, off, at)?;
                path.subnames.push(text);
                off = next;
            }
            (Value::NodePath(path), off)
        }
        VariantType::Dictionary => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut pairs = Vec::with_capacity(n);
            let mut off = body + 4;
            for _ in 0..n {
                let (k, next) = decode(bytes, off)?;
                let (v, next) = decode(bytes, next)?;
                pairs.push((k, v));
                off = next;
            }
            (Value::Dictionary(pairs), off)
        }
        VariantType::Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut items = Vec::with_capacity(n);
            let mut off = body + 4;
            for _ in 0..n {
                let (item, next) = decode(bytes, off)?;
                items.push(item);
                off = next;
            }
            (Value::Array(items), off)
        }
        VariantType::PackedByteArray => {
            let n = read_u32(bytes, body, at)? as usize;
            let data = bytes
                .get(body + 4..body + 4 + n)
                .ok_or(VariantError::Truncated { at })?;
            let pad = (4 - (n % 4)) % 4;
            (Value::Bytes(data.to_vec()), body + 4 + n + pad)
        }
        VariantType::PackedInt32Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                v.push(read_i32(bytes, body + 4 + i * 4)?);
            }
            (Value::Int32s(v), body + 4 + n * 4)
        }
        VariantType::PackedInt64Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                let raw: [u8; 8] = bytes
                    .get(body + 4 + i * 8..body + 12 + i * 8)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(VariantError::Truncated { at })?;
                v.push(i64::from_le_bytes(raw));
            }
            (Value::Int64s(v), body + 4 + n * 8)
        }
        VariantType::PackedFloat32Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                v.push(read_f32(bytes, body + 4 + i * 4)?);
            }
            (Value::Float32s(v), body + 4 + n * 4)
        }
        VariantType::PackedFloat64Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                let raw: [u8; 8] = bytes
                    .get(body + 4 + i * 8..body + 12 + i * 8)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(VariantError::Truncated { at })?;
                v.push(f64::from_le_bytes(raw));
            }
            (Value::Float64s(v), body + 4 + n * 8)
        }
        VariantType::PackedStringArray => {
            let n = read_u32(bytes, body, at)? as usize;
            let mut v = Vec::with_capacity(n);
            let mut off = body + 4;
            for _ in 0..n {
                let (text, next) = read_string(bytes, off, at)?;
                v.push(text);
                off = next;
            }
            (Value::Strings(v), off)
        }
        VariantType::PackedVector2Array
        | VariantType::PackedVector3Array
        | VariantType::PackedColorArray
        | VariantType::PackedVector4Array => {
            let n = read_u32(bytes, body, at)? as usize;
            let width = packed_vector_width(kind);
            let mut v = Vec::with_capacity(n * width);
            for i in 0..n * width {
                v.push(read_f32(bytes, body + 4 + i * 4)?);
            }
            (Value::Vectors { kind, v }, body + 4 + n * width * 4)
        }
        // Every remaining arm is handled by float_count/int_count above.
        _ => return Err(VariantError::UnknownType { id, at }),
    })
}

/// Wraps a run of floats in whichever `Value` its type calls for.
fn float_value(kind: VariantType, v: &[f32]) -> Value {
    match kind {
        VariantType::Vector2 => Value::Vector2([v[0], v[1]]),
        VariantType::Vector3 => Value::Vector3([v[0], v[1], v[2]]),
        VariantType::Basis => {
            let mut a = [0f32; 9];
            a.copy_from_slice(v);
            Value::Basis(a)
        }
        VariantType::Transform3D => {
            let mut a = [0f32; 12];
            a.copy_from_slice(v);
            Value::Transform3D(a)
        }
        VariantType::Projection => {
            let mut a = [0f32; 16];
            a.copy_from_slice(v);
            Value::Projection(a)
        }
        VariantType::Transform2D | VariantType::Aabb => {
            let mut a = [0f32; 6];
            a.copy_from_slice(v);
            Value::Float6 { kind, v: a }
        }
        _ => {
            let mut a = [0f32; 4];
            a.copy_from_slice(v);
            Value::Float4 { kind, v: a }
        }
    }
}

/// A little-endian `u32`, or `Truncated` naming where the value began.
fn read_u32(bytes: &[u8], at: usize, start: usize) -> Result<u32, VariantError> {
    bytes
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(VariantError::Truncated { at: start })
}

/// A little-endian `i32`.
fn read_i32(bytes: &[u8], at: usize) -> Result<i32, VariantError> {
    bytes
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or(VariantError::Truncated { at })
}

/// A length-prefixed UTF-8 string, returning it and the offset past its
/// padding.
///
/// Invalid UTF-8 is replaced rather than refused: the bytes came off a socket,
/// and a peer that sends a malformed name should cost one unreadable label
/// rather than an unreadable packet.
fn read_string(bytes: &[u8], at: usize, start: usize) -> Result<(String, usize), VariantError> {
    let len = read_u32(bytes, at, start)? as usize;
    let data = bytes
        .get(at + 4..at + 4 + len)
        .ok_or(VariantError::Truncated { at: start })?;
    let pad = (4 - (len % 4)) % 4;
    Ok((
        String::from_utf8_lossy(data).into_owned(),
        at + 4 + len + pad,
    ))
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
    // that oracle, or read out of a capture taken by
    // the sync probe. None was computed by this crate.

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
