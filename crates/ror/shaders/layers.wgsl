// See-through layers (render::ids::encode_layers): every fragment of a see-through part or a decal
// that lies in front of the frontmost opaque surface, counted per pixel and per part. The test against
// the opaque depth is made here, not by the depth unit: a shader with side effects may run before the
// depth test, so the hardware test would let hidden fragments count.

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
// per instance: its part's index + 1 (decals their part's)
@group(1) @binding(0) var<storage, read> owners: array<u32>;
@group(1) @binding(1) var<storage, read_write> part_fragments: array<atomic<u32>>;
@group(1) @binding(2) var<storage, read_write> pixel_layers: array<atomic<u32>>;
@group(1) @binding(3) var opaque_depth: texture_depth_2d;

struct LayerOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) @interpolate(flat) owner: u32,
}

@vertex
fn vs_layers(@location(0) position: vec3<f32>, @builtin(instance_index) instance_index: u32) -> LayerOutput {
	var output: LayerOutput;
	output.clip = frame.view_proj * instances[instance_index].model * vec4<f32>(position, 1.0);
	output.owner = owners[instance_index];
	return output;
}

// A decal lies on its part's face, at the opaque depth: depths within this share of it count as in
// front (reversed infinite Z: depth = near / distance, so the share is the same at every distance).
const SLACK: f32 = 1e-4;

@fragment
fn fs_layers(input: LayerOutput) {
	let pixel = vec2<u32>(input.clip.xy);
	let opaque = textureLoad(opaque_depth, vec2<i32>(pixel), 0);
	// reversed Z: nearer is greater
	if input.owner == 0u || input.clip.z < opaque * (1.0 - SLACK) {
		return;
	}
	let width = textureDimensions(opaque_depth).x;
	atomicAdd(&part_fragments[input.owner - 1u], 1u);
	atomicAdd(&pixel_layers[pixel.y * width + pixel.x], 1u);
}
