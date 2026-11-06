//! GUI diagnostics: the SurfaceGuis and BillboardGuis of model and place files, with what they
//! draw on and the properties that place and light them.
//!
//! ```text
//! cargo run -p ror --release --example tools -- gui_scan <model files...> [--tree]
//! ```
//!
//! `--tree` also lists each GUI's descendants (class, name, size).

use std::collections::BTreeMap;

use rbx_dom_weak::types::{Ref, Variant};
use ror::scene::{load, props};

fn describe(value: Option<&Variant>) -> String {
	match value {
		Some(Variant::Bool(value)) => format!("{value}"),
		Some(Variant::Float32(value)) => format!("{value}"),
		Some(Variant::Float64(value)) => format!("{value}"),
		Some(Variant::Int32(value)) => format!("{value}"),
		Some(Variant::Enum(value)) => format!("{}", value.to_u32()),
		Some(Variant::EnumItem(value)) => format!("{}", value.value),
		Some(Variant::Vector2(value)) => format!("({}, {})", value.x, value.y),
		Some(Variant::Vector3(value)) => format!("({}, {}, {})", value.x, value.y, value.z),
		Some(Variant::UDim2(value)) => format!("{{{}, {}}}, {{{}, {}}}", value.x.scale, value.x.offset, value.y.scale, value.y.offset),
		Some(Variant::Ref(value)) => if value.is_some() { "ref".into() } else { "nil".into() },
		Some(other) => format!("{other:?}"),
		None => "-".into(),
	}
}

fn print_tree(document: &load::Document, referent: Ref, depth: usize) {
	let Some(instance) = document.dom.get_by_ref(referent) else {
		return;
	};
	println!("{}{} {} size {} visible {}", "  ".repeat(depth), instance.class, instance.name, describe(props::get(instance, "Size")), describe(props::get(instance, "Visible")));
	for &child in instance.children() {
		print_tree(document, child, depth + 1);
	}
}

pub fn main() -> anyhow::Result<()> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let tree = args.iter().any(|arg| arg == "--tree");
	for file in args.iter().filter(|arg| !arg.starts_with("--")) {
		let document = load::read_file(std::path::Path::new(file))?;
		let mut stack = vec![document.dom.root_ref()];
		let mut totals: BTreeMap<String, usize> = BTreeMap::new();
		while let Some(referent) = stack.pop() {
			let Some(instance) = document.dom.get_by_ref(referent) else {
				continue;
			};
			stack.extend(instance.children().iter().rev().copied());
			let class = instance.class.as_str();
			if class != "SurfaceGui" && class != "BillboardGui" {
				continue;
			}
			let parent = document.dom.get_by_ref(instance.parent()).map(|parent| parent.class.to_string()).unwrap_or_default();
			let mut classes: BTreeMap<String, usize> = BTreeMap::new();
			let mut inner = instance.children().to_vec();
			while let Some(child) = inner.pop() {
				if let Some(child) = document.dom.get_by_ref(child) {
					*classes.entry(child.class.to_string()).or_default() += 1;
					inner.extend(child.children().iter().copied());
				}
			}
			for (name, count) in &classes {
				*totals.entry(name.clone()).or_default() += count;
			}
			let names: &[&str] = if class == "SurfaceGui" {
				&["Enabled", "Adornee", "Face", "SizingMode", "CanvasSize", "PixelsPerStud", "LightInfluence", "Brightness", "AlwaysOnTop", "ZOffset", "ClipsDescendants", "MaxDistance", "ZIndexBehavior"]
			} else {
				&["Enabled", "Adornee", "Size", "SizeOffset", "StudsOffset", "StudsOffsetWorldSpace", "ExtentsOffset", "ExtentsOffsetWorldSpace", "LightInfluence", "Brightness", "AlwaysOnTop", "MaxDistance", "DistanceLowerLimit", "DistanceUpperLimit", "DistanceStep", "ClipsDescendants", "ZIndexBehavior"]
			};
			let values: Vec<String> = names.iter().map(|name| format!("{name}={}", describe(props::get(instance, name)))).collect();
			println!("{} [{class} in {parent}] {}", document.path_of(referent), values.join(" "));
			// where it draws: the adornee or parent part's position, size and look direction
			let target = props::reference(instance, "Adornee").unwrap_or_else(|| instance.parent());
			if let Some(part) = document.dom.get_by_ref(target)
				&& let Some(cframe) = props::cframe(part, "CFrame")
			{
				let look = -cframe.matrix3.z_axis;
				println!("    at {:?} size {} look {:?}", cframe.translation.to_array().map(|v| (v * 100.0).round() / 100.0), describe(props::get(part, "Size")), look.to_array().map(|v| (v * 100.0).round() / 100.0));
			}
			println!("    {}", classes.iter().map(|(name, count)| format!("{name} {count}")).collect::<Vec<_>>().join(", "));
			if tree {
				for &child in instance.children() {
					print_tree(&document, child, 2);
				}
			}
		}
		println!("{file}: descendants {}", totals.iter().map(|(name, count)| format!("{name} {count}")).collect::<Vec<_>>().join(", "));
	}
	Ok(())
}
