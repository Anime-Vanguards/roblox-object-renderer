// Beams (notes/render.md "Beams"): CPU-built ribbons, textured and tinted along their length,
// emitting Color² · Brightness, blended over the √-space scene target (alpha or additive by
// LightEmission).

@group(0) @binding(0) var<uniform> frame: Frame;
// the view's local lights (their voxels light beams and trails, render::voxel_lights)
@group(0) @binding(7) var<storage, read> lights: array<Light>;
// the sky's specular chain (the Atmosphere's fog)
@group(0) @binding(4) var environment_map: texture_cube<f32>;
@group(0) @binding(5) var linear_sampler: sampler;

@group(1) @binding(0) var beam_texture: texture_2d<f32>;
@group(1) @binding(1) var beam_sampler: sampler;

struct BeamInput {
	@location(0) position: vec3<f32>,
	// (u along, v across) · q and q
	@location(1) uvq: vec3<f32>,
	@location(2) color: vec4<f32>,
	@location(3) normal: vec3<f32>,
	@location(4) params: vec4<f32>,
}

struct BeamOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) uvq: vec3<f32>,
	@location(1) color: vec4<f32>,
	@location(2) normal: vec3<f32>,
	@location(3) @interpolate(flat) params: vec4<f32>,
	@location(4) world_position: vec3<f32>,
}

@vertex
fn vs_beam(input: BeamInput) -> BeamOutput {
	var output: BeamOutput;
	output.clip = frame.view_proj * vec4<f32>(input.position, 1.0);
	output.uvq = input.uvq;
	output.color = input.color;
	output.normal = input.normal;
	output.params = input.params;
	output.world_position = input.position;
	return output;
}

@fragment
fn fs_beam(input: BeamOutput) -> @location(0) vec4<f32> {
	// projective over each segment's trapezoid (v is the fraction across the local width); the image
	// runs down along the beam from Attachment0 and across from the side v = 0 (beam-texture: arrow,
	// triangle; a character's ring follows the vertical band in its image)
	let uv = input.uvq.xy / input.uvq.z;
	let texel = textureSample(beam_texture, beam_sampler, uv.yx);
	// colours decode with a square, like part colours and decal textures
	let albedo = input.color.rgb * input.color.rgb * texel.rgb * texel.rgb;
	let alpha = saturate(input.color.a * texel.a);
	// lit: the sun at full strength whichever way the ribbon faces, 0.96 of the parts' flat ambient
	// and the sky's irradiance (beam-color: lit-sun shows Brightness 2 for any facing; beam-brightness:
	// LightInfluence 1 under Ambient 0.5 reads 0.476, parts 0.495)
	let n = normalize(input.normal);
	let sky = (max(sh_irradiance(frame, n), vec3<f32>(0.0)) + max(sh_irradiance(frame, -n), vec3<f32>(0.0))) * 0.5;
	var lit = frame.sun_color.rgb / PI + max(frame.ambient.rgb, frame.outdoor_ambient.rgb) * 0.96 + sky / PI * frame.environment.x;
	// and the local lights' voxels where the ribbon is (light-particle-field beams)
	if (input.params.y > 0.0 && distance(input.world_position, frame.camera_position.xyz) < VOXEL_LIGHT_REACH) {
		lit += voxel_lights(input.world_position) * VOXEL_LIGHT_PARTICLES;
	}
	// LightInfluence mixes the emission (Color² · Brightness · 1.935) and the lit colour by its square
	// root (beam-brightness: LightInfluence 0.25 / 0.5 / 0.75 weigh the lit colour 0.50 / 0.70 / 0.87)
	let weight = sqrt(input.params.y);
	var radiance = albedo * (vec3<f32>(input.params.x) * (1.0 - weight) + lit * weight);
	// at most EXPOSED_CAP once exposed, as every surface before it blends (bloom-colors: saturated
	// beams of x 12-23 at EV +1 and +2 haloed 3-5 % over Studio 2-6 px out uncapped, their
	// anti-aliased edges over the threshold, and within 1-2 % of Studio's Neon tiles capped)
	radiance = min(radiance, vec3<f32>(EXPOSED_CAP / frame.environment.z));
	// fog fades what a beam or trail shows and covers alike (common.wgsl fog_kept)
	let shown = alpha * fog_kept(frame, input.world_position);
	// the background keeps 1 - min(1, opacity · (1 - LightEmission)), as particles show
	return vec4<f32>(sqrt(max(radiance, vec3<f32>(0.0))) * shown, saturate(shown * (1.0 - input.params.z)));
}
