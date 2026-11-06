// Counts what effects use over model files: instance classes, the ParticleEmitter, Trail and Beam
// features, materials of parts, and the emit attributes (EmitCount, EmitDelay,
// EmitDuration):
//   cargo run -p ror --release --example tools -- vfx_census <files...>
// With --effects, one tab-separated line per effect (each part or model outside any other, through
// folders: a bundle's Unit.Skill.Effect): file, dotted path, class, then every feature it uses with
// its count (picking effects for `vfx-attack:<bundle>|auto:<path>` runs that cover the features):
//   cargo run -p ror --release --example tools -- vfx_census --effects <files...>
use std::collections::BTreeMap;

use rbx_dom_weak::Instance;
use rbx_dom_weak::types::Variant;
use ror::scene::props;

#[derive(Default)]
struct Census {
	classes: BTreeMap<String, usize>,
	features: BTreeMap<String, usize>,
	files: BTreeMap<String, usize>,
}

impl Census {
	fn count(&mut self, key: impl Into<String>) {
		*self.features.entry(key.into()).or_default() += 1;
	}
}

fn sequence_varies(instance: &Instance, name: &str) -> bool {
	match props::get(instance, name) {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.windows(2).any(|pair| (pair[0].value - pair[1].value).abs() > 1e-4),
		_ => false,
	}
}

fn sequence_envelope(instance: &Instance, name: &str) -> bool {
	match props::get(instance, name) {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.iter().any(|key| key.envelope.abs() > 1e-4),
		_ => false,
	}
}

fn colors_vary(instance: &Instance) -> bool {
	match props::get(instance, "Color") {
		Some(Variant::ColorSequence(sequence)) => sequence.keypoints.windows(2).any(|pair| {
			let (a, b) = (pair[0].color, pair[1].color);
			(a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs() > 1e-3
		}),
		_ => false,
	}
}

fn range(instance: &Instance, name: &str) -> (f32, f32) {
	match props::get(instance, name) {
		Some(Variant::NumberRange(value)) => (value.min, value.max),
		_ => (0.0, 0.0),
	}
}

fn attribute<'a>(instance: &'a Instance, name: &str) -> Option<&'a Variant> {
	match instance.properties.get(&rbx_dom_weak::ustr("Attributes")) {
		Some(Variant::Attributes(attributes)) => attributes.get(name),
		_ => None,
	}
}

fn number(value: &Variant) -> Option<f64> {
	match value {
		Variant::Float64(value) => Some(*value),
		Variant::Float32(value) => Some(*value as f64),
		Variant::Int32(value) => Some(*value as f64),
		Variant::Int64(value) => Some(*value as f64),
		_ => None,
	}
}

fn bucket(value: f32, edges: &[f32]) -> String {
	for pair in edges.windows(2) {
		if value < pair[1] {
			return format!("{}-{}", pair[0], pair[1]);
		}
	}
	format!(">={}", edges[edges.len() - 1])
}

