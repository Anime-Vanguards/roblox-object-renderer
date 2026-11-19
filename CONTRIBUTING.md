# Contributing to ror

Thanks for contributing. This guide explains how changes are accepted, how to set up a local build, and what we cannot merge.

## How pull requests are merged

This public repository is published from an internal repository via an automated sync tool, so `main` only advances through sync commits.

You still open pull requests here and go through normal review. Once approved, a maintainer imports your diff into the internal repo. The next sync pushes it back to public `main` with you preserved as the commit author, and closes your PR with a link to that commit. A closed PR with a `"Landed in ..."` comment means it was merged.

## Before you open a PR

- **Small fixes** (typos, clear bugs with a test): open a PR directly.
- **Larger changes** (new features, CLI flags, behavior changes, refactors): open an issue first to align on the design. Unsolicited large PRs may be closed to avoid wasted effort.
- **Visual differences from Studio:** use the "Looks different from Studio" issue template and attach side-by-side Studio output. See [Measuring against Studio](#measuring-against-studio).
- **Security vulnerabilities:** never report publicly. Follow [SECURITY.md](SECURITY.md).

Search open issues before filing a new one.

## Development setup

1. Install **Rust 1.90+** (on Windows, use the MSVC toolchain; the bundled `rust-lld` handles linking) and clone this repository.
2. Build and test:
   ```sh
   cargo build --release --workspace --all-targets
   cargo test --release --workspace -- --test-threads=1
   cargo clippy --release --workspace --all-targets
   ```
   (Run `cargo test --release -p ror -- --test-threads=1` to test just the `ror` crate.)
3. **What different test suites need:**
   - **No extra dependencies:** `ror-image`, synthetic `ror-mesh` tests, `ror-assets` unit/mock-server tests, and most `ror` scene, playback, lint, budget, and cost tests.
   - **GPU or software adapter:** `ror` render, video, service, and job tests (falls back to WARP on Windows if no hardware GPU is available).
   - **Installed Roblox Player or Studio:** one `ror-assets` test (auto-skips if absent) and renders relying on built-in skies, avatar meshes, or fonts.
   - **Local Roblox client cache:** `ror-mesh`'s `local_cache` test (auto-skips if absent).
   - **Running Studio + MCP bridge:** not required by `cargo test`; only needed for parity work ([docs/parity.md](docs/parity.md)).
4. **Optional sibling tools:**
   - [rur](https://github.com/Anime-Vanguards/roblox-ui-renderer) for GUI rendering and [ree](https://github.com/Anime-Vanguards/roblox-engine-emulator) for `.luau` inputs, checked out alongside this repo.
   - `ffmpeg` on `PATH` for WebM, GIF, and default MP4 encoding, or the optional `x264` feature ([docs/building.md](docs/building.md)).
5. **Analysis scripts in `tools/`:** require Python 3.10+ with `numpy`, `scipy`, `Pillow`, and `opencv-python` (`tools/perf_suite.py` also needs `psutil` and `pywin32`).

## Pull request guidelines

- **One focused change per PR.** Split unrelated fixes into separate PRs.
- **Include tests for behavior changes.** Run the full build, test, and clippy commands above before pushing, and note what you ran in the PR description.
- **Match existing code style.** This repo uses hard tabs and long lines with no `rustfmt` config—do **not** run `cargo fmt`, as it will reformat the entire tree. Existing Clippy warnings are tolerated, but do not introduce new ones. Write doc comments explaining what code does, the Studio rule it implements, and the calibration experiment that measured it if applicable.
- **Update documentation.** Keep `README.md` and `docs/` in sync when flags, outputs, or behavior change.
- **Clear titles and commit messages.** Use one plain sentence describing the change (e.g., `"Offset particle depth sorting by ZOffset"`).
- **Keep the tree clean.** Do not commit build artifacts, renders, logs, or generated files.

### Measuring against Studio

Because `ror` aims to match Roblox Studio directly, parity PRs must include:

- The Roblox Studio version and graphics settings used for comparison.
- A minimal, self-contained scene or script built from scratch (primitive parts, a small `.rbxm`, or a short script) that reproduces the difference.
- Before-and-after metrics or images compared side by side against Studio.

Never copy code, shaders, lookup tables, or assets from Roblox binaries or installation directories. Fit constants from black-box measurements instead.

## What we cannot accept

- Models, places, images, meshes, audio, fonts, or text you do not have the right to distribute—including assets from Roblox games (ours included) or Roblox's built-in content. Tests and fixtures must construct their own scenes.
- Code copied, decompiled, or reverse-engineered from Roblox binaries, or code under licenses incompatible with GPL-2.0-or-later.
- Telemetry or outbound network requests to hosts other than the Roblox endpoints `ror` already uses.
- Secrets or credentials anywhere in code, tests, logs, or screenshots (never paste a `.ROBLOSECURITY` cookie).

## AI-assisted contributions

AI coding tools are welcome—the maintainers use them heavily as well—under these rules:

- **You own every line.** Read, understand, and verify your changes before opening a PR. Don't submit code you cannot explain in review.
- **Disclose AI use.** Fill out the PR template section noting which tools you used and how.
- **No drive-by bulk PRs.** Unattended agent PRs touching many files or opened in batches without a backing issue will be closed without review.
- **Point agents at [AGENTS.md](AGENTS.md).**
- **Tests are mandatory.** Generated fixes without a test that fails before the fix will not be merged.

## Review process

Issues and pull requests receive an initial automated triage and review (usually within an hour). Anything requiring judgment is escalated to a maintainer. If you file a bug or feature request as an issue, you don't need to submit a PR—maintainers handle fixes directly.

PRs undergo security screening first, then code review. Push requested changes to the same PR branch to trigger re-review. Edits to build scripts, CI workflows, dependencies, or agent instructions always require direct maintainer sign-off.

## License and conduct

`ror` is licensed under GPL-2.0-or-later ([LICENSE](LICENSE)). By submitting a contribution, you confirm you have the right to contribute it under those terms.

All participants must follow the [Code of Conduct](CODE_OF_CONDUCT.md).
