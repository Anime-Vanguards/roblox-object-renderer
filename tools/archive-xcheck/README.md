# archive-xcheck

Checks `crates/ror-archive` against the draft Roblox archive (`.rbx`) support for rbx-dom
([rojo-rbx/rbx-dom#675](https://github.com/rojo-rbx/rbx-dom/pull/675)), the only other reader and
writer of the format that can be run. Roblox's page does not say how a scene marks the values it
keeps in other objects; the draft does, and `ror-archive` follows it. Run this again whenever the
draft moves or merges, and when Roblox Studio starts writing archives.

The draft's crates are not published, so the harness builds them from a clone beside it:

```sh
cd tools/archive-xcheck
git clone --depth 1 --branch feature/rbx-archive https://github.com/pwani-rblx/rbx-dom rbx-dom
cargo run --release                                    # eight exchanges, both ways
cargo run --release --bin roundtrip -- 256 <file>...   # real places and models
cargo run --release --bin fixtures -- <out dir>        # small archives for other readers' tests
```

- The default binary builds a small place and passes it between the two implementations: this
  crate's archives (plain, with values moved out, ZIP64) through the draft reader, and the draft
  writer's archives (plain, with opaque content) through this crate. It ends with `ALL CHECKS PASSED`.
- `roundtrip <min bytes> <file>...` wraps each binary `.rbxl` or `.rbxm` twice, once moving out text
  and bytes of at least that size and once moving out every string value, flattens both, and
  compares everything a DOM parser reads before and after by one hash. The draft reader must agree
  on the first archive; it types its properties and refuses the second by design.
- `fixtures <out dir>` writes the archives another reader's tests can embed, with the plain files
  they hold.

The clone and the build output are not committed.