fn particle(census: &mut Census, instance: &Instance, parent_class: &str) {
	census.count("emitter: total");
	census.count(format!("emitter: parent {}", if props::class_is_a(parent_class, "Attachment") { "Attachment/Bone" } else if props::class_is_a(parent_class, "BasePart") { "BasePart" } else { "other" }));
	let flipbook = props::enum_value(instance, "FlipbookLayout", 0);
	if flipbook != 0 {
		census.count(format!("emitter: flipbook layout {}", props::enum_name("ParticleFlipbookLayout", flipbook).unwrap_or("?")));
		census.count(format!("emitter: flipbook mode {}", props::enum_name("ParticleFlipbookMode", props::enum_value(instance, "FlipbookMode", 0)).unwrap_or("?")));
		if props::bool(instance, "FlipbookStartRandom", false) {
			census.count("emitter: flipbook start random");
			census.count(format!("emitter: flipbook start random with {}", props::enum_name("ParticleFlipbookMode", props::enum_value(instance, "FlipbookMode", 0)).unwrap_or("?")));
		}
		if !props::bool(instance, "FlipbookBlendFrames", true) {
			census.count("emitter: flipbook blend frames off");
		}
	}
	let emission = props::f32(instance, "LightEmission", 0.0);
	census.count(format!("emitter: LightEmission {}", if emission <= 0.0 { if emission < 0.0 { "<0" } else { "0" } } else if emission >= 1.0 { "1" } else { "0-1" }));
	if props::f32(instance, "LightInfluence", 0.0) > 0.0 {
		census.count("emitter: LightInfluence > 0");
	}
	census.count(format!("emitter: Brightness {}", bucket(props::f32(instance, "Brightness", 1.0), &[0.0, 1.0001, 2.0, 5.0, 10.0, 20.0])));
	let orientation = props::enum_value(instance, "Orientation", 0);
	census.count(format!("emitter: orientation {}", props::enum_name("ParticleOrientation", orientation).unwrap_or("?")));
	if orientation >= 2 {
		let speed = range(instance, "Speed");
		census.count(format!("emitter: orientation {} with Speed max {}", props::enum_name("ParticleOrientation", orientation).unwrap_or("?"), if speed.1 <= 0.0 { "0" } else if speed.1 < 0.01 { "< 0.01" } else { ">= 0.01" }));
	}
	if props::f32(instance, "LightInfluence", 0.0) > 0.0 && props::f32(instance, "Brightness", 1.0) > 1.0 {
		census.count("emitter: LightInfluence > 0 and Brightness > 1");
	}
	if !props::class_is_a(parent_class, "Attachment") {
		let shape = props::enum_value(instance, "Shape", 0);
		census.count(format!("emitter: shape {}", props::enum_name("ParticleEmitterShape", shape).unwrap_or("?")));
		if shape != 0 {
			let partial = props::f32(instance, "ShapePartial", 1.0);
			let bucket = if partial < 0.0 { "<0" } else if partial == 0.0 { "0" } else if partial < 1.0 { "0-1" } else if partial == 1.0 { "1" } else { ">1" };
			census.count(format!("emitter: shape {} ShapePartial {bucket}", props::enum_name("ParticleEmitterShape", shape).unwrap_or("?")));
		}
		if props::enum_value(instance, "ShapeStyle", 0) == 1 {
			census.count("emitter: shape style Surface");
		}
		if props::enum_value(instance, "ShapeInOut", 0) != 0 {
			census.count("emitter: shape in/out not Outward");
		}
	}
	if props::f32(instance, "Drag", 0.0) != 0.0 {
		census.count("emitter: Drag");
	}
	if props::vec3(instance, "Acceleration", glam::Vec3::ZERO) != glam::Vec3::ZERO {
		census.count("emitter: Acceleration");
	}
	if props::vec2(instance, "SpreadAngle", [0.0, 0.0]) != [0.0, 0.0] {
		census.count("emitter: SpreadAngle");
	}
	let speed = range(instance, "Speed");
	if speed.1 > 0.0 {
		census.count("emitter: Speed > 0");
	}
	let rotation = range(instance, "Rotation");
	if rotation != (0.0, 0.0) {
		census.count("emitter: Rotation");
	}
	if range(instance, "RotSpeed") != (0.0, 0.0) {
		census.count("emitter: RotSpeed");
	}
	for name in ["Size", "Transparency", "Squash"] {
		if sequence_varies(instance, name) {
			census.count(format!("emitter: {name} varies"));
		}
		if sequence_envelope(instance, name) {
			census.count(format!("emitter: {name} envelope"));
		}
	}
	if colors_vary(instance) {
		census.count("emitter: Color varies");
	}
	let time_scale = props::f32(instance, "TimeScale", 1.0);
	if (time_scale - 1.0).abs() > 1e-4 {
		census.count(format!("emitter: TimeScale {}", bucket(time_scale, &[0.0, 0.25, 0.5, 0.75, 1.0])));
	}
	if props::f32(instance, "ZOffset", 0.0) != 0.0 {
		census.count("emitter: ZOffset");
	}
	if props::bool(instance, "LockedToPart", false) {
		census.count("emitter: LockedToPart");
	}
	if props::f32(instance, "VelocityInheritance", 0.0) != 0.0 {
		census.count("emitter: VelocityInheritance");
	}
	if props::bool(instance, "WindAffectsDrag", false) {
		census.count("emitter: WindAffectsDrag");
	}
	if props::bool(instance, "Enabled", true) {
		census.count("emitter: Enabled");
	}
	if props::f32(instance, "Rate", 20.0) > 0.0 && props::bool(instance, "Enabled", true) {
		census.count("emitter: enabled with Rate > 0");
	}
	let runs = props::bool(instance, "Enabled", true) || attribute(instance, "EmitDuration").and_then(number).is_some_and(|duration| duration > 0.0);
	if runs && props::f32(instance, "Rate", 20.0) > 400.0 {
		census.count("emitter: Rate above 400, enabled or with an EmitDuration");
	}
	if props::content(instance, &["TextureContent", "Texture"]).is_none() {
		census.count("emitter: no texture");
	}
	let lifetime = range(instance, "Lifetime");
	census.count(format!("emitter: Lifetime max {}", bucket(lifetime.1, &[0.0, 0.25, 0.5, 1.0, 2.0, 5.0])));
	match attribute(instance, "EmitCount").and_then(number) {
		Some(count) => census.count(format!("emitter: EmitCount {}", bucket(count as f32, &[0.0, 1.0, 2.0, 5.0, 10.0, 25.0, 100.0]))),
		None => census.count("emitter: EmitCount absent (emits 5)"),
	}
	if attribute(instance, "EmitDelay").and_then(number).is_some_and(|delay| delay > 0.0) {
		census.count("emitter: EmitDelay > 0");
	}
	if attribute(instance, "EmitDuration").and_then(number).is_some_and(|duration| duration > 0.0) {
		census.count("emitter: EmitDuration > 0");
		census.count(format!("emitter: EmitDuration with Rate {}", bucket(props::f32(instance, "Rate", 20.0), &[0.0, 1.0, 10.0, 50.0, 200.0])));
	}
	// where the attributes mode's Enabled window (from the call for EmitDuration, 0 without one)
	// differs from the cutscene helpers' (from EmitDelay, only with an EmitDuration)
	let delay = attribute(instance, "EmitDelay").and_then(number).unwrap_or(0.0);
	let duration = attribute(instance, "EmitDuration").and_then(number).unwrap_or(0.0);
	let streams = props::f32(instance, "Rate", 20.0) > 0.0;
	if streams && delay > 0.0 && duration > 0.0 {
		census.count("emit window: Rate > 0, EmitDelay and EmitDuration > 0 (streams from the call, not the delay)");
	}
	if streams && props::bool(instance, "Enabled", true) && duration <= 0.0 {
		census.count("emit window: enabled in the file with Rate > 0, no EmitDuration (switched off at the call)");
	}
	for name in ["Size", "Transparency", "Squash"] {
		if let Some(Variant::NumberSequence(sequence)) = props::get(instance, name) {
			let largest = sequence.keypoints.iter().map(|key| key.envelope.abs()).fold(0.0f32, f32::max);
			if largest > 1e-4 {
				let mean = sequence.keypoints.iter().map(|key| key.value.abs()).sum::<f32>() / sequence.keypoints.len().max(1) as f32;
				census.count(format!("emitter: {name} envelope / mean value {}", bucket(largest / mean.max(1e-3), &[0.0, 0.1, 0.25, 0.5, 1.0])));
			}
		}
	}
	if props::f32(instance, "Brightness", 1.0) > 1.0 && emission >= 1.0 {
		census.count("emitter: additive and Brightness > 1");
	}
	// how far Squash reaches (measured to ±2: `particle-orient`)
	if let Some(Variant::NumberSequence(sequence)) = props::get(instance, "Squash") {
		let reach = sequence.keypoints.iter().map(|key| key.value.abs() + key.envelope.abs()).fold(0.0f32, f32::max);
		if reach > 2.0 {
			census.count(format!("emitter: Squash reaching {}", bucket(reach, &[2.0, 3.0, 5.0, 10.0])));
		}
	}
	// a black emitter (Color or Brightness 0) beside a lit one at the same place (an effect's SpeedLines,
	// an effect's dash): which one Studio draws on top
	if props::f32(instance, "Brightness", 1.0) <= 0.0 {
		census.count("emitter: Brightness 0");
	}
}

