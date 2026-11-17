# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub: on this repository's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/Anime-Vanguards/roblox-object-renderer/security/advisories/new)). Only the maintainers can see the report.

Please don't open a public issue, PR or discussion about it.

Include what you found, how to reproduce it, which commit you tested, and what an attacker could do with it. A maintainer will reply within a week and keep you updated until it's fixed. We're glad to credit you in the advisory unless you'd rather stay anonymous.

## Supported versions

Only the latest commit on `main` gets security fixes. There are no release branches yet.

## Scope

In scope: the code in this repository, its build scripts and its CI workflows. Examples:

- a crafted model, place, script or mesh file that makes the tool run code, write outside its output folder, or read files it shouldn't;
- a way for the local preview or render service to be reached from other machines or websites;
- anything that could leak a credential the tool is given.

Out of scope: the Roblox platform, Roblox accounts, any Roblox game or experience, and issues in third-party dependencies that are already public (report those upstream).

## Credentials

ror can use a Roblox account cookie (`.ROBLOSECURITY`) to download assets that need authentication. It reads the cookie at run time only from the `ROBLOX_ASSET_DELIVERY_COOKIE` or `ROBLOSECURITY` environment variables, a file named by `ROR_COOKIE_ENV`, or this checkout's `.env` (ignored by Git); it sends it only to `https://assetdelivery.roblox.com` (never to the CDN, the thumbnails API or across a redirect), keeps it in memory in a type that never prints and is wiped when dropped, and never logs it. The cookie is a full login for that account: use a separate account with no value on it if you can.

Instead of the cookie, ror can use an account broker: a service you run that holds the Roblox session and forwards asset requests for personal tokens, so the cookie is on no machine that runs ror ([docs/assets.md](docs/assets.md#account-broker)). Its credentials are `ROBLOX_BROKER_TOKEN` with `ROBLOX_BROKER_URL` in the environment, else the JSON file named by `ROBLOX_BROKER_CREDENTIALS`, else `~/.config/roblox-broker/credentials.json`. When they are found, no cookie is read. The token is sent only to that broker, in the requests the cookie would have been in (never to Roblox, the CDN or across a redirect), only over https or to this machine (`127.0.0.1`, `localhost`), and it is held like the cookie and never logged. A broker sees which assets you ask for: only name one you trust.

ror also runs local listeners: the render service speaks HTTP on 127.0.0.1 only, and every request needs the header `X-ROR: 1`, which keeps browsers' cross-site requests out; it reads any file path a job names, so treat anything that can reach that port as able to read your files. The Studio tooling talks to a Studio MCP bridge at the address `ROR_STUDIO_MCP` gives, which can run Luau in your open Studio: run the bridge on localhost only.

Never paste a cookie, token or key into an issue, PR or log. If you posted one by accident, treat it as stolen: for a Roblox cookie, sign out of all sessions in your Roblox account settings, which invalidates it, then tell us so we can delete the post.
