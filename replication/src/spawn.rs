//! `SPAWN`: a node coming into existence on the far side.
//!
//! # Layout, measured over 112 captured packets
//!
//! ```text
//! [u8 0x04][u8 scene][u32 spawner][u32 net_id][u32 kind][u32 name_len][u32 net_id + 1]
//! [name_len bytes of name][state]
//! ```
//!
//! Each field was pinned by correlation rather than by reading:
//!
//! - `scene` indexes the spawner's `_spawnable_scenes`, which `level.tscn:75`
//!   declares with exactly three entries. Observed 0, 1 and 2, and the names
//!   sort accordingly: 1 spawns `RedRobot`..`RedRobot4`, 2 spawns
//!   `Bullet`..`Bullet4`, 0 spawns the node named after a peer id.
//! - `spawner` is 1 in every packet, which is the path-cache id that
//!   `SIMPLIFY_PATH` assigned to `main/Level/MultiplayerSpawner`.
//! - `name_len` equals the name's length including its NUL in all 112.
//! - the last field is `net_id + 1` in all 112 -- each spawn takes two
//!   consecutive ids, the node and one more.
//!
//! # The state is compact Variants, and the sizes prove it
//!
//! The spawn carries every `spawn = true` property, in config order, in the
//! same compact encoding SYNC uses. Predicted from the field lists and matched
//! against the captured byte counts exactly:
//!
//! ```text
//! robot   transform 52 + health 2 + state 2 + target 16 + dead 1 = 73
//! bullet  transform 52                                          = 52
//! player  transform 52 + id 5 + model 52 + motion 12 + anim 2    = 123
//! ```
//!
//! Note `health` and `dead` appear here and never in a SYNC packet. That is
//! what replication mode 0 means, seen from the other side: they cross once,
//! with the spawn.
//!
//! # One packet in 112 is a different shape, and is not guessed at
//!
//! Its `kind` field is 2 where every other is 1, its name field is two bytes
//! that are not a string (`0b 00`), and its 186-byte state does not walk as
//! Variants. Two readings have been tried and neither held.
//!
//! So it is carried as [`Spawn::Raw`]: preserved exactly, re-encoded byte for
//! byte, and not pretended to be understood. Naming it after a guess would
//! make every later reader believe it had been decoded.

/// Why a spawn packet could not be read.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SpawnError {
    /// The first byte was not [`COMMAND_SPAWN`].
    WrongCommand {
        /// What was found.
        found: u8,
    },
    /// The packet ended inside a field.
    Truncated,
}

/// `SPAWN`.
pub const COMMAND_SPAWN: u8 = 0x04;

/// The `kind` field of a spawn this crate decodes.
pub const KIND_NAMED: u32 = 1;

/// A spawn packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spawn {
    /// The ordinary form: a named node with a state blob.
    Named {
        /// Index into the spawner's `_spawnable_scenes`.
        scene: u8,
        /// The spawner's path-cache id.
        spawner: u32,
        /// The id the spawned node will be addressed by.
        net_id: u32,
        /// The node's name, without its NUL.
        name: String,
        /// The `spawn = true` properties, compact-encoded in config order.
        ///
        /// Kept as bytes rather than decoded here: reading them needs the
        /// node's field list, which is the receiver's business and not the
        /// packet's.
        state: Vec<u8>,
    },
    /// A form this crate does not decode, preserved exactly.
    ///
    /// See the module docs. One packet in the 112-packet capture is this.
    Raw {
        /// The whole packet, unaltered.
        bytes: Vec<u8>,
    },
}

impl Spawn {
    /// Reads one `SPAWN` packet.
    ///
    /// # Errors
    ///
    /// [`SpawnError::WrongCommand`] or [`SpawnError::Truncated`].
    pub fn parse(packet: &[u8]) -> Result<Self, SpawnError> {
        match packet.first() {
            Some(&COMMAND_SPAWN) => {}
            Some(&found) => return Err(SpawnError::WrongCommand { found }),
            None => return Err(SpawnError::Truncated),
        }
        let head = packet.get(1..22).ok_or(SpawnError::Truncated)?;
        let scene = head[0];
        let word = |i: usize| {
            u32::from_le_bytes([
                head[1 + i * 4],
                head[2 + i * 4],
                head[3 + i * 4],
                head[4 + i * 4],
            ])
        };
        let (spawner, net_id, kind, name_len) = (word(0), word(1), word(2), word(3));
        if kind != KIND_NAMED {
            return Ok(Self::Raw {
                bytes: packet.to_vec(),
            });
        }
        let name_len = name_len as usize;
        let name = packet.get(22..22 + name_len).ok_or(SpawnError::Truncated)?;
        // The stored length includes the NUL; the name does not.
        let name = name.strip_suffix(&[0]).unwrap_or(name);
        Ok(Self::Named {
            scene,
            spawner,
            net_id,
            name: String::from_utf8_lossy(name).into_owned(),
            state: packet[22 + name_len..].to_vec(),
        })
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Raw { bytes } => bytes.clone(),
            Self::Named {
                scene,
                spawner,
                net_id,
                name,
                state,
            } => {
                let mut out = Vec::with_capacity(23 + name.len() + state.len());
                out.push(COMMAND_SPAWN);
                out.push(*scene);
                out.extend_from_slice(&spawner.to_le_bytes());
                out.extend_from_slice(&net_id.to_le_bytes());
                out.extend_from_slice(&KIND_NAMED.to_le_bytes());
                let name_len = u32::try_from(name.len() + 1).unwrap_or(u32::MAX);
                out.extend_from_slice(&name_len.to_le_bytes());
                // Each spawn takes two consecutive ids; the second is always
                // the first plus one in every captured packet.
                out.extend_from_slice(&net_id.wrapping_add(1).to_le_bytes());
                out.extend_from_slice(name.as_bytes());
                out.push(0);
                out.extend_from_slice(state);
                out
            }
        }
    }
}