fn trail(census: &mut Census, instance: &Instance) {
	census.count("trail: total");
	if props::bool(instance, "FaceCamera", false) {
		census.count("trail: FaceCamera");
	}
	census.count(format!("trail: TextureMode {}", props::enum_name("TextureMode", props::enum_value(instance, "TextureMode", 0)).unwrap_or("?")));
	if props::content(instance, &["TextureContent", "Texture"]).is_some() {
		census.count("trail: textured");
	}
	census.count(format!("trail: Lifetime {}", bucket(props::f32(instance, "Lifetime", 2.0), &[0.0, 0.1, 0.25, 0.5, 1.0, 2.0])));
	if props::f32(instance, "MinLength", 0.1) != 0.1 {
		census.count("trail: MinLength not 0.1");
	}
	if props::f32(instance, "MaxLength", 0.0) > 0.0 {
		census.count("trail: MaxLength > 0");
	}
	if sequence_varies(instance, "WidthScale") {
		census.count("trail: WidthScale varies");
	}
	if sequence_varies(instance, "Transparency") {
		census.count("trail: Transparency varies");
	}
	if colors_vary(instance) {
		census.count("trail: Color varies");
	}
	let emission = props::f32(instance, "LightEmission", 0.0);
	census.count(format!("trail: LightEmission {}", if emission <= 0.0 { "0" } else if emission >= 1.0 { "1" } else { "0-1" }));
	if props::f32(instance, "LightInfluence", 1.0) > 0.0 {
		census.count("trail: LightInfluence > 0");
	}
	census.count(format!("trail: Brightness {}", bucket(props::f32(instance, "Brightness", 1.0), &[0.0, 1.0001, 2.0, 5.0, 10.0])));
	if props::bool(instance, "Enabled", true) {
		census.count("trail: Enabled");
	}
}

