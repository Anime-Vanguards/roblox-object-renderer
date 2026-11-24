# Roblox archives (`.rbx`)

A Roblox archive is one file that holds a place or a model together with the content it carries. Roblox describes version 1 of the format at [create.roblox.com/docs/rbx](https://create.roblox.com/docs/rbx). `ror` reads an archive wherever it reads a model or a place, and the `ror-archive` crate lets other tools do the same with one call.

```sh
ror render Place.rbx                         # the place inside, as `ror render Place.rbxl` draws it
ror render Model.rbxm --map Lobby.rbx        # an archive as the backdrop
ror cost Place.rbx
```

An asset id whose download is an archive is read the same way. Nothing about a file's name matters: `ror` looks at its first bytes.

## Archive layout

An archive is a ZIP file whose members are stored without compression:

- `manifest.jsonl` at the root: one JSON object per line. The first line is the `header` (the format version and the digest algorithm), the second the `package` (what the archive holds: `"type":"place"` with the path of the place's scene), and every other line a `content` record.
- `objects/<digest>`: the bytes of each content record, named by their digest. Two records with the same bytes share one object.

A `content` record gives embedded bytes a logical path (`"path":"Place.rbxl"`), points at a Roblox asset (`"asset":{"id":…}`), or both. The scene of a place is an ordinary binary `.rbxl` among the objects.

```jsonl
{"kind":"header","version":1,"digestAlgorithm":"blake3-128"}
{"kind":"package","type":"place","place":"Place.rbxl"}
{"kind":"content","path":"Place.rbxl","objectDigest":"7d0e3e7bb0c44be29aa3a4d9c3a1a8a1"}
{"kind":"content","objectDigest":"0f1e2d3c4b5a69788796a5b4c3d2e1f0"}
```

## Flattening: values kept outside the scene

The scene inside an archive may depend on the objects beside it. A writer can move a property's bytes (a script's `Source`, a mesh part's collision data) out of the scene into an object of their own and leave an empty placeholder behind. The property's column in the scene then starts with a marker byte that parsers written before archives do not know. Those parsers skip the whole property and report no error, so a scene taken out of an archive by hand can load with every script empty.

`ror` therefore reads an archive in two steps:

1. It checks the container and the manifest (below) and finds the scene the package names.
2. It writes every value kept in an object back into the scene (*flattening*). The result is a standalone `.rbxl` or `.rbxm`, which the usual parser reads.

A scene with nothing moved out is used as it is stored, without a copy.

The layout of the marker is not part of Roblox's page yet. `ror` follows the draft archive support for rbx-dom ([rojo-rbx/rbx-dom#675](https://github.com/rojo-rbx/rbx-dom/pull/675)), and its tests read that implementation's archives and have it read `ror`'s:

- The marker is the byte `0x82` where the property's type byte would be. A list of references follows, one per instance or a run of instances without one, and then the type byte and the values as usual.
- A reference holds a version, the first 16 bytes of the object's BLAKE3 digest and the object's size. Version 2 adds an identity string.
- Version 1 references stand for plain bytes in a string or shared string column. Flattening restores these.
- Version 2 references stand for content kept as an opaque object. Its layout is not documented, and a standalone file has no place for it, so these values read as empty content. Each property they occur in is reported (see [Warnings and errors](#warnings-and-errors)).

`ror` follows a reference only when all of this holds:

- The archive holds the object, and the object has the size and the BLAKE3 digest the reference gives.
- The value the reference stands for is the empty placeholder of its type: an empty string, an empty shared string, or a content value of `None`.
- For a shared string, the scene has one shared string table for the bytes to go into.

Anything else is an error, and so is a marker or a reference of any other kind: `ror` does not guess at values it cannot restore.

## Validation

| Part | Rule |
|---|---|
| Container | A ZIP, ZIP64 included, with every member stored uncompressed and unencrypted, in one file. Member names are UTF-8 with `/` separators, at most 128 characters, without empty, `.` or `..` parts, and none twice. Each member's CRC-32 must match. |
| Manifest | `header`, then `package`, then `content` records. `version` must be 1. A JSON key may not repeat within an object. Logical paths are relative, use `/` separators, have no empty, `.` or `..` parts, and are unique. The 128-character limit is for member names and does not apply to them. |
| Objects | Every `objectDigest` must name a member under `objects/`, and the member's bytes must match the digest (`blake3-128`: the first 16 bytes of BLAKE3, in lowercase hex). Roblox's page names no algorithm. `blake3-128` is the one the draft uses and the only one `ror` computes. |
| Package | `place`: the `place` field names the content record of the scene. `model`: see below. Any other type is refused. |
| Scene | A binary (`<roblox!`) or XML (`<roblox`) Roblox file. Only a binary scene can hold references. |

Roblox's page gives a model package no layout yet. `ror` finds a model's scene by the package's `model` field, else by the single content record whose path ends in `.rbxm` or `.rbxmx`, else by the single object that is a Roblox file.

Some things a strict reader refuses are read past and reported: members other than the manifest and the objects, objects no record names, fields the format does not define in the header or in a content record, and a digest algorithm `ror` does not know. Under such an algorithm the objects are not checked against their names. An object a property refers to is still checked against the BLAKE3 digest in its reference, and a reference that names no member is an error.

`ror` lists the manifest's asset references and does not download them. A scene's own content ids are fetched as for any other file ([assets.md](assets.md)).

### Limits

| Limit | Default |
|---|---|
| The archive | 4 GiB |
| `manifest.jsonl` | 64 MiB |
| One manifest record | 1 MiB |
| Members, and content records | 1,000,000 |
| One decompressed chunk of the scene | 1 GiB |
| Bytes written back into the scene | 2 GiB |

Members are stored uncompressed, so an archive cannot unpack to more than its own size. The scene's chunks are compressed, which is what the last two limits bound. Bytes written back are counted before they are copied: a string once for each value that holds it, a shared string once however many values share it.

## Warnings and errors

A render's `summary.json` lists these first among its `warnings`:

```text
MeshPart.MeshContent: 3 values kept in the archive as content objects (88211 bytes) that have no documented layout; read as empty
Roblox archive: 1 object(s) have no content record
```

A part whose mesh is such an object is drawn without it. An archive that breaks a rule in the table above is an error that names the rule:

```text
Roblox archive: invalid .rbx container: member objects/5b1c… is compressed (method 8): archive members are stored uncompressed
```

## The `ror-archive` crate

`crates/ror-archive` holds the reader and a writer. It parses no DOM and depends on no Roblox library, so a tool keeps the parser it has:

```rust
let scene = ror_archive::unwrap(&bytes)?;    // a .rbxl/.rbxm/.rbxlx/.rbxmx passes through untouched
let dom = rbx_binary::from_reader(&*scene.bytes)?;
for content in &scene.report.embedded_content { /* values read as empty */ }
```

- `unwrap(bytes)` returns the input itself for a plain Roblox file and the flattened scene for an archive.
- `Archive::open(bytes)` gives the manifest, the objects and content by logical path, `root()` the scene as stored, and `flatten()` the standalone scene.
- `Builder::place(scene)` and `Builder::model(scene)` write archives. `externalize(min_bytes)` moves text and byte values of at least that size into objects, as a writer that saves incrementally does. `content` and `asset` add records. The same input always makes the same bytes.

An example binary inspects, flattens and writes archives:

```sh
cargo run -p ror-archive --release --example rbx -- info Place.rbx
cargo run -p ror-archive --release --example rbx -- flatten Place.rbx Place.rbxl
cargo run -p ror-archive --release --example rbx -- pack Place.rbxl Place.rbx --externalize 1024
```

## Not supported

- Opaque content objects (version 2 references) are reported and read as empty.
- Package types other than `place` and `model`.
- Writing archives from `ror` itself: its outputs (`scene.rbxm`, exported models) stay plain files.
