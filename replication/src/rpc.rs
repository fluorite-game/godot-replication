//! How a node's RPC methods are numbered, and the hash the two ends exchange.
//!
//! # The problem
//!
//! A remote call crosses as a method *id*, not a name. Both peers derive the
//! id from the node's RPC config, and check that they derived it the same way
//! by exchanging an MD5 of that config in the `SIMPLIFY_PATH` packet. Get
//! either wrong and every call lands on the wrong method -- silently, because
//! a wrong id is still a valid id.
//!
//! # Both rules were measured
//!
//! **The id is the index in *sorted* name order, not declaration order.**
//! `player.gd` declares `jump, land, shoot, hit, add_camera_shake_trauma`, and
//! a capture of a client holding the trigger carried method id 4 one hundred
//! and six times -- once per shot. Sorted, `shoot` is index 4; declared, it is
//! 2. The same capture carried id 3 exactly once, to the client's own player
//! just after it spawned: sorted, index 3 is `land`. Declared, index 3 is
//! `hit`, and nothing hit it.
//!
//! **The hash is the MD5 of the sorted names concatenated, UTF-8, and nothing
//! else.** `bullet.gd`'s config is one method with `rpc_mode` 2 and
//! `call_local` true, and its hash on the wire is
//! `f821b5159d85278da0badf5d32ffe210` -- exactly `md5("explode")`, so neither
//! the mode nor the flag is in the digest.
//!
//! That rule was then made falsifiable: hashes for all five of the demo's
//! scripts were predicted from it and searched for across three captures.
//! Every hash present is one of the predicted five, with none left over.

use md5::{Digest, Md5};

/// A remote call on the wire.
///
/// # Two forms, and the lead byte says which
///
/// ```text
/// 0x80  [u8 0x80][u8 cache_id][u8 method]                          3 bytes
/// 0xa0  [u8 0xa0][u32 0x8000_0000 | 6][u8 method][path][NUL]
/// ```
///
/// The long form is what a sender uses before the receiver has confirmed the
/// node's path, and it carries the path itself; the short one names a path id
/// the receiver has confirmed. In a capture of one shooting session the two
/// appear in almost equal numbers -- 108 long and 106 short -- because bullets
/// are spawned and destroyed constantly, and each is exploded before its path
/// comes back confirmed.
///
/// # The long form's 32-bit field is an offset, not an id
///
/// With the top bit set, the low 31 bits say *where in the packet the path
/// starts*. It is `0x80000006` in all 108 captured long calls, across six
/// different target paths -- a per-node id would differ between them -- and 6
/// is exactly one lead byte, four for the field and one for the method.
///
/// This was first read as a path id, because the one sample examined happened
/// to target the node announced as id 6. Sending real path ids there was what
/// exposed it: a stock client logged `Failed to get path from RPC:
/// ain/Level/SpawnedNodes/Bullet4.`, the path read from a different wrong
/// offset in each packet.
///
/// # Read against the path cache, which names every target
///
/// Cache id 5 is `main/Level/SpawnedNodes/466750851`, the client's own player,
/// so the 106 identical `80 05 04` packets are method 4 of `player.gd` --
/// which sorted is `shoot`, once per shot. The long form carries method 0 to
/// nodes whose leaf name starts with `Bullet` 107 times; all of them run
/// `bullet.gd`, whose single RPC is `explode`. Mostly these are the bullet
/// nodes themselves rather than the `BulletCache` they came from, because each
/// new bullet needs its own path announced before it can be addressed cheaply.
/// Method 3 goes to the player once, which sorted is `land`: a character lands
/// once after it spawns.
///
/// # Arguments are not implemented, because nothing sends any
///
/// Every RPC this demo actually calls with `.rpc()` takes no parameters, and
/// the one that takes a float -- `add_camera_shake_trauma` -- is only ever
/// called as a plain method (`player.gd:201`, `:206`, `red_robot.gd:133`). So no
/// captured packet carries an argument list, and there is nothing here to
/// check an implementation of one against. Writing it now would be a guess
/// with tests around it. [`RemoteCall::Path`] and [`RemoteCall::Cached`]
/// therefore stop at the method id, and a packet with trailing bytes is
/// refused rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCall {
    /// Addressed by a path-cache id the receiver has confirmed.
    Cached {
        /// The node's path-cache id.
        cache_id: u8,
        /// Index into the node's sorted RPC method list.
        method: u8,
    },
    /// Addressed by full path, before the cache is confirmed.
    Path {
        /// Index into the node's sorted RPC method list.
        method: u8,
        /// The node's path.
        path: String,
    },
}