/// A ParticleEmitter's world rotation: its part's, or its attachment's (through attachments inside
/// attachments) times its part's; None outside a part.
fn emitter_rotation(dom: &rbx_dom_weak::WeakDom, instance: &Instance) -> Option<glam::Mat3> {
	let mut rotation = glam::Mat3::IDENTITY;
	let mut current = dom.get_by_ref(instance.parent())?;
	loop {
		let local = props::cframe(current, "CFrame").map(|cframe| glam::Mat3::from(cframe.matrix3)).unwrap_or(glam::Mat3::IDENTITY);
		if props::class_is_a(current.class.as_str(), "BasePart") {
			return Some(local * rotation);
		}
		if !props::class_is_a(current.class.as_str(), "Attachment") {
			return None;
		}
		rotation = local * rotation;
		current = dom.get_by_ref(current.parent())?;
	}
}

/// VelocityPerpendicular emitters sending their particles straight up or down (no spread): a
/// direction the image's up cannot be taken from.
fn vertical(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	if props::enum_value(instance, "Orientation", 0) != 3 || props::vec2(instance, "SpreadAngle", [0.0, 0.0]) != [0.0, 0.0] {
		return;
	}
	let Some(rotation) = emitter_rotation(dom, instance) else {
		return;
	};
	// Right, Top, Back, Left, Bottom, Front
	let local = match props::enum_value(instance, "EmissionDirection", 1) {
		0 => glam::Vec3::X,
		1 => glam::Vec3::Y,
		2 => glam::Vec3::Z,
		3 => glam::Vec3::NEG_X,
		4 => glam::Vec3::NEG_Y,
		_ => glam::Vec3::NEG_Z,
	};
	let direction = (rotation * local).normalize_or_zero();
	let tilt = direction.y.abs().clamp(0.0, 1.0).acos().to_degrees();
	// a part's Sphere and Cylinder send their particles out from their shape, not along the direction
	let on_part = dom.get_by_ref(instance.parent()).is_some_and(|parent| props::class_is_a(parent.class.as_str(), "BasePart"));
	if on_part && matches!(props::enum_value(instance, "Shape", 0), 1 | 2) {
		return;
	}
	if tilt < 0.01 {
		census.count("emitter: VelocityPerpendicular straight up or down");
		if props::bool(instance, "Enabled", true) || props::f32(instance, "Rate", 20.0) > 0.0 {
			census.count("emitter: VelocityPerpendicular straight up or down, a Rate");
		}
		// within render::particles' VERTICAL of the world's up and along the emitter's look vector:
		// Studio draws none of them (vfx-perpendicular-frames, vfx-tilt)
		let look = -(rotation * glam::Vec3::Z).normalize_or_zero();
		if direction.cross(glam::Vec3::Y).length() < 2.5e-7 && direction.dot(look) > 0.999 {
			census.count("emitter: VelocityPerpendicular straight up or down along its look vector (not drawn)");
		}
	} else if tilt < 1.0 {
		census.count("emitter: VelocityPerpendicular within 1 degree of vertical");
	}
}

