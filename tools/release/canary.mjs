// Writes the notes of the canary prerelease: which commit of main the zips are a build of, when the passing run built
// them, and the commits since the latest version release. Needs the full git history and the release tags.
//
//   node tools/release/canary.mjs --run <id> --built <run start, ISO 8601> --output release-notes.md [--repository <owner/name>]
import { REPOSITORY, ROOT, commitSubjects, git, main, write } from "./common.mjs";

// Only version tags are releases; the canary tag and any other tag are not.
const RELEASE_TAG = /^v\d+\.\d+\.\d+$/;
// The most commit subjects listed; the compare link has the rest.
const MOST_SUBJECTS = 100;

/** The newest version tag that `commit` contains (the tag of `commit` itself included), or undefined before the first. */
export function latestReleaseTag(commit, root = ROOT) {
	return git(["tag", "--merged", commit, "--sort=-version:refname"], root)
		.split("\n")
		.find((name) => RELEASE_TAG.test(name));
}

/** "2026-11-16 01:11 UTC" for an ISO 8601 time. */
function day(iso) {
	const time = new Date(iso);
	if (!/^\d{4}-\d\d-\d\dT/.test(iso ?? "") || Number.isNaN(time.getTime())) throw new Error(`--built ${iso} is not an ISO 8601 time`);
	return `${time.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

/** The notes as Markdown. `subjects` are the commits since `previous`, oldest first. */
export function canaryText({ repository, commit, run, built, previous, subjects }) {
	const repo = `https://github.com/${repository}`;
	const short = commit.slice(0, 10);
	// A subject is only ever text in a file, never a shell command or a workflow expression.
	const escape = (subject) => subject.replace(/[<>]/g, (char) => (char === "<" ? "&lt;" : "&gt;"));
	const listed = subjects.slice(-MOST_SUBJECTS).map((subject) => `- ${escape(subject)}`);
	if (subjects.length > MOST_SUBJECTS) listed.unshift(`- ... and ${subjects.length - MOST_SUBJECTS} earlier commits`);
	const count = `${subjects.length} commit${subjects.length === 1 ? "" : "s"}`;
	const sections = [
		`A build of main at [\`${short}\`](${repo}/commit/${commit}), built ${day(built)} by [this Release run](${repo}/actions/runs/${run}), which smoke-tested it on every platform.`,
		`This is a build of main, not a release: it may break at any time. Releases are at ${repo}/releases.`,
		previous ? `## ${count} since ${previous}` : `## ${count}, no version release yet`,
		previous ? `[Compare ${previous}...${short}](${repo}/compare/${previous}...${commit})` : "",
		listed.join("\n"),
		"## Verifying and source",
		`Check a zip against SHA256SUMS, then run \`gh attestation verify <zip> -R ${repository}\`. The source of this build is ${repo}/tree/${commit} (archive: ${repo}/archive/${commit}.zip).`,
	];
	return sections.filter(Boolean).join("\n\n") + "\n";
}

export function canary(options, root = ROOT) {
	if (!/^\d+$/.test(options.run ?? "")) throw new Error("--run <the id of the run that built the zips> is required");
	if (!options.output) throw new Error("--output <file> is required");
	const repository = options.repository ?? REPOSITORY;
	if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error(`repository ${repository} is not owner/name`);
	const commit = git(["rev-parse", "HEAD"], root);
	const previous = latestReleaseTag(commit, root);
	const subjects = commitSubjects(previous ? `${previous}..${commit}` : commit, root);
	const text = canaryText({ repository, commit, run: options.run, built: options.built, previous, subjects });
	write(options.output, text);
	return text;
}

main(import.meta.url, (options) => canary(options));
