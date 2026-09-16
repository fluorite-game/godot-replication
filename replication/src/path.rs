//! The path cache, and despawn.
//!
//! # Why a cache exists at all
//!
//! Godot addresses a node on the wire by a small integer, not by its path.
//! `SIMPLIFY_PATH` is how the two ends agree on that integer: the sender
//! announces "id 4 means `main/Level/SpawnedNodes/466750851/BulletCache`, and
//! its RPC config hashes to this", and the receiver answers `CONFIRM_PATH`
//! with the same id once it has resolved it. Afterwards a remote call to that
//! node is three bytes instead of forty.
//!
//! The MD5 rides along because the id is only half the agreement -- the other
//! half is that both ends number the node's RPC methods identically. See
//! [`crate::rpc`].
//!
//! # Layout, measured
//!
//! Read off a capture by matching each packet's length against its path, which
//! is what made the trailing NULs visible:
//!
//! ```text
//! SIMPLIFY_PATH  [u8 0x01][32 bytes ASCII md5][NUL][u32 id][path][NUL]
//! CONFIRM_PATH   [u8 0x02][u8 valid][u32 id]                    6 bytes
//! DESPAWN        [u8 0x05][u32 id]                              5 bytes
//! ```
//!
//! The md5 is a fixed 32 ASCII characters with its own NUL, not a
//! length-prefixed string; the path is NUL-terminated too. A reader that takes
//! Godot's usual `[u32 length][bytes]` string encoding here is off by four and
//! lands mid-path.

/// Why a path packet could not be read.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum PathError {
    /// The first byte was not the expected command.
    WrongCommand {
        /// What was found.
        found: u8,
    },
    /// The packet ended inside a field.
    Truncated,
    /// A NUL-terminated field ran to the end without a terminator.
    Unterminated,
    /// The md5 field held bytes that are not ASCII hex.
    BadHash,
}

/// `SIMPLIFY_PATH`: this id now means this node.
pub const COMMAND_SIMPLIFY_PATH: u8 = 0x01;
/// `CONFIRM_PATH`: the id was resolved, or was not.
pub const COMMAND_CONFIRM_PATH: u8 = 0x02;
/// `DESPAWN`: the node behind this id is gone.
pub const COMMAND_DESPAWN: u8 = 0x05;

/// An announcement that an id stands for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimplifyPath {
    /// The id this node will be addressed by from now on.
    pub id: u32,
    /// The node's path, as the sender's scene tree spells it.
    pub path: String,
    /// The MD5 of the node's sorted RPC method names. See [`crate::rpc`].
    pub rpc_hash: String,
}

/// The answer to a [`SimplifyPath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmPath {
    /// The id being answered.
    pub id: u32,
    /// Whether the receiver resolved the path.
    ///
    /// A false here is not a transport failure -- it means the two scene trees
    /// disagree about what exists, which is the failure this whole exchange is
    /// designed to surface early rather than at the first RPC.
    pub valid: bool,
}

fn nul_terminated(bytes: &[u8], at: usize) -> Result<(&[u8], usize), PathError> {
    let end = bytes
        .get(at..)
        .ok_or(PathError::Truncated)?
        .iter()
        .position(|&b| b == 0)
        .ok_or(PathError::Unterminated)?;
    Ok((&bytes[at..at + end], at + end + 1))
}

impl SimplifyPath {
    /// Reads one `SIMPLIFY_PATH` packet.
    ///
    /// # Errors
    ///
    /// [`PathError`] variants for a wrong command byte, a short packet, an
    /// unterminated field, or an md5 that is not ASCII hex.
    pub fn parse(packet: &[u8]) -> Result<Self, PathError> {
        match packet.first() {
            Some(&COMMAND_SIMPLIFY_PATH) => {}
            Some(&found) => return Err(PathError::WrongCommand { found }),
            None => return Err(PathError::Truncated),
        }
        let hash = packet.get(1..33).ok_or(PathError::Truncated)?;
        if !hash.iter().all(u8::is_ascii_hexdigit) {
            return Err(PathError::BadHash);
        }
        if packet.get(33) != Some(&0) {
            return Err(PathError::Unterminated);
        }
        let id = u32::from_le_bytes(
            packet
                .get(34..38)
                .ok_or(PathError::Truncated)?
                .try_into()
                .map_err(|_| PathError::Truncated)?,
        );
        let (path, _) = nul_terminated(packet, 38)?;
        Ok(Self {
            id,
            path: String::from_utf8_lossy(path).into_owned(),
            rpc_hash: String::from_utf8_lossy(hash).into_owned(),
        })
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(39 + self.path.len());
        out.push(COMMAND_SIMPLIFY_PATH);
        out.extend_from_slice(self.rpc_hash.as_bytes());
        out.push(0);
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(self.path.as_bytes());
        out.push(0);
        out
    }
}

impl ConfirmPath {
    /// Reads one `CONFIRM_PATH` packet.
    ///
    /// # Errors
    ///
    /// [`PathError::WrongCommand`] or [`PathError::Truncated`].
    pub fn parse(packet: &[u8]) -> Result<Self, PathError> {
        match packet.first() {
            Some(&COMMAND_CONFIRM_PATH) => {}
            Some(&found) => return Err(PathError::WrongCommand { found }),
            None => return Err(PathError::Truncated),
        }
        let valid = *packet.get(1).ok_or(PathError::Truncated)? != 0;
        let id = u32::from_le_bytes(
            packet
                .get(2..6)
                .ok_or(PathError::Truncated)?
                .try_into()
                .map_err(|_| PathError::Truncated)?,
        );
        Ok(Self { id, valid })
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![COMMAND_CONFIRM_PATH, u8::from(self.valid)];
        out.extend_from_slice(&self.id.to_le_bytes());
        out
    }
}

/// Reads one `DESPAWN` packet, returning the id that is going away.
///
/// # Errors
///
/// [`PathError::WrongCommand`] or [`PathError::Truncated`].
pub fn parse_despawn(packet: &[u8]) -> Result<u32, PathError> {
    match packet.first() {
        Some(&COMMAND_DESPAWN) => {}
        Some(&found) => return Err(PathError::WrongCommand { found }),
        None => return Err(PathError::Truncated),
    }
    Ok(u32::from_le_bytes(
        packet
            .get(1..5)
            .ok_or(PathError::Truncated)?
            .try_into()
            .map_err(|_| PathError::Truncated)?,
    ))
}

/// Writes a `DESPAWN` packet.
#[must_use]
pub fn encode_despawn(id: u32) -> Vec<u8> {
    let mut out = vec![COMMAND_DESPAWN];
    out.extend_from_slice(&id.to_le_bytes());
    out
}
