//! `SPAWN`: a node coming into existence on the far side.
//!
//! # Layout, measured over 112 captured packets
//!
//! ```text
//! [u8 0x04][u8 scene][u32 spawner][u32 net_id][u32 sync_count][u32 name_len]
//! [u32 sync_net_id x sync_count][name_len bytes of name, NUL included][state]
//! ```
//!
//! All 112 captured spawns fit this layout, and each field was pinned by
//! correlation:
//!
//! - `scene` indexes the spawner's `_spawnable_scenes` (`level.tscn:75`,
//!   three entries): 0 is a player, 1 a robot, 2 a bullet.
//! - `spawner` is 1 in every packet, which is the path-cache id
//!   `SIMPLIFY_PATH` gave `main/Level/MultiplayerSpawner`.
//! - `sync_count` synchronizer ids follow, and they are always
//!   `net_id + 1 ..= net_id + sync_count`: the node and each synchronizer
//!   under it get consecutive ids.
//!
//! # A correction worth keeping
//!
//! The first version of this module read a fixed five-word header and called
//! the third word a "kind", because in 111 packets it was 1 and the fifth
//! word was always `net_id + 1`. The 112th had a 2 there, a two-byte "name"
//! that was not a string, and a state that would not decode, so it was stored
//! unparsed as `Spawn::Raw`.
//!
//! That packet is the host's own player, `"1"`. It is the only spawned node
//! with two synchronizers the server owns: `ServerSynchronizer` and
//! `InputSynchronizer`. The two-byte non-name was its second synchronizer id,
//! `0b 00 00 00`, read half-way through. Nothing about it was unusual. The
//! misreading only surfaced when this crate had to *write* a spawn and needed
//! to know what that word meant.
//!
//! # The state is compact Variants, and the sizes prove it
//!
//! Every `spawn = true` property of every listed synchronizer, in order, in
//! the compact encoding SYNC uses:
//!
//! ```text
//! robot        transform 52 + health 2 + state 2 + target 16 + dead 1   =  73
//! bullet       transform 52                                            =  52
//! client       transform 52 + id 5 + model 52 + motion 12 + anim 2      = 123
//! host "1"     the same with id 2 (= 120), plus input 16x3 + 12 + 1 + 1 = 182
//! ```
//!
//! The client's player lists one synchronizer and the host's lists two
//! because `level.gd` gives each player's `InputSynchronizer` to that
//! player's peer: the server includes only the synchronizers it owns.

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
    /// The name was not NUL-terminated within its stated length.
    Unterminated,
}

/// `SPAWN`.
pub const COMMAND_SPAWN: u8 = 0x04;

/// A spawn packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawn {
    /// Index into the spawner's `_spawnable_scenes`.
    pub scene: u8,
    /// The spawner's path-cache id.
    pub spawner: u32,
    /// The id the spawned node will be addressed by.
    pub net_id: u32,
    /// Ids of the synchronizers under it that the sender owns, in order.
    pub sync_ids: Vec<u32>,
    /// The node's name, without its NUL.
    pub name: String,
    /// The `spawn = true` properties of those synchronizers, compact-encoded
    /// in order.
    ///
    /// Left as bytes: decoding them needs each synchronizer's field list,
    /// which the receiver has and the packet does not.
    pub state: Vec<u8>,
}

fn word(packet: &[u8], at: usize) -> Result<u32, SpawnError> {
    packet
        .get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(SpawnError::Truncated)
}

impl Spawn {
    /// Reads one `SPAWN` packet.
    ///
    /// # Errors
    ///
    /// Any of [`SpawnError`].
    pub fn parse(packet: &[u8]) -> Result<Self, SpawnError> {
        match packet.first() {
            Some(&COMMAND_SPAWN) => {}
            Some(&found) => return Err(SpawnError::WrongCommand { found }),
            None => return Err(SpawnError::Truncated),
        }
        let scene = *packet.get(1).ok_or(SpawnError::Truncated)?;
        let spawner = word(packet, 2)?;
        let net_id = word(packet, 6)?;
        let sync_count = word(packet, 10)? as usize;
        let name_len = word(packet, 14)? as usize;
        let mut at = 18;
        let mut sync_ids = Vec::with_capacity(sync_count.min(64));
        for _ in 0..sync_count {
            sync_ids.push(word(packet, at)?);
            at += 4;
        }
        let name = packet.get(at..at + name_len).ok_or(SpawnError::Truncated)?;
        let name = name.strip_suffix(&[0]).ok_or(SpawnError::Unterminated)?;
        at += name_len;
        Ok(Self {
            scene,
            spawner,
            net_id,
            sync_ids,
            name: String::from_utf8_lossy(name).into_owned(),
            state: packet[at..].to_vec(),
        })
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(19 + 4 * self.sync_ids.len() + self.name.len() + self.state.len());
        out.push(COMMAND_SPAWN);
        out.push(self.scene);
        out.extend_from_slice(&self.spawner.to_le_bytes());
        out.extend_from_slice(&self.net_id.to_le_bytes());
        let count = u32::try_from(self.sync_ids.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        let name_len = u32::try_from(self.name.len() + 1).unwrap_or(u32::MAX);
        out.extend_from_slice(&name_len.to_le_bytes());
        for id in &self.sync_ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
        out.extend_from_slice(self.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&self.state);
        out
    }
}