/// Lead byte of a cached remote call.
pub const LEAD_CACHED: u8 = 0x80;
/// Lead byte of a remote call carrying its target's path.
pub const LEAD_PATH: u8 = 0xa0;

/// Where the path starts in a long-form call with no arguments.
pub const PATH_OFFSET: u32 = 6;

/// The long form's flag that the field below it is a path offset.
const PATH_FLAG: u32 = 0x8000_0000;

/// Why a remote call could not be read.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CallError {
    /// The lead byte was neither [`LEAD_CACHED`] nor [`LEAD_PATH`].
    UnknownForm {
        /// What was found.
        found: u8,
    },
    /// The packet ended inside a field.
    Truncated,
    /// The path was not NUL-terminated.
    Unterminated,
    /// The long form's path does not start straight after the method id.
    ///
    /// Anything between the method and the path would be an argument list,
    /// which this crate does not implement.
    UnexpectedOffset {
        /// The field as read, flag included.
        field: u32,
    },
    /// Bytes followed the packet's last known field.
    ///
    /// Refused rather than ignored: the only thing that could follow is an
    /// argument list, which this crate does not implement because no captured
    /// packet has one. Dropping them would turn an unimplemented feature into
    /// a call made with the wrong arguments.
    TrailingBytes {
        /// How many bytes were left over.
        count: usize,
    },
}

impl RemoteCall {
    /// Reads one remote call.
    ///
    /// # Errors
    ///
    /// Any of [`CallError`].
    pub fn parse(packet: &[u8]) -> Result<Self, CallError> {
        match packet.first() {
            Some(&LEAD_CACHED) => {
                if packet.len() < 3 {
                    return Err(CallError::Truncated);
                }
                if packet.len() > 3 {
                    return Err(CallError::TrailingBytes {
                        count: packet.len() - 3,
                    });
                }
                Ok(Self::Cached {
                    cache_id: packet[1],
                    method: packet[2],
                })
            }
            Some(&LEAD_PATH) => {
                let field = u32::from_le_bytes(
                    packet
                        .get(1..5)
                        .ok_or(CallError::Truncated)?
                        .try_into()
                        .map_err(|_| CallError::Truncated)?,
                );
                if field != PATH_FLAG | PATH_OFFSET {
                    return Err(CallError::UnexpectedOffset { field });
                }
                let method = *packet.get(5).ok_or(CallError::Truncated)?;
                let rest = packet.get(6..).ok_or(CallError::Truncated)?;
                let end = rest
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or(CallError::Unterminated)?;
                if end + 1 != rest.len() {
                    return Err(CallError::TrailingBytes {
                        count: rest.len() - end - 1,
                    });
                }
                Ok(Self::Path {
                    method,
                    path: String::from_utf8_lossy(&rest[..end]).into_owned(),
                })
            }
            Some(&found) => Err(CallError::UnknownForm { found }),
            None => Err(CallError::Truncated),
        }
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Cached { cache_id, method } => vec![LEAD_CACHED, *cache_id, *method],
            Self::Path { method, path } => {
                let mut out = Vec::with_capacity(7 + path.len());
                out.push(LEAD_PATH);
                out.extend_from_slice(&(PATH_FLAG | PATH_OFFSET).to_le_bytes());
                out.push(*method);
                out.extend_from_slice(path.as_bytes());
                out.push(0);
                out
            }
        }
    }
}

/// A node's RPC methods, held in the order the wire numbers them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RpcConfig {
    sorted: Vec<String>,
}

impl RpcConfig {
    /// Builds a config from method names in any order.
    ///
    /// The sort is part of the protocol, not a convenience: two peers holding
    /// the same methods in different orders must still agree, or the hash they
    /// exchange would pass while their numbering differed.
    #[must_use]
    pub fn new<I, S>(methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut sorted: Vec<String> = methods.into_iter().map(Into::into).collect();
        sorted.sort();
        Self { sorted }
    }

