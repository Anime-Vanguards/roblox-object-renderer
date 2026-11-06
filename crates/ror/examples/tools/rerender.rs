//! Re-renders a calibration experiment's runs offline (no Studio) at the capture size, writing
//! `out/calibrate/<experiment>/<run>/<file>`: for comparing ror variants (e.g. `ROR_TUNE_LOD`)
//! against the cached Studio captures.
//!
//! ```text
//! cargo run -p ror --release --example tools -- rerender <experiment> <file name> [width height] [transparent]
//! ```
//!
//! `transparent` draws over a transparent background: the alpha is the subject's coverage (a mask
//! for statistics over the subject alone). `ROR_RERENDER_STRIP=<Class>,...` leaves out the instances
//! of those classes (e.g. what ror drew before a feature, `Shirt,Pants`).

#[cfg(windows)]
pub fn main() -> anyhow::Result<()> {
	use std::sync::Arc;

	use ror::render::gpu::Gpu;
	use ror::render::{Background, Renderer, ViewRequest};

	let mut args = std::env::args().skip(1);
	let name = args.next().expect("experiment name");
	let file = args.next().expect("output file name");
	let width: u32 = args.next().and_then(|value| value.parse().ok()).unwrap_or(1644);
	let height: u32 = args.next().and_then(|value| value.parse().ok()).unwrap_or(1116);
	let background = if args.next().is_some_and(|value| value == "transparent") { Background::Transparent } else { Background::Sky };
	let experiment = ror::studio::calibrate::experiment(&name)?;
	let out = ror::output::repo_dir().join("out").join("calibrate").join(&experiment.name);
	// the fixture, as `ror studio calibrate` writes it
	ror::output::write_atomic(&out.join("model.rbxm"), &experiment.model.to_rbxm()?)?;
	let mut document = experiment.model.into_document();
	let strip: Vec<String> = std::env::var("ROR_RERENDER_STRIP").unwrap_or_default().split(',').map(|class| class.trim().to_string()).filter(|class| !class.is_empty()).collect();
	let mut doomed = Vec::new();
	let mut stack = document.roots.clone();
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else {
			continue;
		};
		if strip.iter().any(|class| class == instance.class.as_str()) {
			doomed.push(referent);
			continue;
		}
		stack.extend(instance.children().iter().copied());
	}
	for referent in doomed {
		document.dom.destroy(referent);
	}
	let base = ror::scene::extract::extract(&document);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = Renderer::new(Arc::new(Gpu::new()?), assets)?;
	for run in &experiment.runs {
		let mut scene = base.clone();
		scene.environment = run.environment.clone();
		let request = ViewRequest { width, height, camera: run.camera.clone(), background, ..Default::default() };
		let (images, _) = renderer.render(&scene, &[request])?;
		let image = images.into_iter().next().expect("one view");
		ror::output::write_png(&out.join(&run.label).join(&file), image.width, image.height, &image.rgba, true)?;
	}
	println!("{} runs -> {}", experiment.runs.len(), out.display());
	Ok(())
}

#[cfg(not(windows))]
pub fn main() {}
