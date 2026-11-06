//! Wraps a calibration experiment's fixture into place files, one per run, with the run's Lighting
//! (`out/calibrate/<experiment>/<run>/place.rbxl`): for rendering the same scene with another ror
//! build, which then reads the lighting from the place.
//!
//! ```text
//! cargo run -p ror --release --example tools -- wrap_place <experiment>
//! ```

#[cfg(windows)]
pub fn main() -> anyhow::Result<()> {
	use rbx_dom_weak::{InstanceBuilder, WeakDom};

	let name = std::env::args().nth(1).expect("experiment name");
	let experiment = ror::studio::calibrate::experiment(&name)?;
	let out = ror::output::repo_dir().join("out").join("calibrate").join(&experiment.name);
	for run in &experiment.runs {
		let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
		let root = dom.root_ref();
		let workspace = dom.insert(root, InstanceBuilder::new("Workspace").with_name("Workspace"));
		let lighting = dom.insert(root, ror::studio::place::lighting_builder(&run.environment));
		let copy = experiment.model.dom.clone_into_external(experiment.model.model, &mut dom);
		dom.transfer_within(copy, workspace);
		let mut bytes = Vec::new();
		rbx_binary::to_writer(&mut bytes, &dom, &[workspace, lighting]).map_err(|error| anyhow::anyhow!("{error}"))?;
		ror::output::write_atomic(&out.join(&run.label).join("place.rbxl"), &bytes)?;
	}
	println!("{} places in {}", experiment.runs.len(), out.display());
	Ok(())
}

#[cfg(not(windows))]
pub fn main() {}
