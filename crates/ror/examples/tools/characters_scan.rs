// Lists every model with a Humanoid: its rig type, body parts (class, colour, mesh), clothing,
// CharacterMeshes, BodyColors and faces:
//   cargo run -p ror --release --example tools -- characters_scan <file> [--brief]
use ror::scene::props;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let brief = args.any(|arg| arg == "--brief");
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	let mut count = 0;
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else {
			continue;
		};
		stack.extend(instance.children().iter().rev().copied());
		let Some(humanoid) = instance.children().iter().filter_map(|&child| dom.get_by_ref(child)).find(|child| child.class == "Humanoid") else {
			continue;
		};
		count += 1;
		let rig = props::enum_value(humanoid, "RigType", 0);
		let children: Vec<_> = instance.children().iter().filter_map(|&child| dom.get_by_ref(child)).collect();
		let mut summary = Vec::new();
		for child in &children {
			match child.class.as_str() {
				"Shirt" => summary.push(format!("Shirt {:?} {:?}", props::content(child, &["ShirtTemplateContent", "ShirtTemplate"]), props::color(child, "Color3", glam::Vec3::ONE))),
				"Pants" => summary.push(format!("Pants {:?} {:?}", props::content(child, &["PantsTemplateContent", "PantsTemplate"]), props::color(child, "Color3", glam::Vec3::ONE))),
				"ShirtGraphic" => summary.push(format!("ShirtGraphic {:?} {:?}", props::content(child, &["TextureContent", "Graphic"]), props::color(child, "Color3", glam::Vec3::ONE))),
				"CharacterMesh" => summary.push(format!(
					"CharacterMesh part {} mesh {:?} base {:?} overlay {:?}",
					props::enum_value(child, "BodyPart", 0),
					props::content(child, &["MeshContent", "MeshId"]),
					props::content(child, &["BaseTextureContent", "BaseTextureId"]),
					props::content(child, &["OverlayTextureContent", "OverlayTextureId"])
				)),
				"BodyColors" => summary.push(format!(
					"BodyColors head {:?} torso {:?} la {:?} ra {:?} ll {:?} rl {:?}",
					props::color(child, "HeadColor3", glam::Vec3::ZERO) * 255.0,
					props::color(child, "TorsoColor3", glam::Vec3::ZERO) * 255.0,
					props::color(child, "LeftArmColor3", glam::Vec3::ZERO) * 255.0,
					props::color(child, "RightArmColor3", glam::Vec3::ZERO) * 255.0,
					props::color(child, "LeftLegColor3", glam::Vec3::ZERO) * 255.0,
					props::color(child, "RightLegColor3", glam::Vec3::ZERO) * 255.0
				)),
				_ => {}
			}
		}
		// where the torso is and which way the character faces (its front is -Z of the torso)
		let torso = children.iter().find(|child| child.name == "Torso" || child.name == "UpperTorso").and_then(|torso| props::cframe(torso, "CFrame"));
		let facing = torso.map(|torso| format!(" torso {:.2} facing {:.2}", glam::Vec3::from(torso.translation), -glam::Vec3::from(torso.matrix3.z_axis))).unwrap_or_default();
		println!("{} rig {} ({} children){facing} {}", document.path_of(referent), if rig == 1 { "R15" } else { "R6" }, children.len(), summary.join("; "));
		if brief {
			continue;
		}
		for child in &children {
			if !props::class_is_a(child.class.as_str(), "BasePart") {
				continue;
			}
			let mut extras = Vec::new();
			for &grandchild in child.children() {
				let Some(grandchild) = dom.get_by_ref(grandchild) else {
					continue;
				};
				match grandchild.class.as_str() {
					"SpecialMesh" => extras.push(format!("SpecialMesh type {} {:?} tex {:?}", props::enum_value(grandchild, "MeshType", 0), props::content(grandchild, &["MeshContent", "MeshId"]), props::content(grandchild, &["TextureContent", "TextureId"]))),
					"Decal" | "Texture" => extras.push(format!("{} {:?} face {}", grandchild.class, props::content(grandchild, &["TextureContent", "Texture"]), props::enum_value(grandchild, "Face", 5))),
					"SurfaceAppearance" => extras.push("SurfaceAppearance".into()),
					_ => {}
				}
			}
			let color = props::color(child, "Color", glam::Vec3::ZERO) * 255.0;
			let mesh = if child.class == "MeshPart" { format!(" mesh {:?} tex {:?}", props::content(child, &["MeshContent", "MeshId"]), props::content(child, &["TextureContent", "TextureID"])) } else { String::new() };
			println!(
				"  {} {:?} colour ({:.0}, {:.0}, {:.0}) material {} transparency {}{} {}",
				child.class,
				child.name,
				color.x,
				color.y,
				color.z,
				props::enum_value(child, "Material", 256),
				props::f32(child, "Transparency", 0.0),
				mesh,
				extras.join("; ")
			);
		}
	}
	println!("{count} models with a Humanoid");
	Ok(())
}
