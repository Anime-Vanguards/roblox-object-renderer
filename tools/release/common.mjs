// Helpers shared by the release scripts.
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// The public repository as owner/name. It is derived from the URL because the publishing gate allows the
// repository's name only in a URL.
export const REPOSITORY = new URL("https://github.com/Anime-Vanguards/roblox-object-renderer").pathname.slice(1);

/** Reads `--name value` pairs into an object: `--tag v0.1.0` becomes `{ tag: "v0.1.0" }`. */
export function args(argv = process.argv.slice(2)) {
	const options = {};
	for (let i = 0; i < argv.length; i += 2) {
		const flag = argv[i];
		const value = argv[i + 1];
		if (!flag.startsWith("--") || value === undefined) {
			throw new Error("expected arguments as --name value pairs");
		}
		options[flag.slice(2)] = value;
	}
	return options;
}

/**
 * Whether two paths are the same file once links are followed. The real paths are compared because one script can
 * be reached by two names: on macOS the temp folder /var is a link to /private/var. A path that cannot be resolved
 * is compared as written.
 */
function sameFile(first, second) {
	try {
		return fs.realpathSync(first) === fs.realpathSync(second);
	} catch {
		return first === second;
	}
}

/** Runs `action` with the command line options when the script was started directly, not imported (by the tests). */
export async function main(url, action) {
	const entryPoint = process.argv[1];
	if (!entryPoint || !sameFile(path.resolve(entryPoint), fileURLToPath(url))) return;
	try {
		await action(args());
	} catch (error) {
		console.error(error.message);
		process.exitCode = 1;
	}
}

/** The output of a git command run in `cwd`, without its trailing newline. */
export function git(argv, cwd = ROOT) {
	return execFileSync("git", argv, { cwd, encoding: "utf8", maxBuffer: 32 * 1024 * 1024 }).trimEnd();
}

// Subjects the notes print instead of a public commit's own, by the commit's full SHA: a subject that went out wrong
// stays in the public history, which is never rewritten. The file is JSON, read as data and never run.
export const CORRECTIONS = "tools/release/notes-corrections.json";

/** The corrections of the checkout at `root`, a Map from full commit SHA to subject; empty when it has no file. */
export function corrections(root = ROOT) {
	let text;
	try {
		text = fs.readFileSync(path.join(root, CORRECTIONS), "utf8");
	} catch (error) {
		if (error.code === "ENOENT") return new Map();
		throw error;
	}
	let data;
	try {
		// a byte order mark, which some Windows editors write, is not part of the JSON
		data = JSON.parse(text.charCodeAt(0) === 0xfeff ? text.slice(1) : text);
	} catch (error) {
		throw new Error(`${CORRECTIONS} is not valid JSON: ${error.message}`);
	}
	if (data === null || typeof data !== "object" || Array.isArray(data)) {
		throw new Error(`${CORRECTIONS} is not an object of full commit SHAs to subjects`);
	}
	const subjects = new Map();
	for (const [sha, subject] of Object.entries(data)) {
		if (!/^[a-f0-9]{40}$/.test(sha)) throw new Error(`${CORRECTIONS}: ${sha} is not a full commit SHA`);
		if (typeof subject !== "string" || !subject.trim() || /[\u0000-\u001f\u007f]/.test(subject)) {
			throw new Error(`${CORRECTIONS}: the subject for ${sha} is not one line of text`);
		}
		subjects.set(sha, subject);
	}
	return subjects;
}

/** The subjects of the commits in `range`, oldest first, each replaced by its correction if it has one. */
export function commitSubjects(range, root = ROOT) {
	const corrected = corrections(root);
	return git(["log", "--reverse", "--format=%H%x09%s", range], root)
		.split("\n")
		.filter(Boolean)
		.map((line) => {
			const tab = line.indexOf("\t");
			return corrected.get(line.slice(0, tab)) ?? line.slice(tab + 1);
		})
		.filter(Boolean);
}

/** Writes a file, making its folder first. */
export function write(file, text) {
	fs.mkdirSync(path.dirname(file), { recursive: true });
	fs.writeFileSync(file, text);
}

/** Checks which release a build belongs to and returns it as `{ version, tag, commit, repository }`. */
export function identity(version, tag, commit, repository = REPOSITORY) {
	if (!/^\d+\.\d+\.\d+$/.test(version) || tag !== `v${version}`) {
		throw new Error(`tag ${tag} is not v<manifest version> (manifests say ${version})`);
	}
	if (!/^[a-f0-9]{40}$/.test(commit)) {
		throw new Error(`commit ${commit} is not a full SHA`);
	}
	if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) {
		throw new Error(`repository ${repository} is not owner/name`);
	}
	return { version, tag, commit, repository };
}

/** SOURCE.md: where the Corresponding Source of a binary is. It links the exact commit: a canary's is not its version tag's. */
export function sourceText({ tag, commit, repository }) {
	const repo = `https://github.com/${repository}`;
	const paragraphs = [
		"# Corresponding Source",
		"This binary is distributed under GPL-3.0-or-later.",
		`Source: ${repo}/tree/${commit}\nCommit: ${commit} (version ${tag})\nSource archive: ${repo}/archive/${commit}.tar.gz`,
		"The source includes Cargo.lock and the release scripts. Obtain the Rust crates pinned in Cargo.lock from crates.io; the lockfile records their SHA-256 checksums. Any git dependencies are available from their public repository URLs at the commits pinned in Cargo.lock.",
		"Build with Rust 1.90.0: cargo build --locked --profile dist --bin ror\nUse the public default features and the native target triple (--target <triple>). On Windows set RUSTFLAGS=\"-C target-feature=+crt-static\" to include the C runtime. No x264 library or other downloaded build archive is required.",
		"See .github/workflows/release.yml at this commit for the target triples, runner setup, licence inventory and packaging commands.",
	];
	return paragraphs.join("\n\n") + "\n";
}
