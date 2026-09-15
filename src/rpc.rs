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
    /// them. Read out of the engine by `tools/rpc_config_oracle.gd`.
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
