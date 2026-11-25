# Assets, caches, and disk use

This page explains how `ror` resolves and downloads Roblox assets, caches GPU textures, shares files across checkouts, and prevents disk usage from growing unbounded. For how local files on disk are stamped and invalidated across memory, disk, and shared caches, see [local-files.md](local-files.md).

Relevant code lives in `crates/ror-assets` (content URIs, HTTP downloads, disk cache, content store, Roblox install detection, cookies, Roblox Studio's signed-in account, broker auth, and Open Cloud API keys), `crates/ror-image`, and `crates/ror/src/render/texels.rs` (GPU texture preparation).

Throughout this page, `<state dir>` refers to the active [state directory](building.md#standalone-commands-and-state) (`<checkout>/.cache` for source builds).

## Downloads and built-in content

Assets are downloaded from Roblox on first use and cached under `<state dir>/assets`. Before rendering a still or video, `ror` prefetches missing scene assets concurrently via Roblox's `assetdelivery` batch endpoint (for example, fetching 615 assets for a 19.7k-part map in ~4 s instead of ~40 s sequentially).

Built-in textures, skies, and avatar meshes are read directly from your local Roblox installation (Studio first, falling back to Player). Pass `--offline` to disable network access and use cached files only.

### Refused assets

When Roblox refuses an asset, `ror` records it as missing with the reason returned by `assetdelivery`:

- **Permanent refusals** (`HTTP 403, rejected by Roblox moderation`, `archived by its creator`, `not yet reviewed by Roblox moderation`): refused for all accounts and places. Studio draws nothing for these assets, and `ror` matches that behavior.
- **Authorization failures** (`not authorized for this account`, `authentication required`): require an [Open Cloud API key](#open-cloud-api-key), a `.ROBLOSECURITY` cookie, [Roblox Studio's account](#roblox-studios-account), an [account broker](#account-broker), or a different account with access ([building.md](building.md)).

### Which credential is tried

`ror` asks for every asset without a credential first. Only when Roblox answers that the asset needs authentication (401, 403) does it try a credential, in this order, until one gets the asset:

1. An [Open Cloud API key](#open-cloud-api-key), when one is set: it is the narrower credential.
2. The account: an [account broker](#account-broker)'s token when broker credentials are found, else the `.ROBLOSECURITY` cookie: from the environment or env files, else, only with `ROR_STUDIO_COOKIE=1`, [Roblox Studio's account](#roblox-studios-account).

A refusal is remembered for a day once a credential was tried too; `--refresh` asks again.

### Open Cloud API key

An [Open Cloud API key](https://create.roblox.com/docs/cloud/auth/api-keys) with the `legacy-asset:manage` scope downloads the assets its owner can access, without a cookie. Lookup order:

1. The `ROBLOX_ASSET_DELIVERY_API_KEY` environment variable.
2. A `ROBLOX_ASSET_DELIVERY_API_KEY=<key>` line in the env files the cookie is read from: the file named by `ROR_COOKIE_ENV`, then a source checkout's `.env`.

A value that is not a usable header value (whitespace, `;`, `,`, `"`) is skipped. `ror` reports where the key came from or why a source was skipped, never the key itself.

How the key is used:
- It is sent only as the `x-api-key` header, only over HTTPS to `https://apis.roblox.com/asset-delivery-api/v1/assetId/{id}` (or `.../version/{n}` for a versioned asset), with the same query `ror` sends assetdelivery for KTX2 transcodes, and only after the request without a credential was refused.
- Redirects are followed without the header. The asset bytes come from the returned CDN URL without it.
- That route has no batch endpoint, so prefetching asks the key one asset at a time: a batch without a credential first, then the key for each asset that batch refused, then a batch with the account's credential (if any) for what the key could not get.
- Roblox documents a limit of 1000 requests a minute per key owner. `ror` paces its requests by the `x-ratelimit-remaining` and `x-ratelimit-reset` response headers, and on `HTTP 429` holds back every request with the key until `Retry-After` (or the window's reset) before retrying, never longer than a minute at a time.
- Child processes `ror` starts (ffmpeg, Studio, rur, ree) do not inherit `ROBLOX_ASSET_DELIVERY_API_KEY`. `ror`'s own render service does, since it downloads for its callers. rur and ree are still given the env files' paths, as for the cookie.

### Account broker

When a team shares a single Roblox account for private assets, an account broker lets you keep the `.ROBLOSECURITY` cookie off developer machines. The broker holds the Roblox session and proxies asset requests using per-user bearer tokens.

When `ror` finds broker credentials, it ignores any local cookie. Lookup order:

1. `ROBLOX_BROKER_TOKEN` and `ROBLOX_BROKER_URL` environment variables.
2. JSON file at `ROBLOX_BROKER_CREDENTIALS`, defaulting to `~/.config/roblox-broker/credentials.json` (`%USERPROFILE%\.config\roblox-broker\credentials.json` on Windows):
   ```json
   { "url": "https://...", "token": "..." }
   ```
   (Extra keys are ignored.)

The broker URL must use `https://`, or `http://` targeting `127.0.0.1` or `localhost` (e.g., an SSH tunnel). Any other URL is rejected as unusable rather than falling back to a cookie.

**Broker protocol:**
- `ror` only sends requests to the broker after an unauthenticated Roblox request fails.
- Requests targeting `https://assetdelivery.roblox.com/<path>?<query>` are sent to `<url>/rbx/assetdelivery.roblox.com/<path>?<query>` with `Authorization: Bearer <token>`, preserving the HTTP method, body, and `Content-Type`, and omitting cookies and CSRF tokens (the broker handles Roblox CSRF challenges).
- `ror` proxies `GET /v1/assetId/{id}[/version/{n}]` (with query parameters) and `POST /v1/assets/batch`. The actual asset bytes are downloaded directly from the returned CDN URL without sending the bearer token.
- Forward Roblox's HTTP status and response body as-is. If the broker itself rejects a request, include an `X-Broker-Error: <code>` header and a JSON body `{ "error", "message" }`. `ror` surfaces that message (e.g., `account broker: the shared account is signed out (HTTP 503)`) without caching it as a Roblox refusal. `HTTP 429` responses with `Retry-After` are retried after waiting.

### Roblox Studio's account

With `ROR_STUDIO_COOKIE=1`, `ror` uses the account signed in to Roblox Studio on this machine when no other account credential is set: no broker credentials and no cookie in the environment or env files. It is off by default, and any value other than `1` leaves it off. rur reads the same variable, so one setting covers both tools.

`ror` reads that account's `.ROBLOSECURITY` cookie from where Studio keeps it:
- **Windows:** Windows Credential Manager, in the generic credentials Studio writes under `https://www.roblox.com:RobloxStudioAuth` (the signed-in account's user id, then that account's cookie). Older Studio builds kept it in the registry (`HKCU\Software\Roblox\RobloxStudioBrowser\roblox.com`), which is read when the Credential Manager has none.
- **macOS:** Studio's cookie file, `~/Library/HTTPStorages/com.Roblox.RobloxStudio.binarycookies`: the entries Studio keeps its sign-in in (`/RobloxStudioAuth/userid`, the signed-in account's user id, and `/RobloxStudioAuth/.ROBLOSECURITY<user id>`, its cookie), read as the Credential Manager's are, else a cookie named `.ROBLOSECURITY` itself.
- **Other platforms:** not available.

From there it is handled like any other cookie: sent only to `https://assetdelivery.roblox.com`, never printed, logged, cached or written anywhere. `ror` reports where the account came from (`studio (Windows Credential Manager)`) and the source's status (`off (set ROR_STUDIO_COOKIE=1)`, `not found`, `found (<store>)`, `unreadable: <reason>`), never the value.

Child processes keep `ROR_STUDIO_COOKIE` itself, which holds no credential, so `ror`'s render service and rur read the account themselves; the cookie never leaves the process that read it. A render service that is already running keeps the setting it was started with.

The cookie is your own account, with all its access: turn this on only on a machine you alone use, and prefer an [Open Cloud API key](#open-cloud-api-key) where one is enough.

## Texture caches and streaming

- **KTX2 transcodes:** Image assets and `SurfaceAppearance` texture packs use Roblox's KTX2 transcodes (BC1/BC3 with Roblox-generated mipmaps) to match Studio, falling back to the uploaded image if no transcode exists. Set `ROR_TRANSCODES=0` to force raw uploaded images.
- **Prepared GPU textures:** Decoded GPU-ready textures are cached in `<state dir>/textures/<code>` (4 GB LRU budget by default; override with `ROR_TEXTURE_FILES_MB`) so uploads require a single read and copy per mip level. `<code>` is a hash of the texture-decoding code and locked dependencies (`render/texels.rs`, transcode joining, `ror-image`), so incompatible builds never share bad cache files while unrelated rebuilds keep the cache intact.
- **Mip residency:** Material maps stream only from the finest mip level sampled by the current camera views, leaving visual output unchanged (for example, viewing a large place from a distance reads ~100 MB instead of 1.46 GB). Set `ROR_RESIDENCY=off` to load full mip chains. For static-camera videos, feedback is gathered once for visible parts and remaining parts load in full.

## Disk use and shared storage

Renders, Studio captures, and downloaded assets can accumulate tens of gigabytes. Several mechanisms keep disk usage under control:

### Shared content store

By default, checkouts, builds, and state directories on the same machine share downloads (`assets/`), prepared textures (`textures/<code>/`), GUI images (`guis/<rur build>-<code>/`), refusal records, Vulkan pipeline caches, and GPU selection through a user-level content store (see [building.md](building.md) for default path; set `ROR_STORE=<dir>` to relocate or `ROR_STORE=0` to disable).

- When a state directory needs a file, it hard-links it from the store (validating asset metadata/size or texture/GUI headers) or from another state directory registered in `state-dirs.txt`. New downloads and prepared files are published back to the store via atomic rename.
- Eviction is LRU with a default limit of 4096 MB (`ROR_STORE_MB`), checked at most once per hour.
- Sharing cuts cold startup dramatically (for example, a fresh clone's first 4K map clip drops from 49–89 s to 4.4–4.8 s).
- To link downloads from another tool's cache directory without moving them into the store, set `ROR_SHARED_STATE_DIRS=<dir>;<dir>`.

### Output and capture deduplication

- **Calibration images:** Calibration runs only write `studio.png` and `ror.png` by default. `diff.png` and `side-by-side.png` are generated only when `ROR_DIFF_IMAGES=1` (always enabled for `ror compare`), or on demand with `python tools/analysis/side_by_side.py <experiment or dir>`.
- **Keeping snapshots:** `python tools/keep.py <tag> [filter]` saves `ror.png` and `background.png` as `<tag>.png` / `<tag>-background.png` using hard links, costing zero extra disk space until overwritten.
- **Unchanged writes:** Rewriting a file with identical bytes updates its timestamp without rewriting disk blocks, preserving hard links. Fast PNG outputs use adaptive row filtering (23–37% smaller than the Up filter for ~1 ms extra per image).
- **Cleanup (`ror clean`):** Deduplicates identical files within each category (`out/` PNGs, downloaded assets including shared state dirs, Studio captures, and decoded material packs; never linking captures against `out/`). Flags:
  - `--dry-run`: report savings without changing files.
  - `--older-than <days>`: delete `diff.png` and `side-by-side.png` under `out/` older than `<days>`.
  - `--out <dir>`: target a different output directory.

> [!WARNING]
> Files under `out/` and `<state dir>` may be hard-linked under multiple paths. `ror` always replaces files atomically via rename so other links stay untouched. If you overwrite files manually, remove the target first (e.g., `cp --remove-destination`) rather than writing in place, or you will mutate every hard link.
