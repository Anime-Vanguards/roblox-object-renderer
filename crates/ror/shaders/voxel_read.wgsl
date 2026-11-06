// The local lights' voxels at a point (particles, beams and trails): from the view's baked field
// (voxel_field.wgsl) where it holds the point, else each light's 64 voxels (`voxel_light`). Needs the
// frame and its lights declared.

@group(0) @binding(18) var voxel_field: texture_3d<f32>;

fn voxel_lights(position: vec3<f32>) -> vec3<f32> {
	let grid = (position - 2.0) * 0.25;
	let base = floor(grid);
	// the two voxels either side along each axis, in the field: the sampler's trilinear filter between
	// their centres weighs them as `voxel_light` does
	let at = vec3<i32>(base - frame.voxel_field.xyz);
	let size = frame.voxel_field_size.xyz;
	if (frame.voxel_field.w > 0.0 && all(at >= vec3<i32>(0)) && all(at + 1 < vec3<i32>(size))) {
		return textureSampleLevel(voxel_field, linear_sampler, (grid - frame.voxel_field.xyz + 0.5) / size, 0.0).rgb;
	}
	var lit = vec3<f32>(0.0);
	for (var i = 0u; i < frame.counts.x; i++) {
		lit += voxel_light(lights[i], position);
	}
	return lit;
}