    /// The methods, in wire order.
    #[must_use]
    pub fn methods(&self) -> &[String] {
        &self.sorted
    }

    /// The id `method` travels under, or `None` if this node has no such RPC.
    #[must_use]
    pub fn id_of(&self, method: &str) -> Option<u16> {
        self.sorted
            .iter()
            .position(|m| m == method)
            .and_then(|i| u16::try_from(i).ok())
    }

    /// The method an id names, or `None` when the id is out of range.
    ///
    /// A decoder that cannot answer "no such method" has no way to notice it
    /// is desynchronized, which is the reason the hash exists.
    #[must_use]
    pub fn method_of(&self, id: u16) -> Option<&str> {
        self.sorted.get(usize::from(id)).map(String::as_str)
    }

    /// The MD5 Godot puts in `SIMPLIFY_PATH` for a node with this config.
    ///
    /// A node with no RPCs hashes the empty string, which is why
    /// `d41d8cd98f00b204e9800998ecf8427e` appears against every
    /// `MultiplayerSpawner` and `MultiplayerSynchronizer` in a capture.
    #[must_use]
    pub fn hash(&self) -> String {
        let mut hasher = Md5::new();
        for method in &self.sorted {
            hasher.update(method.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo's five configs, in declaration order as the `.gd` files write
    /// them. Read out of the engine by an RPC-config oracle run inside Godot.
    fn player() -> RpcConfig {
        RpcConfig::new(["jump", "land", "shoot", "hit", "add_camera_shake_trauma"])
    }

    #[test]
    fn the_id_is_the_sorted_index() {
        // What the wire said: id 4, one hundred and six times, once per shot.
        assert_eq!(player().id_of("shoot"), Some(4));
        assert_eq!(player().method_of(4), Some("shoot"));
        // And id 3 exactly once, to a player that had just spawned.
        assert_eq!(player().id_of("land"), Some(3));
        // Declaration order would have put `shoot` at 2 and `hit` at 3, and
        // nothing in the capture hit the player.
        assert_ne!(player().id_of("shoot"), Some(2));
    }

    #[test]
    fn input_order_cannot_change_the_result() {
        // Two peers may hold the same methods in different orders. If the sort
        // were not part of the rule, their hashes would match while their
        // numbering silently differed.
        let reversed = RpcConfig::new(["add_camera_shake_trauma", "hit", "shoot", "land", "jump"]);
        assert_eq!(reversed.hash(), player().hash());
        assert_eq!(reversed.id_of("shoot"), Some(4));
    }

    #[test]
    fn the_hash_is_md5_of_the_sorted_names_and_nothing_else() {
        // `bullet.gd` carries one RPC with rpc_mode 2 and call_local true, and
        // this is its hash on the wire -- which is md5("explode"), so neither
        // the mode nor the flag is in the digest.
        assert_eq!(
            RpcConfig::new(["explode"]).hash(),
            "f821b5159d85278da0badf5d32ffe210"
        );
        // Every MultiplayerSpawner and MultiplayerSynchronizer in a capture.
        assert_eq!(
            RpcConfig::default().hash(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        // Predicted from the rule, then found in three separate captures.
        assert_eq!(player().hash(), "c54b18d512c48639aa169b0e05e68195");
        assert_eq!(
            RpcConfig::new(["jump"]).hash(),
            "ba535ef5a9f7b8bc875812bb081286bb"
        );
        // Not yet seen on a wire: no capture has sent an RPC to a robot or a
        // death part, so these two are the rule's prediction.
        assert_eq!(
            RpcConfig::new(["hit", "play_shoot"]).hash(),
            "58f7e2b471c71ac7c69d8daef123a6cb"
        );
        assert_eq!(
            RpcConfig::new(["destroy"]).hash(),
            "fb14982288108e1fbd6207ef55f05027"
        );
    }

    #[test]
    fn an_id_nothing_declares_is_answerable() {
        assert_eq!(RpcConfig::new(["explode"]).method_of(4), None);
        assert_eq!(RpcConfig::new(["explode"]).id_of("shoot"), None);
    }
}
