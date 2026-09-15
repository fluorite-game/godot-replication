//! The SYNC packet's framing.
//!
//! # Shape
//!
//! ```text
//! 06 | 0200 | 01000080 | 05000000 | 82 | 44332211
//! ^    ^      ^          ^          ^^^^^^^^^^^^^
//! |    |      |          |          the body: compact Variants
//! |    |      |          body length, u32 little-endian
//! |    |      synchronizer net id, u32 little-endian
//! |    a counter, incrementing once per packet
//! the SceneMultiplayer command, 0x06
//! ```
//!
//! The `[net id][length][body]` record repeats: one per synchronizer with
//! something to send. A real game packet carried up to eleven of them.
//!
//! # How it was found
//!
//! Two hypotheses failed against the game's packets first. The framing above
//! was one of them -- it was right, and failed anyway, because the *bodies*
//! were being read as plain four-byte-header Variants when SYNC carries the
//! compact form. Rather than try a third layout against packets whose contents
//! were unknown, `tools/capture_sync_probe.sh` put two peers on one node with
//! one property and a value unmistakable in a hex dump, and the framing could
//! then be read instead of searched for.
//!
//! Validated where it counts: all 12814 SYNC packets in a two-peer capture of
//! the real demo parse to the exact byte under this rule, and the field shapes
//! that fall out are the `.tscn` property lists -- with the mode-0 fields
//! (`health`, `dead`, `player_id`) absent from the stream, as their
//! replication mode says they should be.

use crate::variant::{decode_compact, encode_compact, Value, VariantError};

/// The `SceneMultiplayer` command byte for a sync packet.
pub const COMMAND_SYNC: u8 = 0x06;

/// Bytes before the first record: the command and a `u16` counter.
pub const HEADER_LEN: usize = 3;

/// One synchronizer's worth of a sync packet.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRecord {
    /// The synchronizer's network id, as assigned when its path was cached.
    pub net_id: u32,
    /// Its replicated fields, in the order its `SceneReplicationConfig` lists
    /// them -- mode-`ALWAYS` properties only.
    pub fields: Vec<Value>,
}

/// A decoded sync packet.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPacket {
    /// Increments once per sync packet sent.
    pub counter: u16,
    /// One per synchronizer with something to say this tick.
    pub records: Vec<SyncRecord>,
}

/// Why a packet could not be read.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SyncError {
    /// The first byte was not [`COMMAND_SYNC`].
    NotSync {
        /// What it was instead.
        found: u8,
    },
    /// A record claimed more bytes than the packet holds.
    Truncated {
        /// Where the record began.
        at: usize,
    },
    /// The records did not end exactly on the packet's last byte.
    ///
    /// Reported rather than tolerated. Landing short means a field was decoded
    /// with the wrong width, and every value after it is then read from the
    /// wrong offset while still looking like a number.
    Overrun {
        /// Where parsing stopped.
        at: usize,
        /// How long the packet is.
        len: usize,
    },
    /// A field could not be decoded.
    Field(VariantError),
}

impl From<VariantError> for SyncError {
    fn from(error: VariantError) -> Self {
        Self::Field(error)
    }
}

/// Parses one SYNC packet.
///
/// # Errors
///
/// Any of [`SyncError`]. In particular this refuses a packet whose records do
/// not land exactly on its final byte, which is the check that makes a wrong
/// field width visible instead of silent.
///
/// # Panics
///
/// Does not: the `expect` calls convert slices whose length was checked on the
/// line above into fixed-size arrays.
pub fn parse(packet: &[u8]) -> Result<SyncPacket, SyncError> {
    let &command = packet.first().ok_or(SyncError::Truncated { at: 0 })?;
    if command != COMMAND_SYNC {
        return Err(SyncError::NotSync { found: command });
    }
    let counter = u16::from_le_bytes([
        *packet.get(1).ok_or(SyncError::Truncated { at: 1 })?,
        *packet.get(2).ok_or(SyncError::Truncated { at: 2 })?,
    ]);

    let mut records = Vec::new();
    let mut at = HEADER_LEN;
    while at < packet.len() {
        let head = packet.get(at..at + 8).ok_or(SyncError::Truncated { at })?;
        let net_id = u32::from_le_bytes(head[0..4].try_into().expect("checked"));
        let length = u32::from_le_bytes(head[4..8].try_into().expect("checked")) as usize;
        let body = packet
            .get(at + 8..at + 8 + length)
            .ok_or(SyncError::Truncated { at })?;

        let mut fields = Vec::new();
        let mut inner = 0usize;
        while inner < body.len() {
            let (value, next) = decode_compact(body, inner)?;
            fields.push(value);
            inner = next;
        }
        if inner != body.len() {
            return Err(SyncError::Overrun {
                at: inner,
                len: body.len(),
            });
        }

        records.push(SyncRecord { net_id, fields });
        at += 8 + length;
    }
    if at != packet.len() {
        return Err(SyncError::Overrun {
            at,
            len: packet.len(),
        });
    }
    Ok(SyncPacket { counter, records })
}

/// Writes a sync packet back out.
///
/// Byte-identical to what Godot sends for the same values: the crate's tests
/// re-encode 163 captured packets and require the output to match the capture
/// exactly. That covers the framing, the compact tags, the float encoding, the
/// vector and transform layouts and the field order -- all of it against bytes
/// Godot wrote.
///
/// It does *not* cover the compact int width rule, and the tests say so rather
/// than leaving it implied. Every one of the 738 ints in that fixture uses
/// width code 0, and that is not a thin capture: the only ints the demo
/// *streams* are `state` and `current_animation`, both small enums. Its one
/// large int, `player_id`, is replication mode 0 and rides the spawn packet,
/// so a SYNC stream from this demo can never carry a wider one. Codes 1 to 3
/// are pinned instead by `tools/capture_sync_probe.sh`, which replicated an
/// int of each magnitude on purpose, and those bytes are quoted in
/// `variant`'s unit tests.
#[must_use]
pub fn encode(packet: &SyncPacket) -> Vec<u8> {
    let mut out = vec![COMMAND_SYNC];
    out.extend_from_slice(&packet.counter.to_le_bytes());
    for record in &packet.records {
        let mut body = Vec::new();
        for field in &record.fields {
            body.extend_from_slice(&encode_compact(field));
        }
        out.extend_from_slice(&record.net_id.to_le_bytes());
        let length = u32::try_from(body.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&body);
    }
    out
}
