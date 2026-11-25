// ror's resource workloads (tools/resources/measure.mjs runs them; AGENTS.md "Resource budgets" says when).
//
// Each workload runs real `ror` commands through the render service, as a caller does: the first command starts the
// service, later ones share it, and the last stops it (`ror service stop`), so the service's own memory, files and
// exit are measured too. All of them render fixtures/shapes.rbxmx offline, on a state folder and a shared store of the
// run's own (ROR_STATE_DIR, ROR_STORE), so "cold" means no prepared textures, GUI images, pipeline cache or GPU choice
// yet, and "warm" means what the cold run left.
//
// On a CI runner there is no GPU: ror draws on the software adapter, and a service's first start compiles every
// pipeline on the CPU (tens of seconds), so the suite starts a service only twice.
//
// Options: --ror <exe> (default: target/release/ror beside this checkout, or ROR_BIN).

import fs from "node:fs";
import path from "node:path";

export const FLAGS = [];

export const SUITES = {
	ci: { about: "What CI runs on every push and pull request: two service sessions on the software adapter (a few minutes on a 4-CPU runner).", workloads: ["render-cold", "render-warm"] },
	full: { about: "The CI set, then a bigger crowd of clients on one warm service.", workloads: ["render-cold", "render-warm", "service-crowd"] },
};

const EXE = process.platform === "win32" ? "ror.exe" : "ror";
const VIEWS = ["iso", "front", "back", "left", "right", "top"];

function rorBinary(context) {
	const given = context.args.ror || process.env.ROR_BIN;
	const binary = path.resolve(given || path.join(context.repo, "target", "release", EXE));
	if (!fs.existsSync(binary)) throw new Error(`tools/resources: no ror at ${binary} (cargo build --release, or pass --ror <exe>)`);
	return binary;
}

function folders(context, id, out) {
	return [
		{ label: "State folder (prepared textures, pipelines, service)", dir: path.join(context.work, "state"), kind: "state" },
		{ label: "Shared store (downloads, prepared files)", dir: path.join(context.work, "store"), kind: "state" },
		{ label: "Output folder", dir: out, kind: "out" },
		{ label: "Temp folder", dir: path.join(context.work, id, "temp"), kind: "temp" },
	];
}

// A still (two 1024x1024 views) and a one-second orbit clip (640x360 at 30 frames a second, an animated PNG ror writes
// itself, so no encoder is needed) through the service
function renderSteps(ror, fixture, out, tag) {
	return [
		{ argv: [ror, "render", fixture, "--views", "iso,front", "--size", "1024x1024", "--offline", "--out", path.join(out, `still-${tag}`)], log: path.join(out, `still-${tag}.log`) },
		{ argv: [ror, "video", fixture, "--orbit", "90", "--duration", "1", "--fps", "30", "--size", "640x360", "--format", "apng", "--offline", "--out", path.join(out, `video-${tag}`)], log: path.join(out, `video-${tag}.log`) },
	];
}

// `count` clients at once, each a still of another view, on the running service
function crowd(ror, fixture, out, count, size) {
	return {
		parallel: Array.from({ length: count }, (_, index) => {
			const view = VIEWS[index % VIEWS.length];
			return { argv: [ror, "render", fixture, "--views", view, "--size", size, "--offline", "--out", path.join(out, `crowd-${index}`)], log: path.join(out, `crowd-${index}.log`) };
		}),
	};
}

function workload({ id, title, about, fresh, requires, steps, expect }) {
	return {
		id, title, about, requires,
		prepare(context) {
			const ror = rorBinary(context);
			const fixture = path.join(context.repo, "fixtures", "shapes.rbxmx");
			const state = path.join(context.work, "state"), store = path.join(context.work, "store");
			if (fresh) for (const dir of [state, store]) fs.rmSync(dir, { recursive: true, force: true });
			for (const dir of [state, store]) fs.mkdirSync(dir, { recursive: true });
			const out = path.join(context.work, id, "out");
			fs.rmSync(path.join(context.work, id), { recursive: true, force: true });
			fs.mkdirSync(out, { recursive: true });
			const plan = path.join(context.work, id, "plan.json");
			fs.writeFileSync(plan, JSON.stringify({ steps: [...steps(ror, fixture, out), { argv: [ror, "service", "stop"], log: path.join(out, "stop.log") }] }));
			// the service exits a moment after `ror service stop`
			return { argv: [context.node, context.fanout, plan], env: { ROR_STATE_DIR: state, ROR_STORE: store }, folders: folders(context, id, out), out, lingerMs: 15000 };
		},
		// every still and clip it asked for is on disk
		verify(prepared) {
			const missing = expect.filter(file => !fs.existsSync(path.join(prepared.out, file)));
			return missing.length ? `missing outputs: ${missing.join(", ")}` : null;
		},
	};
}

export function workloads() {
	return [
		workload({
			id: "render-cold", title: "A still and a clip, cold caches",
			about: "`ror render` (two 1024x1024 views) and `ror video` (a one-second 640x360 orbit) of fixtures/shapes.rbxmx through a render service started on a new state folder, then `ror service stop`.",
			fresh: true,
			steps: (ror, fixture, out) => renderSteps(ror, fixture, out, "cold"),
			expect: ["still-cold/iso.png", "still-cold/front.png", "video-cold/video.png"],
		}),
		workload({
			id: "render-warm", title: "The same with warm caches, then 6 clients at once",
			about: "The same still and clip on the state folder the cold run left (a new service, warm caches on disk), then 6 `ror render` clients at once on that service, then `ror service stop`.",
			fresh: false, requires: ["render-cold"],
			steps: (ror, fixture, out) => [...renderSteps(ror, fixture, out, "warm"), crowd(ror, fixture, out, 6, "768x768")],
			expect: ["still-warm/iso.png", "video-warm/video.png", ...Array.from({ length: 6 }, (_, index) => `crowd-${index}/${VIEWS[index]}.png`)],
		}),
		workload({
			id: "service-crowd", title: "24 clients at once on one service",
			about: "24 `ror render` clients started together on one service over the warm state folder, then `ror service stop`: the service's memory admission and caches under a crowd.",
			fresh: false, requires: ["render-cold"],
			steps: (ror, fixture, out) => [crowd(ror, fixture, out, 24, "1024x1024")],
			expect: Array.from({ length: 24 }, (_, index) => `crowd-${index}/${VIEWS[index % VIEWS.length]}.png`),
		}),
	];
}
