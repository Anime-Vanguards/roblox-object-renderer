// Prints the instances under a dotted path of a model file with their properties (one line each,
// sequences and long values shortened), for reading what an effect is made of:
//   cargo run -p ror --release --example tools -- vfx_dump <file> <dotted path> [class filter]
use rbx_dom_weak::types::Variant;

fn short(value: &Variant) -> String {
	short_in(value, None)
}

/// A value shortened; references by the name path of what they point at.
fn short_in(value: &Variant, dom: Option<&rbx_dom_weak::WeakDom>) -> String {
	if let (Variant::Ref(referent), Some(dom)) = (value, dom) {
		let mut names = Vec::new();
		let mut current = *referent;
		while let Some(instance) = dom.get_by_ref(current) {
			names.push(instance.name.clone());
			current = instance.parent();
		}
		names.reverse();
		return if names.is_empty() { "none".into() } else { format!("ref {}", names.join(".")) };
	}
	let text = match value {
		Variant::NumberSequence(sequence) => sequence.keypoints.iter().map(|key| if key.envelope.abs() > 1e-6 { format!("{:.3}:{:.3}±{:.3}", key.time, key.value, key.envelope) } else { format!("{:.3}:{:.3}", key.time, key.value) }).collect::<Vec<_>>().join(" "),
		Variant::ColorSequence(sequence) => sequence.keypoints.iter().map(|key| format!("{:.3}:({:.2},{:.2},{:.2})", key.time, key.color.r, key.color.g, key.color.b)).collect::<Vec<_>>().join(" "),
		Variant::NumberRange(range) => format!("{}..{}", range.min, range.max),
		Variant::Color3(color) => format!("({:.3},{:.3},{:.3})", color.r, color.g, color.b),
		Variant::Color3uint8(color) => format!("({},{},{})", color.r, color.g, color.b),
		Variant::Vector3(v) => format!("({:.3},{:.3},{:.3})", v.x, v.y, v.z),
		Variant::Vector2(v) => format!("({:.3},{:.3})", v.x, v.y),
		Variant::CFrame(c) => format!("pos({:.2},{:.2},{:.2}) x({:.2},{:.2},{:.2}) y({:.2},{:.2},{:.2})", c.position.x, c.position.y, c.position.z, c.orientation.x.x, c.orientation.y.x, c.orientation.z.x, c.orientation.x.y, c.orientation.y.y, c.orientation.z.y),
		Variant::Attributes(attributes) => attributes.iter().map(|(name, value)| format!("{name}={}", short(value))).collect::<Vec<_>>().join(","),
		Variant::Enum(value) => format!("{}", value.to_u32()),
		Variant::Ref(_) => "ref".into(),
		Variant::Content(content) => format!("{content:?}"),
		other => format!("{other:?}"),
	};
	if text.len() > 160 { format!("{}…", &text[..text.char_indices().nth(160).map(|(index, _)| index).unwrap_or(text.len())]) } else { text }
}

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let dotted = args.next().expect("dotted path");
	let filter = args.next();
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut current = dom.root_ref();
	for segment in dotted.split('.') {
		current = dom.get_by_ref(current).and_then(|instance| instance.children().iter().copied().find(|&child| dom.get_by_ref(child).is_some_and(|child| child.name == segment))).ok_or_else(|| anyhow::anyhow!("no {segment:?}"))?;
	}
	let mut stack = vec![(current, 0usize)];
	while let Some((referent, depth)) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else {
			continue;
		};
		for &child in instance.children().iter().rev() {
			stack.push((child, depth + 1));
		}
		if filter.as_deref().is_some_and(|class| class != instance.class.as_str()) {
			continue;
		}
		let mut properties: Vec<_> = instance.properties.iter().filter(|(name, _)| !matches!(name.as_str(), "UniqueId" | "HistoryId" | "Capabilities" | "Tags" | "SourceAssetId" | "Sandboxed" | "DefinesCapabilities" | "PhysicalConfigData" | "PhysicsData" | "MeshData" | "ChildData" | "InitialSize" | "AeroMeshData" | "FluidFidelityInternal")).collect();
		properties.sort_by_key(|(name, _)| name.as_str());
		let listed: Vec<String> = properties.iter().map(|(name, value)| format!("{name}={}", short_in(value, Some(dom)))).collect();
		println!("{}{} {:?}: {}", "  ".repeat(depth), instance.class, instance.name, listed.join("; "));
	}
	Ok(())
}