/// An attachment's world position (its part's CFrame times its own, through attachments inside
/// attachments), None outside a part.
fn attachment_position(dom: &rbx_dom_weak::WeakDom, referent: rbx_dom_weak::types::Ref) -> Option<glam::Vec3> {
	let attachment = dom.get_by_ref(referent).filter(|instance| props::class_is_a(instance.class.as_str(), "Attachment"))?;
	let local = props::cframe(attachment, "CFrame").unwrap_or(glam::Affine3A::IDENTITY);
	let parent = dom.get_by_ref(attachment.parent())?;
	let world = if props::class_is_a(parent.class.as_str(), "BasePart") {
		props::cframe(parent, "CFrame")? * local
	} else if props::class_is_a(parent.class.as_str(), "Attachment") {
		glam::Affine3A::from_translation(attachment_position(dom, attachment.parent())?) * local
	} else {
		return None;
	};
	Some(glam::Vec3::from(world.translation))
}

fn beam(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	census.count("beam: total");
	let ends = (props::reference(instance, "Attachment0").and_then(|end| attachment_position(dom, end)), props::reference(instance, "Attachment1").and_then(|end| attachment_position(dom, end)));
	if let (Some(a), Some(b)) = ends {
		let apart = a.distance(b);
		let curved = props::f32(instance, "CurveSize0", 0.0) != 0.0 || props::f32(instance, "CurveSize1", 0.0) != 0.0;
		if apart < 1e-3 {
			census.count(if curved { "beam: attachments at one point, curved" } else { "beam: attachments at one point" });
			if props::bool(instance, "Enabled", true) {
				census.count("beam: attachments at one point, enabled");
			}
		} else if apart < 0.1 {
			census.count("beam: attachments under 0.1 studs apart");
		}
	}
	if props::f32(instance, "TextureSpeed", 1.0) != 0.0 {
		census.count("beam: TextureSpeed != 0");
	}
	if props::bool(instance, "FaceCamera", false) {
		census.count("beam: FaceCamera");
	}
	if props::f32(instance, "CurveSize0", 0.0) != 0.0 || props::f32(instance, "CurveSize1", 0.0) != 0.0 {
		census.count("beam: curved");
	}
	let emission = props::f32(instance, "LightEmission", 0.0);
	census.count(format!("beam: LightEmission {}", if emission <= 0.0 { "0" } else if emission >= 1.0 { "1" } else { "0-1" }));
	census.count(format!("beam: Brightness {}", bucket(props::f32(instance, "Brightness", 1.0), &[0.0, 1.0001, 2.0, 5.0, 10.0])));
	if props::bool(instance, "Enabled", true) {
		census.count("beam: Enabled");
	}
	if attribute(instance, "EmitDuration").is_some() || attribute(instance, "Width_Start").is_some() || attribute(instance, "Transparency_Start").is_some() {
		census.count("beam: Forge tween attributes");
	}
}

fn part(census: &mut Census, instance: &Instance) {
	let material = props::enum_value(instance, "Material", 256);
	let transparency = props::f32(instance, "Transparency", 0.0);
	if transparency >= 1.0 {
		census.count("part: invisible (Transparency 1)");
		return;
	}
	census.count(format!("part: material {}", props::enum_name("Material", material).unwrap_or("?")));
	if transparency > 0.0 {
		census.count("part: half transparent");
	}
}

