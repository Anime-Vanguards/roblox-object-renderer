//! End-to-end check that `ror render` and `ror video` pass `--playback <file>` by path to the
//! render service and automatically fall back to in-process rendering when the service refuses a
//! request.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ror::scene::build::{ModelBuilder, PartSpec};
use ror::service::{ServiceInfo, build_info_path, http};

fn build_id_for(exe: &Path) -> String {
	let meta = std::fs::metadata(exe).unwrap();
	let modified = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_millis();
	format!("{}-{}-{}", env!("CARGO_PKG_VERSION"), meta.len(), modified)
}

#[test]
fn cli_passes_playback_by_path_and_falls_back_in_process_on_service_refusal() {
	let root = std::env::temp_dir().join(format!("ror-service-fallback-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	let state = root.join("state");
	std::fs::create_dir_all(&state).unwrap();

	let model_path = root.join("box.rbxm");
	let mut model = ModelBuilder::new("Box");
	model.part(&PartSpec::default());
	let document = model.into_document();
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &document.roots).unwrap();
	std::fs::write(&model_path, bytes).unwrap();

	let playback_path = root.join("playback.json");
	std::fs::write(&playback_path, r#"{"actions":[{"at":0,"target":"Box","emit":1}]}"#).unwrap();

	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	let port = listener.local_addr().unwrap().port();
	listener.set_nonblocking(true).unwrap();

	let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
	let seen_bodies: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
	let stop_server = stop.clone();
	let bodies_server = seen_bodies.clone();
	let server = std::thread::spawn(move || {
		while !stop_server.load(std::sync::atomic::Ordering::Relaxed) {
			let Ok((stream, _)) = listener.accept() else {
				std::thread::sleep(Duration::from_millis(5));
				continue;
			};
			let _ = stream.set_nonblocking(false);
			let Ok(request) = http::read_request(&stream) else {
				continue;
			};
			match (request.method.as_str(), request.path.as_str()) {
				("GET", "/health") => {
					let _ = http::write_response_and_close(&stream, 200, "application/json", br#"{"shuttingDown":false}"#);
				}
				("POST", path @ ("/render" | "/video")) => {
					if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&request.body) {
						bodies_server.lock().unwrap().push((path.to_string(), json));
					}
					let _ = http::write_response_and_close(&stream, 503, "application/json", br#"{"error":"service busy (test refusal)"}"#);
				}
				_ => {
					let _ = http::write_response_and_close(&stream, 404, "application/json", br#"{"error":"not found"}"#);
				}
			}
		}
	});

	let binary = Path::new(env!("CARGO_BIN_EXE_ror"));
	let build = build_id_for(binary);
	let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
	let info = ServiceInfo { port, pid: std::process::id(), build: build.clone(), started, source: Some(binary.display().to_string()) };
	let record = build_info_path(&state, &build);
	std::fs::create_dir_all(record.parent().unwrap()).unwrap();
	std::fs::write(&record, serde_json::to_vec(&info).unwrap()).unwrap();

	let run = |args: &[&str]| {
		let mut cmd = Command::new(binary);
		cmd.args(args).env("ROR_STATE_DIR", &state).env("ROR_STORE", "0");
		for (name, _) in std::env::vars() {
			if name.starts_with("ROR_") && name != "ROR_STATE_DIR" && name != "ROR_STORE" {
				cmd.env_remove(&name);
			}
		}
		cmd.output().unwrap()
	};

	// 1. ror render with --playback <file>
	let out_render = root.join("out-render");
	let output = run(&[
		"render",
		model_path.to_str().unwrap(),
		"--playback",
		playback_path.to_str().unwrap(),
		"--views",
		"iso",
		"--size",
		"32x32",
		"--msaa",
		"1",
		"--no-bloom",
		"--offline",
		"--out",
		out_render.to_str().unwrap(),
	]);
	let stderr = String::from_utf8_lossy(&output.stderr);
	assert!(output.status.success(), "render failed: {stderr}");
	assert!(stderr.contains("falling back to in-process rendering"), "missing fallback notice: {stderr}");
	assert!(stderr.contains("service busy (test refusal)"), "missing refusal reason: {stderr}");
	assert!(out_render.join("iso.png").is_file());
	assert!(out_render.join("summary.json").is_file());

	// 2. ror video with --playback <file>
	let out_video = root.join("out-video");
	let output = run(&[
		"video",
		model_path.to_str().unwrap(),
		"--playback",
		playback_path.to_str().unwrap(),
		"--duration",
		"0.1",
		"--fps",
		"10",
		"--format",
		"png",
		"--size",
		"32x32",
		"--msaa",
		"1",
		"--no-bloom",
		"--offline",
		"--out",
		out_video.to_str().unwrap(),
	]);
	let stderr = String::from_utf8_lossy(&output.stderr);
	assert!(output.status.success(), "video failed: {stderr}");
	assert!(stderr.contains("falling back to in-process rendering"), "missing fallback notice: {stderr}");
	assert!(stderr.contains("service busy (test refusal)"), "missing refusal reason: {stderr}");
	assert!(out_video.join("summary.json").is_file());
	assert!(out_video.join("frames").join("0000.png").is_file());

	stop.store(true, std::sync::atomic::Ordering::Relaxed);
	let _ = server.join();

	let bodies = seen_bodies.lock().unwrap();
	assert_eq!(bodies.len(), 2, "expected one /render and one /video request: {bodies:?}");
	for (path, json) in bodies.iter() {
		assert!(json.get("playbackFile").and_then(|v| v.as_str()).is_some(), "{path} did not send playbackFile: {json}");
		assert!(json.get("playback").is_none() || json["playback"].is_null(), "{path} inlined playback JSON instead of path: {json}");
	}

	let _ = std::fs::remove_dir_all(&root);
}
