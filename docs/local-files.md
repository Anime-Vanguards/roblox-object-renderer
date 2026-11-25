# Local file identity and cache invalidation

When `ror` draws a local model, place, map, texture, mesh, union, texture pack, skybox face, clothing template, or GUI image/font, every cache in the render service and on disk keys that input by a **file stamp** (`FileStamp` in `crates/ror-assets/src/stamp.rs`) rather than by path alone. Rewriting or replacing a local file invalidates every affected cache on the next draw without restarting the service or clearing the state directory.

## How a stamp is built

`FileStamp` follows one rule for every local file:

### 1. Volumes with a trustworthy kernel change token

- **Windows (NTFS / ReFS with an active USN journal):** `ror` opens the file's attributes (`CreateFileW` with `FILE_READ_ATTRIBUTES`, no read of file data) and queries:
  - Byte length (`FILE_STANDARD_INFO.EndOfFile`)
  - Modification time and metadata change time (`FILE_BASIC_INFO.LastWriteTime` and `ChangeTime`)
  - Volume serial number and 128-bit file identifier (`FILE_ID_INFO`)
  - Kernel-maintained update sequence number (`FSCTL_READ_FILE_USN_DATA`, `USN_RECORD_V2` or `V3`, requiring `Usn > 0`)
- **Unix (Linux / macOS):** `ror` reads `len`, `mtime`, `ctime`, `st_dev`, and `st_ino` (requiring `st_ino != 0` and valid `mtime` and `ctime`).

A volume is treated as **trusted** only when all of those fields succeed (on Windows, that requires an active USN journal returning `Usn > 0`).

- **File first seen older than 2 seconds:** Keyed directly by the OS metadata stamp (`StampKind::Os`). The file's bytes are not hashed on lookup. Because the Windows USN and Unix `ctime` advance on every write—including writes through another hard link, a junction, a `subst` drive, an open handle, a memory map, or a tool that restores `mtime`/`ChangeTime`—reusing an old cache entry after a real edit is prevented without reading the file.
- **File within the 2-second recent window (`modified` or `ChangeTime`/`ctime` less than 2 s old):** To close same-tick writes and open-handle updates, files up to **64 MiB** are streamed through BLAKE3 (64 KiB buffer, first 128 bits kept) and keyed by `(len, hash)` (`StampKind::Content`). Files larger than 64 MiB on a trusted volume fall back to their OS stamp during the recent window.
- **Settling past 2 seconds:** When a recently hashed file ages past the 2-second window with its OS stamp unchanged, `ror` hashes it once more on the first aged look (closing any write made in the final timer tick) and marks the `(volume, file_id) -> (os_stamp, hash)` entry settled. Subsequent lookups with the same OS stamp keep returning that content key without reading the file. If the OS stamp later changes, the settled entry is dropped.

### 2. Volumes without a trustworthy kernel change token

On FAT/exFAT, NTFS/ReFS volumes with no active USN journal, network shares that do not report a USN (SMB shares backed by NTFS with an active USN journal forward `USN_RECORD_V3` and stay trusted; non-USN shares fall back here), or platforms where `ChangeTime`/`ctime` is unavailable, `OsStamp::trusted` is `false`:

- **Every lookup streams the entire file through BLAKE3** and keys it by `(len, hash)`, with **no 64 MiB size cap** and regardless of file age.
- Draws remain accurate, but repeated looks pay full-file hashing on every lookup.
- To make that cost visible, each draw's JSON report includes `stats.untrustedStampHashes` (in both `ror render` and `ror video`), counting how many local files had to be hashed in full because their volume gave no trusted OS stamp. When the count is greater than zero, the CLI also prints a one-line notice to `stderr`.

## Identical-byte rewrites

Rewriting a file with identical bytes keeps the exact same cache key **only for a file that was first seen fresh (within 2 seconds of its write)**, because it is already keyed by `(len, hash)`.

A file that was **first seen aged** (older than 2 seconds on a trusted volume) starts under its OS stamp. Rewriting it with identical bytes advances the OS stamp and places it inside the 2-second window, moving its key from `Os(...)` to `Content(len, hash)`. That single draw reloads the file once; any further identical-byte rewrites (and aging back past 2 seconds) keep that content key and reuse the cache.

## Concurrent writes during a draw (A -> B -> A)

Every cache enforces one invariant: **what is kept under a key is what was read.**

Before reading a local file, `ror` captures a `FileSnapshot` carrying both the cache key and the raw OS metadata. After reading the bytes (and before inserting into the in-memory asset cache, `FILE_HASHES`, GPU mesh/texture/pack/skin/sky/material/effect caches, prepared scene/part caches, parsed scene/map/place caches, or GUI disk/store caches), `ror` verifies the read:

- **Content-stamped snapshot:** Checks that the exact bytes read match `(len, blake3(bytes))`.
- **Settled trusted OS snapshot:** Re-reads the OS stamp after reading and requires the monotonic OS token (USN + file ID on Windows; `ctime` + inode on Unix) and byte length to match the pre-read snapshot.
- **Untrusted or still-recent OS snapshot:** Cannot prove stillness without a content hash and is treated as unverified.

If a file moves between stamping and reading, the current draw finishes with the bytes it read, nothing is cached under the earlier key (memory, disk, or shared content store), and concurrent callers waiting on the same key retry instead of inheriting the unverified result. A subsequent draw after the file settles back to its earlier bytes (A -> B -> A) always reads and draws A.

## Local files inside GUIs

`SurfaceGui`, `BillboardGui`, `ScreenGui`, and `ViewportFrame` overlays can reference local images (including `ImageRectOffset` sprite sheets) and local fonts (`FontFace`, including local `.json` font definitions referencing local `.ttf`/`.otf` faces), whether in the input model or in a backdrop passed via `--map`.

- Every local file referenced by a GUI subtree is collected when the GUI is extracted and re-stamped live whenever the GUI is planned for a view (including when the parsed `Scene` is reused from `SceneCache`).
- Before invoking the UI renderer (`rur`), `ror` reads each referenced local file, verifies it against its snapshot, and stages an immutable content-addressed copy inside the atlas work directory so `rur` always decodes the verified bytes.
- If any local file moved before staging, the rendered GUI image is used for the current draw only and is never written to `<state dir>/guis/`, published to the shared content store, or inserted into the GPU cache.

## What callers can rely on

- **Ordinary saves, renames, hard links, junctions, `subst` drives, and timestamp-preserving copies** are detected automatically on the next draw without restarting the service or passing `--no-service`.
- **Editing a file while another draw is in flight** never poisons the service cache for later draws—even if the file is reverted to its earlier bytes.
- **Prepared texture files on disk (`<state dir>/textures/`)** are keyed by the BLAKE3 hash of the bytes actually read, so editing a local image never collides with a previously decoded texture file.
- **Watch `stats.untrustedStampHashes`** (or the `stderr` note) when running on removable drives, FAT/exFAT volumes, or network shares: a non-zero value explains why repeated warm draws are hashing local inputs in full. Moving working assets onto a local NTFS/ReFS volume with an active USN journal (Windows) or local POSIX filesystem (Linux/macOS) restores metadata-only lookups for aged files.

See also: [service.md](service.md) and [assets.md](assets.md).