/// A SurfaceAppearance by its AlphaMode, its part's material (Neon or another) and Transparency (0,
/// up to 0.05, more): what draws by coverage and what blends.
fn surface_appearance(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	// its own glow (EmissiveMaskContent, EmissiveStrength, EmissiveTint: an effect's clock)
	if props::content(instance, &["EmissiveMaskContent", "EmissiveMask"]).is_some() && props::f32(instance, "EmissiveStrength", 1.0) > 0.0 {
		census.count("surface appearance: emissive mask");
	}
	let mode = match props::enum_value(instance, "AlphaMode", 0) {
		0 => "Overlay",
		1 => "Transparency",
		2 => "TintMask",
		_ => "?",
	};
	let Some(part) = dom.get_by_ref(instance.parent()).filter(|parent| props::class_is_a(parent.class.as_str(), "BasePart")) else {
		census.count(format!("surface appearance: {mode}, not on a part"));
		return;
	};
	let neon = if props::enum_value(part, "Material", 256) == 288 { "Neon" } else { "not Neon" };
	let transparency = props::f32(part, "Transparency", 0.0);
	let level = if transparency <= 0.0 { "Transparency 0" } else if transparency <= 0.05 { "Transparency 0-0.05" } else if transparency < 1.0 { "Transparency 0.05-1" } else { "invisible" };
	census.count(format!("surface appearance: {mode}, {neon}, {level}"));
}

/// Counts an instance's features (its class's) into `census`.
fn visit(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	let class = instance.class.as_str();
	let parent_class = dom.get_by_ref(instance.parent()).map(|parent| parent.class.to_string()).unwrap_or_default();
	// a copy: a same-named emitter of one parent after its first (drawn from a stream of its own)
	if class == "ParticleEmitter"
		&& let Some(parent) = dom.get_by_ref(instance.parent())
	{
		let before = parent.children().iter().take_while(|&&child| child != instance.referent()).filter(|&&child| dom.get_by_ref(child).is_some_and(|child| child.class == "ParticleEmitter" && child.name == instance.name)).count();
		census.count(if before > 0 { "emitter: a same-named copy under its parent" } else { "emitter: first of its name under its parent" });
	}
	// a Cylinder's or Sphere's round on a part far from round (its particles leave along the stretched
	// radius), and a Cylinder's ShapePartial
	if class == "ParticleEmitter"
		&& let Some(parent) = dom.get_by_ref(instance.parent())
		&& props::is_a(parent, "BasePart")
	{
		let shape = props::enum_value(instance, "Shape", 0);
		let size = props::vec3(parent, "Size", glam::Vec3::splat(4.0));
		let direction = props::enum_value(instance, "EmissionDirection", 1).min(5);
		let aspect = match shape {
			// across the Cylinder's axis: the part's Y and Z from Top or Bottom, X and Y from Right or
			// Left, Z and Y from Back or Front (SHAPE_FRAMES in render/particles.rs)
			2 => {
				let (a, b) = match direction {
					1 | 4 => (size.y, size.z),
					0 | 3 => (size.x, size.y),
					_ => (size.z, size.y),
				};
				a.max(b) / a.min(b).max(1e-3)
			}
			1 => size.max_element() / size.min_element().max(1e-3),
			_ => 1.0,
		};
		if shape == 1 || shape == 2 {
			let name = if shape == 1 { "Sphere" } else { "Cylinder" };
			let bucket = if aspect < 1.2 { "< 1.2" } else if aspect < 2.0 { "1.2-2" } else if aspect < 5.0 { "2-5" } else { ">= 5" };
			census.count(format!("emitter: {name} round's aspect {bucket}"));
		}
		if shape == 2 {
			let partial = props::f32(instance, "ShapePartial", 1.0);
			let bucket = if partial < 0.0 { "< 0" } else if partial == 0.0 { "0" } else if partial <= 0.5 { "0-0.5" } else if partial < 1.0 { "0.5-1" } else if partial == 1.0 { "1" } else { "> 1" };
			census.count(format!("emitter: Cylinder ShapePartial {bucket}"));
		}
	}
	match class {
		"ParticleEmitter" => {
			particle(census, instance, &parent_class);
			vertical(census, dom, instance);
			// an attachment (or a chain of them) under no part: a Model's or a Folder's (an effect's
			// Skill4Stuff), which ror's attachments do not place
			let mut current = instance.parent();
			while let Some(parent) = dom.get_by_ref(current).filter(|parent| props::class_is_a(parent.class.as_str(), "Attachment")) {
				current = parent.parent();
				if dom.get_by_ref(current).is_some_and(|above| !props::class_is_a(above.class.as_str(), "Attachment") && !props::class_is_a(above.class.as_str(), "BasePart") && above.class != "Terrain") {
					census.count("emitter: on an attachment under no part");
				}
			}
		}
		"Trail" => trail(census, instance),
		"Beam" => beam(census, dom, instance),
		"Decal" => decal(census, dom, instance),
		"Highlight" => highlight(census, dom, instance),
		"SurfaceAppearance" => surface_appearance(census, dom, instance),
		_ if props::class_is_a(class, "BasePart") => part(census, instance),
		_ => {}
	}
	// an Attachment inside another (not a Bone): where its emitters and the trails and beams that
	// use it draw
	if class == "Attachment" && parent_class == "Attachment" {
		census.count("attachment: inside an Attachment");
		let effects = instance.children().iter().filter_map(|&child| dom.get_by_ref(child)).filter(|child| child.class == "ParticleEmitter").count();
		if effects > 0 {
			census.count("attachment: inside an Attachment, with emitters");
			for _ in 0..effects {
				census.count("emitter: on an Attachment inside an Attachment");
			}
		}
	}
	if matches!(class, "Trail" | "Beam") {
		for name in ["Attachment0", "Attachment1"] {
			let parent_of = props::reference(instance, name).and_then(|attachment| dom.get_by_ref(attachment)).and_then(|attachment| dom.get_by_ref(attachment.parent()));
			if parent_of.is_some_and(|parent| parent.class == "Attachment") {
				census.count(format!("{}: {name} inside an Attachment", class.to_lowercase()));
			}
		}
	}
}

