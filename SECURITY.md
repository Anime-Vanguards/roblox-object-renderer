# Security policy

## Reporting a vulnerability

Report security issues privately through GitHub's **Security** tab by clicking **Report a vulnerability** ([direct link](https://github.com/Anime-Vanguards/roblox-object-renderer/security/advisories/new)). Only maintainers can view these reports.

Do not open public issues, pull requests, or discussions for security vulnerabilities.

Please include:
- A description of the issue and potential impact.
- Reproduction steps and the commit hash you tested.

A maintainer will respond within a week and keep you updated through the fix. We will gladly credit you in the advisory unless you prefer to remain anonymous.

## Supported versions

Security fixes apply only to the latest commit on `main`. There are no long-term release branches.

## Scope

**In scope:** Code, build scripts, and CI workflows in this repository. Examples include:
- A crafted model, place, script, or mesh file causing arbitrary code execution, path traversal outside the output directory, or unintended file reads.
- Cross-site or remote access to the local render service or Studio tooling.
- Credential or token leakage.

**Out of scope:** The Roblox platform, Roblox accounts or experiences, and already-public vulnerabilities in third-party dependencies (report those upstream).

## Credentials and local listeners

### Roblox cookie (`.ROBLOSECURITY`)

`ror` can use a `.ROBLOSECURITY` cookie to download authenticated assets. It reads the cookie at runtime only from:
1. `ROBLOX_ASSET_DELIVERY_COOKIE` or `ROBLOSECURITY` environment variables
2. The file path in `ROR_COOKIE_ENV`
3. A local `.env` file in the checkout (git-ignored)

Handling rules:
- Sent **only** to `https://assetdelivery.roblox.com`—never to CDNs, the thumbnails API, or across redirects.
- Held in a memory type that cannot be printed/logged and is zeroed on drop.
- Because a cookie grants full account access, use a dedicated throwaway account if possible.

### Open Cloud API key

`ror` can use an Open Cloud API key (scope `legacy-asset:manage`) instead of, or before, a cookie or broker ([docs/assets.md](docs/assets.md#open-cloud-api-key)). It reads the key at runtime only from the `ROBLOX_ASSET_DELIVERY_API_KEY` environment variable, or a `ROBLOX_ASSET_DELIVERY_API_KEY=` line in the same env files as the cookie.

Handling rules:
- Sent **only** as the `x-api-key` header, over HTTPS to `https://apis.roblox.com/asset-delivery-api/v1/assetId/{id}[/version/{n}]`, after a request without credentials was refused. Never to another host or path, and never across redirects.
- Held in the same memory type as the cookie (cannot be printed or logged, zeroed on drop). Never part of URLs, errors, cache keys, file names or cached files; error messages from Roblox that could echo the key are withheld.
- Removed from the environment of every child process `ror` starts (ffmpeg, Studio, rur, ree), except its own render service, which downloads for it.
- Give the key only the `legacy-asset:manage` scope, and an IP allowlist if you can.

### Account broker

Instead of storing a cookie locally, `ror` supports an account broker that holds the Roblox session and proxies asset requests using personal bearer tokens ([docs/assets.md](docs/assets.md#account-broker)).

`ror` looks for broker credentials in `ROBLOX_BROKER_TOKEN` + `ROBLOX_BROKER_URL`, then in the JSON file at `ROBLOX_BROKER_CREDENTIALS` (defaulting to `~/.config/roblox-broker/credentials.json`). When broker credentials are present, `ror` does not read any cookie. Tokens are sent only to the configured broker over HTTPS (or HTTP to `127.0.0.1` / `localhost`), never forwarded across redirects, and never logged. Because the broker sees every asset ID you request, only configure a broker you trust.

### Local HTTP listeners and Studio bridge

- **Render service:** Binds to `127.0.0.1` only and requires an `X-ROR: 1` header on every request to block browser cross-site requests. Because jobs can reference arbitrary local file paths, treat any local process that can reach the port as able to read your files.
- **Studio MCP bridge:** Configured via `ROR_STUDIO_MCP`. Because the bridge can execute arbitrary Luau inside your open Roblox Studio, bind it to `localhost` only.

### Accidental leaks

Never paste cookies, tokens, or keys into issues, PRs, or logs. If you accidentally expose a `.ROBLOSECURITY` cookie, immediately sign out of all sessions in your Roblox account settings to invalidate it, then contact us so we can scrub the post. An exposed Open Cloud API key is revoked or regenerated on the Creator Dashboard's API Keys page.
