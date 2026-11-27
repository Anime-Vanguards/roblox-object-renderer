// Rewrites test-weights.json, the seconds each of ror's library tests takes in CI, from the logs of the library jobs
// (.github/workflows/ci.yml, "Test ror's library, part <n> of 6"):
//
//   node tools/ci/refresh-weights.mjs --repo <owner>/<name> <run id>...   the runs' library jobs' logs, through gh
//   node tools/ci/refresh-weights.mjs --log <file>...                     job logs saved before (gh api .../logs)
//
// A job runs its tests one at a time, and its log has a timestamp on every line: a test takes from the line before it
// that ended a test (or began a run of them) to its own. With several runs, each test takes its median. A test the logs
// do not show (a job cancelled before it) keeps the time it had; --prune drops those instead (a test that is gone).
// --out <file> writes elsewhere.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { fileURLToPath } from "node:url";

const WEIGHTS = fileURLToPath(new URL("./test-weights.json", import.meta.url));
const LIBRARY_JOB = /^Test ror's library, part \d+ of \d+/;
const STAMP = /(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) (.*)$/;

// The seconds each test of one job's log took, by name
export function testSeconds(log) {
	const seconds = new Map();
	let last = null;
	for (const line of log.split(/\r?\n/)) {
		const stamped = STAMP.exec(line);
		if (!stamped) continue;
		const [, stamp, text] = stamped;
		const at = Date.parse(stamp);
		if (/^running \d+ tests?$/.test(text)) {
			last = at;
			continue;
		}
		const done = /^test (\S+) \.\.\. (?:ok|FAILED|ignored\b.*)$/.exec(text);
		if (done && last !== null) {
			seconds.set(done[1], (at - last) / 1000);
			last = at;
		} else if (/^test result: /.test(text)) {
			last = null;
		}
	}
	return seconds;
}

function median(values) {
	const sorted = [...values].sort((a, b) => a - b);
	const middle = Math.floor(sorted.length / 2);
	return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
}

// The table `old` becomes with the logs' times: `runs` is one map per run of every library job's tests
export function refresh(old, runs, { prune = false } = {}) {
	const seen = new Map();
	for (const run of runs) {
		for (const [name, value] of run) seen.set(name, [...(seen.get(name) ?? []), value]);
	}
	const seconds = {};
	const names = new Set([...Object.keys(old), ...seen.keys()]);
	for (const name of [...names].sort()) {
		if (seen.has(name)) seconds[name] = Math.round(median(seen.get(name)) * 10) / 10;
		else if (!prune) seconds[name] = old[name];
	}
	return seconds;
}

function gh(args) {
	const result = spawnSync("gh", args, { encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true });
	if (result.error) throw result.error;
	if (result.status !== 0) throw new Error(`gh ${args.join(" ")}: ${result.stderr.trim()}`);
	return result.stdout;
}

// One map of every library job's test times per run, read through gh
function fetchRuns(repo, ids) {
	return ids.map((id) => {
		const jobs = gh(["api", `repos/${repo}/actions/runs/${id}/jobs`, "--paginate", "--jq", ".jobs[] | [.id, .name] | @tsv"])
			.split("\n")
			.filter(Boolean)
			.map((row) => row.split("\t"))
			.filter(([, name]) => LIBRARY_JOB.test(name));
		if (jobs.length === 0) throw new Error(`run ${id} has no library jobs`);
		const times = new Map();
		for (const [job] of jobs) for (const [name, value] of testSeconds(gh(["api", `repos/${repo}/actions/jobs/${job}/logs`]))) times.set(name, value);
		return times;
	});
}

function main(argv) {
	let repo = null;
	let out = WEIGHTS;
	let prune = false;
	const logs = [];
	const ids = [];
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--repo") repo = argv[++i];
		else if (arg === "--out") out = argv[++i];
		else if (arg === "--prune") prune = true;
		else if (arg === "--log") logs.push(argv[++i]);
		else if (/^\d+$/.test(arg)) ids.push(arg);
		else throw new Error(`unknown argument ${arg}`);
	}
	if (ids.length && !repo) throw new Error("run ids need --repo <owner>/<name>");
	if (!ids.length && !logs.length) throw new Error("usage: refresh-weights.mjs --repo <owner>/<name> <run id>... | --log <file>...");
	// saved logs count as one run together
	const runs = ids.length ? fetchRuns(repo, ids) : [];
	if (logs.length) runs.push(new Map(logs.flatMap((file) => [...testSeconds(fs.readFileSync(file, "utf8"))])));
	const table = JSON.parse(fs.readFileSync(WEIGHTS, "utf8"));
	const seconds = refresh(table.seconds ?? {}, runs, { prune });
	const from = ids.length ? `public CI run${ids.length > 1 ? "s" : ""} ${ids.join(", ")}` : `saved job logs`;
	table.measured = `${from}, ${new Date().toISOString().slice(0, 10)}`;
	table.seconds = seconds;
	fs.writeFileSync(out, `${JSON.stringify(table, null, "\t")}\n`);
	console.error(`${Object.keys(seconds).length} tests, ${Math.round(Object.values(seconds).reduce((sum, value) => sum + value, 0))} s together, from ${from}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === fs.realpathSync(process.argv[1])) {
	try {
		main(process.argv.slice(2));
	} catch (error) {
		console.error(error.message);
		process.exitCode = 1;
	}
}