/// Highlights: their transparencies (outside 0..1 too), depth mode, on invisible parts.
fn highlight(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	census.count("highlight: total");
	let fill = props::f32(instance, "FillTransparency", 0.5);
	census.count(format!("highlight: FillTransparency {}", if fill < 0.0 { "<0" } else if fill < 1.0 { "0-1" } else { ">=1" }));
	let outline = props::f32(instance, "OutlineTransparency", 0.0);
	census.count(format!("highlight: OutlineTransparency {}", if outline < 0.0 { "<0" } else if outline < 1.0 { "0-1" } else { ">=1" }));
	census.count(format!("highlight: DepthMode {}", props::enum_name("HighlightDepthMode", props::enum_value(instance, "DepthMode", 0)).unwrap_or("?")));
	let adornee = props::reference(instance, "Adornee").and_then(|adornee| dom.get_by_ref(adornee)).or_else(|| dom.get_by_ref(instance.parent()));
	if let Some(adornee) = adornee {
		let mut parts = Vec::new();
		let mut stack = vec![adornee.referent()];
		while let Some(current) = stack.pop() {
			let Some(item) = dom.get_by_ref(current) else {
				continue;
			};
			stack.extend(item.children().iter().copied());
			if props::class_is_a(item.class.as_str(), "BasePart") {
				parts.push((props::f32(item, "Transparency", 0.0), props::enum_name("Material", props::enum_value(item, "Material", 256)).unwrap_or("?")));
			}
		}
		if !parts.is_empty() && parts.iter().all(|&(transparency, _)| transparency >= 1.0) {
			census.count("highlight: every adorned part invisible");
			for (_, material) in &parts {
				census.count(format!("highlight: invisible adorned part of {material}"));
			}
		}
	}
}

/// Decals: on what (a part, a SpecialMesh's mesh type, a MeshPart), which face, over an invisible
/// part, coloured above 1, below Transparency 0.
fn decal(census: &mut Census, dom: &rbx_dom_weak::WeakDom, instance: &Instance) {
	let Some(parent) = dom.get_by_ref(instance.parent()) else {
		return;
	};
	let mesh = parent.children().iter().filter_map(|&child| dom.get_by_ref(child)).find(|child| child.class == "SpecialMesh");
	let on = match mesh {
		Some(mesh) => format!("SpecialMesh {}", props::enum_name("MeshType", props::enum_value(mesh, "MeshType", 0)).unwrap_or("?")),
		None => parent.class.to_string(),
	};
	let face = props::enum_name("NormalId", props::enum_value(instance, "Face", 5)).unwrap_or("?").to_string();
	census.count(format!("decal: on {on}, face {face}"));
	if props::f32(parent, "Transparency", 0.0) >= 1.0 {
		census.count("decal: on an invisible part");
	}
	if let Some(Variant::Color3(color)) = props::get(instance, "Color3")
		&& color.r.max(color.g).max(color.b) > 1.0
	{
		census.count("decal: Color3 above 1");
	}
	if props::f32(instance, "Transparency", 0.0) < 0.0 {
		census.count("decal: Transparency below 0");
	}
}

