// What local lights a game's models use (notes/render.md "Local lights"): one tab-separated line per PointLight,
// SpotLight and SurfaceLight in the files (file, dotted path, class, Enabled, Brightness, Range,
// Color as 0-255, Shadows, Angle, Face, what it sits on: the part's class, material, Transparency
// and size, or the attachment and how deep in attachments), then counts over them:
//   cargo run -p ror --release --example tools -- light_census <files...>
// With --positions, the lights ror draws (enabled, on a part or attachment) and where they are:
//   cargo run -p ror --release --example tools -- light_census --positions <file> [path prefix]
use std::collections::BTreeMap;

use rbx_dom_weak::Instance;
use ror::scene::props;

const LIGHTS: [&str; 3] = ["PointLight", "SpotLight", "SurfaceLight"];

/// Which bucket of `edges` a value falls in, as "lo-hi".
fn bucket(value: f32, edges: &[f32]) -> String {
	for pair in edges.windows(2) {
		if value < pair[1] {
			return format!("{}-{}", pair[0], pair[1]);
		}
	}
	format!("{}+", edges[edges.len() - 1])
}

pub fn main() -> anyhow::Result<()> {
	let files: Vec<String> = std::env::args().skip(1).collect();
	if files.first().is_some_and(|flag| flag == "--positions") {
		let document = ror::scene::load::read_file(std::path::Path::new(files.get(1).expect("file")))?;
		let prefix = files.get(2).cloned().unwrap_or_default();
		let scene = ror::scene::extract::extract(&document);
		for light in scene.lights.iter().filter(|light| light.path.starts_with(&prefix)) {
			let at = light.cframe.translation;
			println!("{}	{:?}	{:.2},{:.2},{:.2}	B{} R{} shadows {}", light.path, light.kind, at.x, at.y, at.z, light.brightness, light.range, light.shadows);
		}
		return Ok(());
	}
	let mut counts: BTreeMap<String, usize> = BTreeMap::new();
	let mut count = |key: String| *counts.entry(key).or_default() += 1;
	println!("file\tpath\tclass\tenabled\tbrightness\trange\tcolor\tshadows\tangle\tface\tparent");
	for file in &files {
		let document = match ror::scene::load::read_file(std::path::Path::new(file)) {
			Ok(document) => document,
			Err(error) => {
				eprintln!("{file}: {error:#}");
				continue;
			}
		};
		let dom = &document.dom;
		let name = std::path::Path::new(file).file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
		let mut stack = vec![dom.root_ref()];
		let mut in_file = 0;
		while let Some(referent) = stack.pop() {
			let Some(instance) = dom.get_by_ref(referent) else {
				continue;
			};
			stack.extend(instance.children().iter().rev().copied());
			if !LIGHTS.contains(&instance.class.as_str()) {
				continue;
			}
			in_file += 1;
			let parent: Option<&Instance> = dom.get_by_ref(instance.parent());
			// the part it sits on, through attachments (how many of them)
			let mut depth = 0;
			let mut holder = parent;
			while let Some(current) = holder
				&& current.class == "Attachment"
			{
				depth += 1;
				holder = dom.get_by_ref(current.parent());
			}
			let on = match holder {
				Some(part) if props::is_a(part, "BasePart") => {
					let size = props::vec3(part, "Size", glam::Vec3::ONE);
					let material = props::enum_name("Material", props::enum_value(part, "Material", 256)).unwrap_or("?");
					format!("{} {} T{} {:.1}x{:.1}x{:.1} att{depth}", part.class, material, props::f32(part, "Transparency", 0.0), size.x, size.y, size.z)
				}
				Some(other) => format!("{} (not a part) att{depth}", other.class),
				None => "none".to_string(),
			};
			let enabled = props::bool(instance, "Enabled", true);
			let brightness = props::f32(instance, "Brightness", 1.0);
			let range = props::f32(instance, "Range", 8.0);
			let color = props::color(instance, "Color", glam::Vec3::ONE) * 255.0;
			let shadows = props::bool(instance, "Shadows", false);
			let angle = props::f32(instance, "Angle", 90.0);
			let face = props::enum_name("NormalId", props::enum_value(instance, "Face", 5)).unwrap_or("?");
			println!(
				"{name}\t{}\t{}\t{enabled}\t{brightness}\t{range}\t{:.0},{:.0},{:.0}\t{shadows}\t{angle}\t{face}\t{on}",
				document.path_of(referent),
				instance.class,
				color.x,
				color.y,
				color.z
			);
			let class = instance.class.as_str();
			count(format!("class {class}"));
			count(format!("{class} enabled {enabled}"));
			count(format!("{class} shadows {shadows}"));
			count(format!("{class} brightness {}", bucket(brightness, &[0.0, 0.5, 1.0, 1.01, 2.0, 3.0, 5.0, 10.0, 20.0, 40.0])));
			count(format!("{class} range {}", bucket(range, &[0.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 45.0, 60.0, 61.0])));
			let white = color.min_element() > 250.0;
			let saturation = (color.max_element() - color.min_element()) / color.max_element().max(1.0);
			count(format!("{class} color {}", if white { "white".to_string() } else { bucket(saturation, &[0.0, 0.25, 0.5, 0.75, 1.0]) }));
			count(format!("{class} on {}", on.split(' ').next().unwrap_or("?")));
			count(format!("{class} attachment depth {depth}"));
			if class != "PointLight" {
				count(format!("{class} angle {}", bucket(angle, &[0.0, 30.0, 60.0, 90.0, 91.0, 120.0, 150.0, 180.0])));
			}
		}
		count(format!("files with {}", if in_file > 0 { "lights" } else { "none" }));
	}
	eprintln!("{} files", files.len());
	for (key, value) in &counts {
		eprintln!("{value:6}  {key}");
	}
	Ok(())
}
