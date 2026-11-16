// Tests of the canary prerelease: its notes (canary.mjs) and the workflow rules that keep it apart from version releases
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { test } from "node:test";
import { ROOT } from "./common.mjs";
import { canary, canaryText, latestReleaseTag } from "./canary.mjs";
import { notes } from "./notes.mjs";

const SHA = "0123456789abcdef0123456789abcdef01234567";
const BUILT = "2026-11-16T01:11:42Z";
// Windows checkouts may have CRLF line ends
const workflow = fs.readFileSync(path.join(ROOT, ".github/workflows/release.yml"), "utf8").replace(/\r\n/g, "\n");

/** The text of one job, from its key to the next job's. */
function job(name) {
	const start = workflow.indexOf(`\n  ${name}:\n`);
	assert.ok(start >= 0, `no job ${name}`);
	const next = workflow.slice(start + 1).search(/\n  [\w-]+:\n/);
	return next < 0 ? workflow.slice(start) : workflow.slice(start, start + 1 + next);
}

/** The text of the step named `name`, up to the next step or the comment above it. */
function step(name) {
	const start = workflow.indexOf(`- name: ${name}\n`);
	assert.ok(start >= 0, `no step ${name}`);
	const next = workflow.slice(start + 1).search(/\n      (?:- |# )|\n  [\w-]+:\n/);
	return next < 0 ? workflow.slice(start) : workflow.slice(start, start + 1 + next);
}

/** A git repository in a temporary folder with a workspace at version 0.1.0, removed after the test. */
function repository(t) {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ror-canary-test-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	fs.mkdirSync(path.join(root, "crates/ror"), { recursive: true });
	fs.writeFileSync(path.join(root, "Cargo.toml"), '[workspace]\nmembers = ["crates/ror"]\n[workspace.package]\nversion = "0.1.0"\n');
	fs.writeFileSync(path.join(root, "crates/ror/Cargo.toml"), '[package]\nname = "ror"\nversion.workspace = true\n');
	const git = (...argv) => execFileSync("git", argv, { cwd: root, encoding: "utf8" }).trim();
	git("init", "--quiet");
	git("config", "user.name", "Release tests");
	git("config", "user.email", "release-test");
	git("add", ".");
	git("commit", "--quiet", "-m", "First change");
	return { root, git };
}

test("canary notes name the commit, the run, the build time and the commits since the latest version release", () => {
	const text = canaryText({ repository: "owner/name", commit: SHA, run: "42", built: BUILT, previous: "v0.3.0", subjects: ["One <b>", "Two"] });
	assert.ok(text.includes(`[\`0123456789\`](https://github.com/owner/name/commit/${SHA})`));
	assert.ok(text.includes("built 2026-11-16 01:11 UTC by [this Release run](https://github.com/owner/name/actions/runs/42)"));
	assert.ok(text.includes("This is a build of main, not a release: it may break at any time. Releases are at https://github.com/owner/name/releases."));
	assert.ok(text.includes("## 2 commits since v0.3.0"));
	assert.ok(text.includes(`[Compare v0.3.0...0123456789](https://github.com/owner/name/compare/v0.3.0...${SHA})`));
	assert.ok(text.includes("- One &lt;b&gt;\n- Two"));
	assert.ok(text.includes(`https://github.com/owner/name/archive/${SHA}.zip`));
	assert.throws(() => canaryText({ repository: "owner/name", commit: SHA, run: "42", built: "", previous: "v0.3.0", subjects: [] }), /--built/);
});

test("canary notes list the newest hundred subjects and count the rest", () => {
	const subjects = Array.from({ length: 103 }, (_, i) => `Change ${i + 1}`);
	const text = canaryText({ repository: "owner/name", commit: SHA, run: "1", built: BUILT, previous: "v0.1.0", subjects });
	assert.ok(text.includes("## 103 commits since v0.1.0"));
	assert.ok(text.includes("- ... and 3 earlier commits\n- Change 4\n"));
	assert.doesNotMatch(text, /- Change 3\n/);
	assert.ok(text.includes("- Change 103\n"));
});

test("the canary tag is never a version release, for the canary's notes or for a release's", (t) => {
	const { root, git } = repository(t);
	const output = path.join(root, "notes.md");

	// before any version release, every commit is listed and nothing is compared
	git("tag", "canary");
	let text = canary({ run: "7", built: BUILT, output }, root);
	assert.ok(text.includes("## 1 commit, no version release yet"));
	assert.doesNotMatch(text, /Compare/);

	git("tag", "v0.0.1");
	git("commit", "--quiet", "--allow-empty", "-m", "After the release");
	git("tag", "--force", "canary");
	git("commit", "--quiet", "--allow-empty", "-m", "After the canary");
	assert.equal(latestReleaseTag("HEAD", root), "v0.0.1");
	text = canary({ run: "7", built: BUILT, output }, root);
	assert.ok(text.includes("## 2 commits since v0.0.1"));
	assert.ok(text.includes("- After the release\n- After the canary\n"));
	assert.doesNotMatch(text, /First change/);

	// a version release's notes reach back past the canary tag to the previous version tag
	git("tag", "v0.1.0");
	const release = notes({ output, tag: "v0.1.0" }, root);
	assert.ok(release.includes("- After the release\n- After the canary"));
	// and the canary at the released commit has no commits since it
	assert.ok(canary({ run: "7", built: BUILT, output }, root).includes("## 0 commits since v0.1.0"));
});

test("the canary notes need the run and a build time", (t) => {
	const { root } = repository(t);
	const output = path.join(root, "notes.md");
	assert.throws(() => canary({ built: BUILT, output }, root), /--run/);
	assert.throws(() => canary({ run: "7", built: "yesterday", output }, root), /--built/);
});

test("workflow builds nightly in a concurrency group of its own and publishes the canary tag", () => {
	assert.match(workflow, /\n  schedule:\n    - cron: "11 1 \* \* \*"\n/);
	assert.match(workflow, /\n    tags: \['v\[0-9\]\+\.\[0-9\]\+\.\[0-9\]\+', 'canary'\]\n/);
	// pushes to main keep their path filter
	assert.match(workflow, /\n    paths:\n      - \.github\/workflows\/release\.yml\n/);
	const group = /\n  group: (.+)\n  cancel-in-progress: false\n/.exec(workflow)[1];
	assert.equal(group, "release-${{ (github.event_name == 'push' && github.ref_type == 'branch' && github.ref) || (github.event_name == 'schedule' && 'nightly') || (github.event_name == 'push' && github.ref == 'refs/tags/canary' && 'canary') || github.run_id }}");
});

test("workflow checks a version tag against the manifests but never the canary", () => {
	const version = step("Check manifest version and tag ancestry");
	assert.match(version, /if \[\[ "\$GITHUB_REF_NAME" == canary \]\]; then\n\s+version=\$\(node tools\/release\/version\.mjs\)\n\s+else\n\s+version=\$\(node tools\/release\/version\.mjs --tag "\$GITHUB_REF_NAME"\)/);
	// both must be on main
	assert.match(version, /git merge-base --is-ancestor "\$GITHUB_SHA" refs\/remotes\/origin\/main/);
	// a canary run never builds: without a passing run it fails (dry-run.test.mjs checks the --required step)
	assert.match(step("Find a passing dry run of this commit"), /&& '--required' \|\| ''/);
	const notesJob = job("notes");
	assert.match(notesJob, /if: \$\{\{ github.ref != 'refs\/tags\/canary' \}\}\n\s+shell: bash\n\s+run: node tools\/release\/notes\.mjs/);
	assert.match(notesJob, /if: \$\{\{ github.ref == 'refs\/tags\/canary' \}\}\n[^]*run: node tools\/release\/canary\.mjs --run "\$RUN_ID" --built "\$BUILT"/);
});

test("workflow publishes the canary as a prerelease under lasting names without moving its tag", () => {
	const publish = job("publish");
	const release = step("Create the release for the existing tag");
	assert.match(release, /if: \$\{\{ github.ref != 'refs\/tags\/canary' \}\}/);
	assert.doesNotMatch(release, /prerelease|latest/);

	const names = step("Give the canary's zips their lasting names");
	assert.match(names, /if: \$\{\{ github.ref == 'refs\/tags\/canary' \}\}/);
	assert.match(names, /mv -- "\$zip" "ror-canary-\$platform"/);
	// the names come before the checksums and the attestation, which name the files
	assert.ok(publish.indexOf("Give the canary's zips") < publish.indexOf("- name: Write SHA256SUMS"));
	assert.ok(publish.indexOf("- name: Write SHA256SUMS") < publish.indexOf("- name: Attest build provenance"));

	const canaryStep = step("Create or update the canary prerelease for the existing tag");
	assert.match(canaryStep, /if: \$\{\{ github.ref == 'refs\/tags\/canary' \}\}/);
	assert.match(canaryStep, /title="Canary \(main \$\{GITHUB_SHA:0:10\}\)"/);
	assert.match(canaryStep, /gh release create "\$TAG" --verify-tag --prerelease --latest=false /);
	assert.match(canaryStep, /gh release edit "\$TAG" --prerelease --latest=false /);
	// no ref is ever created or moved
	assert.doesNotMatch(canaryStep.slice(canaryStep.indexOf("run: |")), /--target|target_commitish|git (?:tag|push)|refs\//);
	// each asset is replaced before the stale ones go
	assert.ok(canaryStep.indexOf("gh release upload \"$TAG\" --clobber") >= 0);
	assert.ok(canaryStep.indexOf("--clobber") < canaryStep.indexOf("gh release delete-asset"));
	// the publish job alone writes
	for (const name of ["version", "build", "smoke", "notes"]) assert.doesNotMatch(job(name), /: write/);
});
