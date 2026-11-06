// Loads a model or place and prints the extracted scene, at a moment of a playback (a JSON file or
// inline JSON) if one is given: cargo run -p ror --example tools -- scene_dump <file> [select] [playback]
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("usage: scene_dump <file> [select] [playback]");
	let mut document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	if let Some(select) = args.next().filter(|select| !select.is_empty()) {
		document.select(&select)?;
	}
	let playback: Option<ror::scene::Playback> = match args.next() {
		Some(text) if text.trim_start().starts_with('{') => Some(serde_json::from_str(&text)?),
		Some(file) => Some(serde_json::from_str(&std::fs::read_to_string(file)?)?),
		None => None,
	};
	let scene = match &playback {
		Some(playback) => {
			let resolved = playback.resolve(&document)?;
			resolved.apply_state(&mut document)?;
			let mut scene = ror::scene::extract::extract(&document);
			resolved.apply_history(&document, &mut scene);
			scene
		}
		None => ror::scene::extract::extract(&document),
	};
	for part in &scene.parts {
		println!(
			"{} [{}] size={:?} pos={:?} geom={:?} color={:?} mat={} tr={} tex={:?} decals={} sa={:?}",
			part.path,
			part.class,
			part.size,
			part.cframe.translation,
			part.geometry,
			part.color,
			ror::scene::props::enum_name("Material", part.material).unwrap_or("?"),
			part.transparency,
			part.texture,
			part.decals.len(),
			part.surface_appearance.as_ref().map(|appearance| appearance.alpha_mode)
		);
	}
	for light in &scene.lights {
		println!("light {:?}", light);
	}
	for beam in &scene.beams {
		println!("beam {:?}", beam);
	}
	for emitter in &scene.particles {
		match &emitter.births {
			Some(births) => println!("particles {} births {} (ages {:?})", emitter.path, births.len(), births.iter().map(|birth| birth.age).take(8).collect::<Vec<_>>()),
			None => println!("particles {:?}", emitter),
		}
	}
	for trail in &scene.trails {
		println!(
			"trail {} points {} lifetime {} face_camera {} first {:?} last {:?}",
			trail.path,
			trail.points.len(),
			trail.lifetime,
			trail.face_camera,
			trail.points.first().map(|point| (point.time, point.position0, point.position1)),
			trail.points.last().map(|point| (point.time, point.position0, point.position1))
		);
	}
	println!("bounds {:?}", scene.bounds());
	println!("warnings {:?}", scene.warnings);
	Ok(())
}