/// `--effects`: every part or model outside any other (through folders) and what it uses.
fn effects(files: &[String]) {
	for path in files {
		let document = match ror::scene::load::read_file(std::path::Path::new(path)) {
			Ok(document) => document,
			Err(error) => {
				eprintln!("{path}: {error:#}");
				continue;
			}
		};
		let dom = &document.dom;
		let name = std::path::Path::new(path).file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
		let mut roots = Vec::new();
		let mut pending = vec![(dom.root_ref(), String::new())];
		while let Some((referent, prefix)) = pending.pop() {
			for &child in dom.get_by_ref(referent).map(|instance| instance.children()).unwrap_or_default() {
				let Some(instance) = dom.get_by_ref(child) else {
					continue;
				};
				let path = if prefix.is_empty() { instance.name.to_string() } else { format!("{prefix}.{}", instance.name) };
				if props::class_is_a(instance.class.as_str(), "BasePart") || instance.class == "Model" {
					roots.push((child, path));
				} else if matches!(instance.class.as_str(), "Folder" | "Configuration") {
					pending.push((child, path));
				}
			}
		}
		roots.sort_by(|a, b| a.1.cmp(&b.1));
		for (referent, effect) in roots {
			let Some(root) = dom.get_by_ref(referent) else {
				continue;
			};
			let mut census = Census::default();
			let mut stack = vec![referent];
			while let Some(current) = stack.pop() {
				let Some(instance) = dom.get_by_ref(current) else {
					continue;
				};
				stack.extend(instance.children().iter().copied());
				visit(&mut census, dom, instance);
				if matches!(instance.class.as_str(), "PointLight" | "SpotLight" | "SurfaceLight" | "Highlight" | "Decal" | "Texture" | "SpecialMesh" | "MeshPart") {
					census.count(format!("class: {}", instance.class));
				}
			}
			if census.features.is_empty() {
				continue;
			}
			let features: Vec<String> = census.features.iter().map(|(feature, count)| format!("{feature}={count}")).collect();
			println!("{name}\t{effect}\t{}\t{}", root.class, features.join(";"));
		}
	}
}

pub fn main() -> anyhow::Result<()> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	if args.first().is_some_and(|arg| arg == "--effects") {
		effects(&args[1..]);
		return Ok(());
	}
	let mut census = Census::default();
	let mut per_file: Vec<(String, usize, usize, usize, usize)> = Vec::new();
	for path in args {
		let document = match ror::scene::load::read_file(std::path::Path::new(&path)) {
			Ok(document) => document,
			Err(error) => {
				eprintln!("{path}: {error:#}");
				continue;
			}
		};
		let dom = &document.dom;
		let mut seen: BTreeMap<String, usize> = BTreeMap::new();
		let mut stack = vec![dom.root_ref()];
		while let Some(referent) = stack.pop() {
			let Some(instance) = dom.get_by_ref(referent) else {
				continue;
			};
			stack.extend(instance.children().iter().copied());
			*seen.entry(instance.class.to_string()).or_default() += 1;
			visit(&mut census, dom, instance);
		}
		let name = std::path::Path::new(&path).file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default();
		per_file.push((name, *seen.get("ParticleEmitter").unwrap_or(&0), *seen.get("Trail").unwrap_or(&0), *seen.get("Beam").unwrap_or(&0), seen.values().sum()));
		for (class, count) in seen {
			*census.files.entry(class.clone()).or_default() += 1;
			*census.classes.entry(class).or_default() += count;
		}
	}
	let mut classes: Vec<_> = census.classes.iter().collect();
	classes.sort_by(|a, b| b.1.cmp(a.1));
	println!("classes (instances, files):");
	for (class, count) in classes {
		println!("{count:8} {:4}  {class}", census.files[class]);
	}
	println!("\nfeatures:");
	for (feature, count) in &census.features {
		println!("{count:8}  {feature}");
	}
	println!("\nper file (emitters, trails, beams, instances):");
	per_file.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
	for (name, emitters, trails, beams, total) in per_file {
		println!("{emitters:6} {trails:4} {beams:5} {total:7}  {name}");
	}
	Ok(())
}
