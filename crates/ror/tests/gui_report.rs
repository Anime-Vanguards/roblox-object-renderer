//! End-to-end check that `rur` is discovered through `RUR_HOME` and that `summary.json` records
//! every drawn and skipped GUI (with its skip reason) for both `ror render` and `ror video`.

use std::path::{Path, PathBuf};
use std::process::Command;

use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Enum, UDim, UDim2, Vector2};
use ror::scene::build::{ModelBuilder, PartSpec};

fn executable(stem: &str) -> String {
	if cfg!(windows) { format!("{stem}.exe") } else { stem.to_string() }
}

/// Writes a self-contained mock `cli/rur.mjs` that parses `--viewport`, `--scale`, and `--out`
/// and writes a solid-opaque PNG of the exact requested dimensions plus `scene.json` with
/// `"background":null`, matching `render::rur::render_through_node`.
fn write_mock_rur(root: &Path) -> PathBuf {
	let cli = root.join("cli");
	std::fs::create_dir_all(&cli).unwrap();
	let script = r#"import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";

const args = process.argv.slice(2);
const flag = (name) => {
	const index = args.indexOf(name);
	return index >= 0 ? args[index + 1] : null;
};
const [lw, lh] = (flag("--viewport") || "16x16").split("x").map(Number);
const scale = Number(flag("--scale") || "1");
const out = flag("--out");
const width = Math.max(1, Math.round(lw * scale));
const height = Math.max(1, Math.round(lh * scale));
fs.mkdirSync(out, { recursive: true });

function crc32(buf) {
	let crc = 0xffffffff;
	for (const byte of buf) {
		crc ^= byte;
		for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
	}
	return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
	const len = Buffer.alloc(4);
	len.writeUInt32BE(data.length, 0);
	const body = Buffer.concat([Buffer.from(type), data]);
	const crc = Buffer.alloc(4);
	crc.writeUInt32BE(crc32(body), 0);
	return Buffer.concat([len, body, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(width, 0);
ihdr.writeUInt32BE(height, 4);
ihdr[8] = 8;
ihdr[9] = 6;
const row = Buffer.alloc(1 + width * 4);
row[0] = 0;
for (let x = 0; x < width; x++) {
	row[1 + x * 4] = 40;
	row[1 + x * 4 + 1] = 200;
	row[1 + x * 4 + 2] = 60;
	row[1 + x * 4 + 3] = 255;
}
const raw = Buffer.concat(Array.from({ length: height }, () => row));
const png = Buffer.concat([
	Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
	chunk("IHDR", ihdr),
	chunk("IDAT", zlib.deflateSync(raw)),
	chunk("IEND", Buffer.alloc(0)),
]);
fs.writeFileSync(path.join(out, "render.png"), png);
fs.writeFileSync(path.join(out, "scene.json"), JSON.stringify({ background: null }));
console.log(JSON.stringify({ status: "ok", missingImages: [], missingFonts: [], renderErrors: [], scriptErrors: 0, seconds: 0.01, phases: {} }));
"#;
	std::fs::write(cli.join("rur.mjs"), script).unwrap();
	root.to_path_buf()
}

fn write_gui_model(path: &Path) {
	let mut model = ModelBuilder::new("GuiScene");
	let part = model.part(&PartSpec { name: "Board".into(), cframe: Affine3A::IDENTITY, size: Vec3::new(4.0, 4.0, 1.0), ..Default::default() });
	// Front face (-Z, NormalId 5): faces a camera at z = -10 looking at origin
	let visible = model.child(part, InstanceBuilder::new("SurfaceGui").with_name("Visible").with_property("Face", Enum::from_u32(5)).with_property("CanvasSize", Vector2::new(64.0, 64.0)));
	model.child(visible, InstanceBuilder::new("Frame").with_property("Size", UDim2::new(UDim::new(1.0, 0), UDim::new(1.0, 0))));
	// Back face (+Z, NormalId 2): faces away from camera at z = -10
	model.child(part, InstanceBuilder::new("SurfaceGui").with_name("BackSide").with_property("Face", Enum::from_u32(2)).with_property("CanvasSize", Vector2::new(64.0, 64.0)));
	// Beyond MaxDistance (1 stud, camera is 10 studs away)
	model.child(part, InstanceBuilder::new("SurfaceGui").with_name("TooFar").with_property("Face", Enum::from_u32(5)).with_property("MaxDistance", 1.0f32).with_property("CanvasSize", Vector2::new(64.0, 64.0)));
	// Disabled GUI
	model.child(part, InstanceBuilder::new("SurfaceGui").with_name("Disabled").with_property("Enabled", false).with_property("Face", Enum::from_u32(5)));
	// Stray ScreenGui outside PlayerGui
	model.child(model.model, InstanceBuilder::new("ScreenGui").with_name("Stray"));

	let document = model.into_document();
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &document.roots).unwrap();
	std::fs::write(path, bytes).unwrap();
}

#[test]
fn summary_records_drawn_and_skipped_guis_and_finds_rur_via_rur_home() {
	let root = std::env::temp_dir().join(format!("ror-gui-report-test-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	let bin = root.join("bin");
	let state = root.join("state");
	std::fs::create_dir_all(&bin).unwrap();
	std::fs::create_dir_all(&state).unwrap();

	// Copy ror outside any checkout so ../roblox-ui-renderer cannot be picked up implicitly.
	let binary = bin.join(executable("ror"));
	std::fs::copy(env!("CARGO_BIN_EXE_ror"), &binary).unwrap();
	let model = root.join("guis.rbxm");
	write_gui_model(&model);
	let mock_rur = write_mock_rur(&root.join("mock-rur"));

	let base_cmd = |rur_home: Option<&Path>, no_guis: bool| {
		let mut cmd = Command::new(&binary);
		cmd.env("ROR_STATE_DIR", &state).env("ROR_STORE", "0").env("ROR_RUR_DIRECT", "0");
		cmd.env_remove("ROR_RUR").env_remove("RUR_HOME").env_remove("ROR_NO_GUIS");
		if let Some(paths) = std::env::var_os("PATH") {
			let filtered: Vec<PathBuf> = std::env::split_paths(&paths)
				.filter(|dir| !["rur", "rur.mjs", "rur.cmd", "rur.bat", "rur.exe"].iter().any(|name| dir.join(name).is_file()))
				.collect();
			if let Ok(joined) = std::env::join_paths(filtered) {
				cmd.env("PATH", joined);
			}
		}
		if let Some(dir) = rur_home {
			cmd.env("RUR_HOME", dir);
		}
		if no_guis {
			cmd.env("ROR_NO_GUIS", "1");
		}
		cmd
	};

	let camera = r#"{"mode":"lookAt","eye":[0,0,-10],"target":[0,0,0],"fov":50}"#;
	let reason_of = |summary: &serde_json::Value, suffix: &str| -> Option<String> {
		summary["guis"]["skipped"].as_array()?.iter().find(|entry| entry["path"].as_str().is_some_and(|p| p.ends_with(suffix)))?.get("reason")?.as_str().map(str::to_string)
	};

	// 1. Without ROR_RUR or RUR_HOME: Visible is recorded as skipped due to missing rur, along with
	//    every extraction and view-planning skip reason.
	let out_missing = root.join("out-missing");
	let output = base_cmd(None, false)
		.args(["render", model.to_str().unwrap(), "--camera", camera, "--size", "64x64", "--msaa", "1", "--no-bloom", "--no-service", "--offline", "--out", out_missing.to_str().unwrap()])
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(out_missing.join("summary.json")).unwrap()).unwrap();
	assert_eq!(summary["guis"]["drawn"], serde_json::json!([]));
	assert_eq!(reason_of(&summary, "Board.Disabled").as_deref(), Some("disabled"));
	assert_eq!(reason_of(&summary, "Stray").as_deref(), Some("ScreenGui outside PlayerGui"));
	assert_eq!(reason_of(&summary, "Board.BackSide").as_deref(), Some("facing away from camera"));
	assert_eq!(reason_of(&summary, "Board.TooFar").as_deref(), Some("beyond MaxDistance"));
	assert!(reason_of(&summary, "Board.Visible").as_deref().is_some_and(|r| r.contains("rur not found")), "{summary}");

	// 2. With ROR_NO_GUIS=1: Visible is skipped with "ROR_NO_GUIS set".
	let out_disabled = root.join("out-no-guis");
	let output = base_cmd(Some(&mock_rur), true)
		.args(["render", model.to_str().unwrap(), "--camera", camera, "--size", "64x64", "--msaa", "1", "--no-bloom", "--no-service", "--offline", "--out", out_disabled.to_str().unwrap()])
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(out_disabled.join("summary.json")).unwrap()).unwrap();
	assert_eq!(reason_of(&summary, "Board.Visible").as_deref(), Some("ROR_NO_GUIS set"));

	// 3. With RUR_HOME set (and ROR_RUR unset): ror finds rur via RUR_HOME and draws Visible in
	//    both `ror render` and `ror video`, reporting it under `guis.drawn`.
	let out_render = root.join("out-render");
	let output = base_cmd(Some(&mock_rur), false)
		.args(["render", model.to_str().unwrap(), "--camera", camera, "--size", "64x64", "--msaa", "1", "--no-bloom", "--no-service", "--offline", "--out", out_render.to_str().unwrap()])
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(out_render.join("summary.json")).unwrap()).unwrap();
	assert!(summary["guis"]["drawn"].as_array().unwrap().iter().any(|v| v.as_str().is_some_and(|p| p.ends_with("Board.Visible"))), "{summary}");
	assert_eq!(reason_of(&summary, "Board.Disabled").as_deref(), Some("disabled"));
	assert_eq!(reason_of(&summary, "Board.BackSide").as_deref(), Some("facing away from camera"));
	assert_eq!(reason_of(&summary, "Board.TooFar").as_deref(), Some("beyond MaxDistance"));

	let out_video = root.join("out-video");
	let output = base_cmd(Some(&mock_rur), false)
		.args(["video", model.to_str().unwrap(), "--camera", camera, "--duration", "0.1", "--fps", "10", "--format", "png", "--size", "64x64", "--msaa", "1", "--no-bloom", "--no-service", "--offline", "--out", out_video.to_str().unwrap()])
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: serde_json::Value = serde_json::from_slice(&std::fs::read(out_video.join("summary.json")).unwrap()).unwrap();
	assert!(summary["guis"]["drawn"].as_array().unwrap().iter().any(|v| v.as_str().is_some_and(|p| p.ends_with("Board.Visible"))), "{summary}");
	assert_eq!(reason_of(&summary, "Board.Disabled").as_deref(), Some("disabled"));
	assert_eq!(reason_of(&summary, "Stray").as_deref(), Some("ScreenGui outside PlayerGui"));
	assert_eq!(reason_of(&summary, "Board.BackSide").as_deref(), Some("facing away from camera"));

	let _ = std::fs::remove_dir_all(&root);
}
