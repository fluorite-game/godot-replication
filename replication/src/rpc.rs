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

use crate::variant::{decode_compact, encode_compact, Value};

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
/// # Arguments
///
/// No captured packet carries one: every RPC the demo this was measured from
/// actually calls takes no parameters. So the framing below was not read off a
/// capture but asked of the engine directly -- `oracle/rpc_oracle.gd` installs
/// a `MultiplayerPeerExtension`, which a stock `SceneMultiplayer` encodes into
/// exactly as it would a socket, and dumps what it was handed.
///
/// The lead byte is two flags over one command:
///
/// ```text
///   0x80  cached, no arguments   [80][cache id][method]
///   0x00  cached, with arguments [00][cache id][method][u8 count][args...]
///   0xa0  path,   no arguments   [a0][u32 offset|flag][method][path...]
///   0x20  path,   with arguments [20][u32 offset|flag][method][u8 count][args...][path...]
/// ```
///
/// Bit `0x20` says the target travels as a path rather than a cache id, and
/// bit `0x80` says there are no arguments. The offset field exists because in
/// the long form the path sits *after* the arguments, so its start cannot be
/// a constant once there are any.
///
/// Arguments are encoded in the compact Variant form -- the same one SYNC
/// uses, where a `bool` or a small `int` gets a one-byte header and everything
/// else keeps the plain four-byte one.
// Not `Eq`: an argument can be a float, and two calls differing only in a NaN
// argument are not equal to themselves either. `PartialEq` is what the wire
// comparison actually needs.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteCall {
    /// Addressed by a path-cache id the receiver has confirmed.
    Cached {
        /// The node's path-cache id.
        cache_id: u8,
        /// Index into the node's sorted RPC method list.
        method: u8,
        /// The call's arguments, in order.
        args: Vec<Value>,
    },
    /// Addressed by full path, before the cache is confirmed.
    Path {
        /// Index into the node's sorted RPC method list.
        method: u8,
        /// The node's path.
        path: String,
        /// The call's arguments, in order.
        args: Vec<Value>,
    },
}

/// Lead byte of a cached remote call with no arguments.
pub const LEAD_CACHED: u8 = 0x80;
/// Lead byte of a remote call carrying its target's path, with no arguments.
pub const LEAD_PATH: u8 = 0xa0;
/// Lead byte of a cached remote call that carries arguments.
pub const LEAD_CACHED_ARGS: u8 = 0x00;
/// Lead byte of a path-addressed remote call that carries arguments.
pub const LEAD_PATH_ARGS: u8 = 0x20;
/// The lead bit that means the target travels as a path, not a cache id.
pub const LEAD_BIT_PATH: u8 = 0x20;
/// The lead bit that means no argument list follows the method id.
pub const LEAD_BIT_NO_ARGS: u8 = 0x80;

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
    /// The long form's path offset is not where the fields say it should be.
    ///
    /// The offset has to agree with the argument list that precedes it. A
    /// packet whose offset points into the middle of an argument, or past the
    /// end, is refused rather than read from the offset and hoped for.
    UnexpectedOffset {
        /// The field as read, flag included.
        field: u32,
    },
    /// An argument could not be decoded.
    BadArgument {
        /// Which argument, counting from zero.
        index: usize,
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
        let lead = *packet.first().ok_or(CallError::Truncated)?;
        // Anything outside the two flag bits is a command this is not.
        if lead & !(LEAD_BIT_PATH | LEAD_BIT_NO_ARGS) != 0 {
            return Err(CallError::UnknownForm { found: lead });
        }
        let by_path = lead & LEAD_BIT_PATH != 0;
        let has_args = lead & LEAD_BIT_NO_ARGS == 0;

        if by_path {
            let field = u32::from_le_bytes(
                packet
                    .get(1..5)
                    .ok_or(CallError::Truncated)?
                    .try_into()
                    .map_err(|_| CallError::Truncated)?,
            );
            if field & PATH_FLAG == 0 {
                return Err(CallError::UnexpectedOffset { field });
            }
            let offset = (field & !PATH_FLAG) as usize;
            let method = *packet.get(5).ok_or(CallError::Truncated)?;
            let (args, after) = if has_args {
                read_args(packet, 6)?
            } else {
                (Vec::new(), 6)
            };
            // The arguments have to end exactly where the path begins. An
            // offset that disagrees means the two halves were read
            // differently, and reading the path from the offset anyway would
            // turn that into a call with the wrong arguments.
            if after != offset {
                return Err(CallError::UnexpectedOffset { field });
            }
            let rest = packet.get(offset..).ok_or(CallError::Truncated)?;
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
                args,
            })
        } else {
            let cache_id = *packet.get(1).ok_or(CallError::Truncated)?;
            let method = *packet.get(2).ok_or(CallError::Truncated)?;
            let (args, after) = if has_args {
                read_args(packet, 3)?
            } else {
                (Vec::new(), 3)
            };
            if after != packet.len() {
                return Err(CallError::TrailingBytes {
                    count: packet.len() - after,
                });
            }
            Ok(Self::Cached {
                cache_id,
                method,
                args,
            })
        }
    }

    /// Writes it back out.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Cached {
                cache_id,
                method,
                args,
            } => {
                if args.is_empty() {
                    return vec![LEAD_CACHED, *cache_id, *method];
                }
                let mut out = vec![LEAD_CACHED_ARGS, *cache_id, *method];
                write_args(args, &mut out);
                out
            }
            Self::Path { method, path, args } => {
                let mut body = Vec::new();
                if !args.is_empty() {
                    write_args(args, &mut body);
                }
                let offset = 6 + body.len();
                let mut out = Vec::with_capacity(offset + path.len() + 1);
                out.push(if args.is_empty() {
                    LEAD_PATH
                } else {
                    LEAD_PATH_ARGS
                });
                #[allow(clippy::cast_possible_truncation)]
                let field = PATH_FLAG | offset as u32;
                out.extend_from_slice(&field.to_le_bytes());
                out.push(*method);
                out.extend_from_slice(&body);
                out.extend_from_slice(path.as_bytes());
                out.push(0);
                out
            }
        }
    }
}

/// Reads `[u8 count][value; count]` from `at`, returning the values and the
/// offset past them.
fn read_args(packet: &[u8], at: usize) -> Result<(Vec<Value>, usize), CallError> {
    let count = *packet.get(at).ok_or(CallError::Truncated)? as usize;
    let mut args = Vec::with_capacity(count);
    let mut off = at + 1;
    for index in 0..count {
        let (value, next) =
            decode_compact(packet, off).map_err(|_| CallError::BadArgument { index })?;
        args.push(value);
        off = next;
    }
    Ok((args, off))
}

/// Writes `[u8 count][value; count]`.
///
/// The count is one byte, which is the engine's own limit rather than a
/// simplification here: a call with more than 255 arguments cannot be
/// expressed in this framing at all.
fn write_args(args: &[Value], out: &mut Vec<u8>) {
    out.push(u8::try_from(args.len()).unwrap_or(u8::MAX));
    for arg in args {
        out.extend_from_slice(&encode_compact(arg));
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
