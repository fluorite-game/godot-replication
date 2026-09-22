# godot-replication

Godot 4.5's high-level multiplayer protocol, in Rust, **measured off the wire
rather than read from engine source**.

`SceneMultiplayer` is how a Godot game replicates spawns, synchronized
properties and `@rpc` calls. It is an engine internal: undocumented, and with
no promise of stability between releases. This crate implements the parts of it
that were observed in real traffic, so that a non-Godot program can play
against a **stock, unmodified** Godot build.

```toml
[dependencies]
godot_replication = { git = "https://github.com/fluorite-game/godot-replication" }
```

## The two crates

| crate | what it is | depends on |
| --- | --- | --- |
| `godot_replication` | the protocol itself — Variant codec, RPC numbering, SYNC/SPAWN/path framing | `md-5`, nothing else |
| `godot_replication_ext` | a `MultiplayerApiExtension` GDExtension that routes Godot's own spawners, synchronizers and `@rpc` calls through it | `godot` 0.5, `api-4-5` |

The split is so the protocol can be worked on without gdext in the build:
`cargo test -p godot_replication` needs no Godot and finishes instantly, where
anything touching gdext is a multi-minute compile.

## Scope, stated plainly

This implements the subset that appeared in the traffic it was measured from.
That subset is smaller than Godot's, and the gaps are sharp edges rather than
gentle degradation:

* **Variant types: all of them but four.** Every id from `Nil` through
  `PackedVector4Array` round-trips against bytes the engine wrote. The four
  refused are `Object`, `Callable`, `Signal` and `RID`, which encode a pointer
  or an instance id that means nothing at the far end; `var_to_bytes` writes
  them only with `full_objects` set, and a replication protocol that accepted
  them would be handing a remote peer a deserialization primitive. They decode
  to `VariantError::UnknownType`, and there is no guessing past it: an unknown
  type has an unknown length, so continuing would turn one unreadable value
  into an unreadable packet.
* **RPC arguments are not implemented.** Method ids, the config hash and the
  call framing are; an argument list is not, because no captured packet had
  one. A call with arguments is refused rather than silently truncated.
* **No compression and no fragmentation.** Neither appeared in 17266 captured
  packets, and the largest was 1057 bytes against a 1392-byte MTU.

If you need those, the crate will tell you — loudly, at the point of failure —
rather than hand you a plausible wrong number.

## Why "measured, not read" matters

No part of this was taken from engine source or documentation. It comes from
packet captures of two peers playing a real game, and of two peers replicating
one known value so a single field could be isolated.

That provenance is the point, because of how this code fails. A field decoded
one byte out of place yields a plausible number rather than an error, and a
wrong RPC id is still a valid id at the far end. Nothing complains; the game
just behaves slightly wrongly, forever. So every constant here is pinned to a
byte somebody watched arrive, and the fixtures under
`replication/tests/fixtures/` are those bytes.

`cargo test -p godot_replication` re-encodes every captured packet and asserts
it reproduces byte for byte.

For the Variant codec the ground truth is regenerable rather than only
captured. `oracle/` is a minimal Godot project -- no assets, no autoloads --
that calls `var_to_bytes()` over every type at two values each, one zero and
one whose every field differs:

```
godot --headless --path oracle res://variant_oracle.tscn
```

Its output is `replication/tests/fixtures/variant_types.hex`, and the tests
assert that decoding consumes exactly the bytes the engine wrote -- not fewer,
which in a packet would leave the next field reading its header from the wrong
offset.

The capture harness itself — the scripts that drive two peers, dump the corpus
and diff one implementation's traffic against stock Godot's — lives with the
game it was built for and is not part of this repository.

## Versions

Measured against Godot **4.5.2**. `compatibility_minimum` is 4.5. The protocol
is an engine internal, so treat a different minor as unverified until its
traffic has been through the same fixtures.

## License

Apache-2.0.
