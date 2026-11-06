// The local lights' voxels baked for a view (render::voxel_field, notes/render.md "Local lights"):
// each voxel of the field holds Σ over the lights of voxel_color · its 27 neighbours' fade / 27, the
// blurred voxel `voxel_light` reads four of along each axis. Read trilinearly between two of them
// (voxel_read.wgsl) it gives what `voxel_light`'s 64 voxels give, in one filtered sample a pixel
// whatever the lights.

struct FieldInfo {
	// the first voxel's index (voxel i's centre at 4i + 2 studs)
	origin: vec4<i32>,
	// voxels along x, y and z, w the lights
	size: vec4<u32>,
}

@group(0) @binding(0) var<uniform> info: FieldInfo;
@group(0) @binding(1) var<storage, read> lights: array<Light>;
@group(0) @binding(2) var field: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(64)
fn bake(@builtin(global_invocation_id) id: vec3<u32>) {
	let index = id.x;
	let size = info.size.xyz;
	if (index >= size.x * size.y * size.z) {
		return;
	}
	let voxel = info.origin.xyz + vec3<i32>(vec3<u32>(index % size.x, (index / size.x) % size.y, index / (size.x * size.y)));
	let centre = vec3<f32>(voxel) * 4.0 + 2.0;
	var light_sum = vec3<f32>(0.0);
	for (var l = 0u; l < info.size.w; l++) {
		let light = lights[l];
		let kind = u32(light.position.w);
		var nearest = light.position.xyz;
		if (kind == 2u) {
			let offset = centre - light.position.xyz;
			nearest += light.right.xyz * clamp(dot(offset, light.right.xyz), -light.right.w, light.right.w);
			nearest += light.up.xyz * clamp(dot(offset, light.up.xyz), -light.up.w, light.up.w);
		}
		// past its reach (1.007 R + 8√3) every neighbour's centre is past 1.007 R: none holds any
		if (distance(centre, nearest) >= light.voxel_color.w) {
			continue;
		}
		var sum = 0.0;
		for (var k = -1; k <= 1; k++) {
			for (var j = -1; j <= 1; j++) {
				for (var i = -1; i <= 1; i++) {
					sum += voxel_inject(light, kind, centre + vec3<f32>(f32(i), f32(j), f32(k)) * 4.0);
				}
			}
		}
		light_sum += light.voxel_color.rgb * (sum / 27.0);
	}
	textureStore(field, voxel - info.origin.xyz, vec4<f32>(light_sum, 0.0));
}
