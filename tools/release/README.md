# Release scripts

Node scripts with no npm dependencies, called by `.github/workflows/release.yml`. Run them from the
repository root:

```sh
node tools/release/version.mjs
node tools/release/version.mjs --tag v0.1.0
node tools/release/licenses.mjs --target x86_64-pc-windows-msvc --output release/THIRD_PARTY_LICENSES.txt
node tools/release/package.mjs --binary target/dist/ror.exe --version 0.1.0 --os windows --arch x86_64 --licenses release/THIRD_PARTY_LICENSES.txt --out release
node tools/release/notes.mjs --tag v0.1.0 --output release-notes.md
node tools/release/canary.mjs --run 123 --built 2026-11-16T01:11:00Z --output release-notes.md
node tools/release/dry-run.mjs --repository <owner/name> --sha <commit> --artifacts <name,...> [--required]
node --test tools/release/*.test.mjs
```

| script | what it does |
|---|---|
| `version.mjs` | prints the version in the manifests; fails if they disagree, or with `--tag` if the tag is not `v<version>` |
| `licenses.mjs` | writes `THIRD_PARTY_LICENSES.txt` for one build target |
| `package.mjs` | writes `SOURCE.md` and the release zip |
| `notes.mjs` | writes the release notes |
| `canary.mjs` | writes the notes of the canary prerelease |
| `dry-run.mjs` | finds the passing run whose zips a tag publishes |
| `smoke.mjs` | runs a built binary the way a user gets it |
| `release.test.mjs` | tests of the scripts and of the workflow's safety rules |
| `canary.test.mjs`, `dry-run.test.mjs` | tests of the canary and of the choice of a passing run |

## licenses.mjs

It runs `cargo metadata --locked --format-version 1 --filter-platform <target>` with the public
default features and walks the normal dependencies of `ror` that are active, so the list is the crates
compiled into the binary. An optional dependency counts only when a feature turns it on: a weak
reference such as `name?/feature` does not. For each crate it keeps the licence and notice files of
the package root, the files of a `LICENSES` folder and the notices in vendored sources. The crates of
this repo also get the repository's licence and notices. A crate that ships no licence file gets its
declared licence, and the standard text of that licence comes from another local package, in an
appendix. If no package has the text the script fails. It never downloads anything.

To check it against this repo, save that same metadata as JSON, set `ROR_RELEASE_METADATA` to the
file and run the tests. `--metadata <json>` makes the script read such a file instead of running
Cargo, to regenerate an inventory from the exact metadata of a release. The metadata must describe
that release's target and features. The workflow calls the script without it, so it always uses the
public defaults.

## package.mjs

It writes the zip with Node's zlib: the entries in a fixed order, a fixed date and Unix modes on
every host, so the same input gives the same zip. The binary is the first entry, stored with mode
0755, and no other entry is executable. The script reads the zip back and checks this before it
writes anything. `SOURCE.md` is written inside the zip and beside it.

`--tag`, `--commit` and `--repository` are optional. They default to `v<version>`, HEAD and this
public repository.

## notes.mjs

It needs the full git history and the release tags. The notes list the commit subjects since the
previous release tag (all of them for the first release), then how to install and verify, and where
the source is.

## dry-run.mjs

It lists the runs of the release workflow at one commit of main and takes the newest that a push, a
manual start or the nightly schedule started, that passed every job but the tag-only ones and still
has every zip. A version tag publishes that run's zips instead of building them again, and builds them
itself when there is none. A nightly run that finds one has nothing to do. The canary tag passes
`--required`: it only ever publishes such a run's zips, so without one, or when the API cannot be
read, it fails. It prints `run-id=` and `run-date=` (when that run started) for `$GITHUB_OUTPUT`.

## canary.mjs

The canary tag, moved by the export to the newest commit of main with a passing run, publishes that
run's zips as a prerelease named canary. Its notes give the commit, when the run built the zips, a link
to the run, and the commits since the newest version tag the commit contains, with a compare link; the
canary tag itself never counts as a release. The workflow names the zips `ror-canary-<os>-<arch>.zip`,
writes `SHA256SUMS` and replaces the assets under those names, so links to them never change, then
deletes the assets the build no longer has. The prerelease is never marked latest, and nothing that
reads release tags (`version.mjs --tag`, `notes.mjs`) takes it for one.

## smoke.mjs

```sh
node tools/release/smoke.mjs --binary <extracted ror> --fixture <shapes.rbxmx> --version <version> [--workspace <folder>]
```

It copies the executable and fixture into a temporary folder, then runs `--version`, a direct render
and a service render, with the home, cache and temporary folders redirected there too. It waits for
the service processes to exit and checks the PNG header and size.

It also checks paths and file metadata for unexpected writes in three places: that temporary folder,
the workspace and the folder the binary was extracted to. `--workspace` names the workspace; it
defaults to `GITHUB_WORKSPACE`, else this checkout. In the redirected home, only ror's cache and the
named Mesa, NVIDIA, AMD and Direct3D cache folders are allowed. Driver cache files are never opened
for hashing, and the real user folders are never scanned.

It needs a GPU or a software adapter; the workflow installs Mesa on Linux.

## Tests

`node --test tools/release/*.test.mjs` checks every script: versions that disagree and a wrong tag,
the zip's first entry, checksums, modes and full layout, the normal-dependency walk and crates
without licence files, notes bounded by the previous tag with shell syntax in a subject, the canary's
notes and the choice of a passing run, and the workflow's rules (pinned actions, no cache, a publish job
without repository code, no `${{ }}` in `run:` scripts, the nightly schedule and its concurrency, a
canary that never builds, is never latest and never moves its tag). The workflow runs them before building.
