// Per-part ids (render::ids): every instance writes the id a storage buffer gives it into an
// integer target: its Highlight's number (render::highlights) or its part's index + 1 (part ids).

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(1) @binding(0) var<storage, read> ids: array<u32>;

struct IdInput {
	@location(0) position: vec3<f32>,
}

struct IdOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) @interpolate(flat) id: u32,
}

@vertex
fn vs_ids(input: IdInput, @builtin(instance_index) instance_index: u32) -> IdOutput {
	let instance = instances[instance_index];
	var output: IdOutput;
	output.clip = frame.view_proj * instance.model * vec4<f32>(input.position, 1.0);
	output.id = ids[instance_index];
	return output;
}

@fragment
fn fs_ids(input: IdOutput) -> @location(0) u32 {
	return input.id;
}
