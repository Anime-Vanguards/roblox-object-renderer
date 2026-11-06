// Particles (notes/render.md "Particles"): CPU-built quads, textured (with flipbook frames
// cross-faded), tinted, emitting Color² · Brightness, blended over the √-space scene target (alpha
// or additive by LightEmission), like beams.

@group(0) @binding(0) var<uniform> frame: Frame;
// the sky chain the Atmosphere's fog fades into
@group(0) @binding(19) var faded_map: texture_cube<f32>;
@group(0) @binding(5) var linear_sampler: sampler;
// the view's local lights (their voxels light particles, render::voxel_lights)
@group(0) @binding(7) var<storage, read> lights: array<Light>;
// the opaque scene's depth, single-sampled (soft particles)
@group(0) @binding(8) var scene_depth: texture_depth_2d;

@group(1) @binding(0) var particle_texture: texture_2d<f32>;
@group(1) @binding(1) var particle_sampler: sampler;

struct ParticleInput {
	@location(0) position: vec3<f32>,
	// this corner in the current flipbook frame (xy) and the next (zw)
	@location(1) uv: vec4<f32>,
	@location(2) color: vec4<f32>,
	// towards the camera (xyz) and the depth the particle fades over (w)
	@location(3) normal: vec4<f32>,
	@location(4) params: vec4<f32>,
	// x the most light emitted per channel, y 1 for a texture of premultiplied colour, z and w the width
	// and threshold of the texel alpha that shows (z 0: the alpha as is)
	@location(5) extra: vec4<f32>,
}

struct ParticleOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) uv: vec4<f32>,
	@location(1) color: vec4<f32>,
	@location(2) normal: vec4<f32>,
	@location(3) @interpolate(flat) params: vec4<f32>,
	@location(4) world_position: vec3<f32>,
	@location(5) @interpolate(flat) extra: vec4<f32>,
	// the light that is the same over the whole particle (`particle_lit`)
	@location(6) @interpolate(flat) lit: vec3<f32>,
}

@vertex
fn vs_particle(input: ParticleInput) -> ParticleOutput {
	var output: ParticleOutput;
	output.clip = frame.view_proj * vec4<f32>(input.position, 1.0);
	output.uv = input.uv;
	output.color = input.color;
	output.normal = input.normal;
	output.params = input.params;
	output.world_position = input.position;
	output.extra = input.extra;
	output.lit = particle_lit(input.normal.xyz);
	return output;
}

// Lit like beams: the sun at full strength, 0.92 of the parts' flat ambient (particle-basic: lit-flat 126
// against parts' 129) and the sky's irradiance by the particle's normal, the same at its four corners:
// per vertex, not per pixel (an aura at 1650 px: its particles 2.67 -> 2.25 ms a view). The
// corners' own normal, where a pixel's interpolated one could differ in its last bit: of 94 match-map
// views 3 had a pixel 1 / 255 off, the light-particle and particle calibrations none
fn particle_lit(normal: vec3<f32>) -> vec3<f32> {
	let n = normalize(normal);
	return frame.sun_color.rgb / PI + max(frame.ambient.rgb, frame.outdoor_ambient.rgb) * 0.92 + max(sh_irradiance(frame, n), vec3<f32>(0.0)) / PI * frame.environment.x;
}

// A particle at this pixel: its emitted radiance, its opacity (the particle's times its texel's) and
// the particle's own (its fade over life and the soft fade, without the texel's).
struct ParticleLight {
	radiance: vec3<f32>,
	alpha: f32,
	opacity: f32,
}

// Whether the draw's flipbook cross-fades its frames (render::particles::ParticleDraw::cross_fades): a
// draw whose particles all show one frame reads only it, which is what the cross-fade gives at 0 (Ant
// King's particles 2.25 -> 2.19 ms a view)
override CROSS_FADE: bool = true;

// The particle's texel at this pixel (the flipbook's two frames cross-faded)
fn particle_texel(input: ParticleOutput) -> vec4<f32> {
	if (CROSS_FADE) {
		return mix(textureSample(particle_texture, particle_sampler, input.uv.xy), textureSample(particle_texture, particle_sampler, input.uv.zw), input.params.w);
	}
	return textureSample(particle_texture, particle_sampler, input.uv.xy);
}

// How much of the pixel the texel covers: its alpha, or a legacy effect's above a threshold of it over a
// width (`extra.w`, `extra.z`)
fn particle_coverage(input: ParticleOutput, texel: vec4<f32>) -> f32 {
	return select(texel.a, saturate((texel.a - input.extra.w) / max(input.extra.z, 1e-4)), input.extra.z > 0.0);
}

fn particle_light(input: ParticleOutput, texel: vec4<f32>) -> ParticleLight {
	// colours decode with a square, like part colours and decal textures; a legacy effect's texture
	// holds its colour premultiplied by its alpha (`extra.y`)
	let tint = select(texel.rgb, texel.rgb / max(texel.a, 1e-3), input.extra.y > 0.5);
	let albedo = input.color.rgb * input.color.rgb * tint * tint;
	// soft: opacity fades in over the particle's Size in front of the surface behind it, by view depth
	// (particle-soft: 0.13 / 0.33 / 0.68 / 1 at 0.1 / 0.25 / 0.5 / 0.75 studs for Size 0.75;
	// particle-soft-size: 0.25 and 0.5 at 0.5 and 1 for Size 2, 0.40 and 0.82 at 0.1 and 0.2 for 0.25)
	let behind = textureLoad(scene_depth, vec2<i32>(input.clip.xy), 0);
	let near = frame.camera_position.w;
	let gap = near / max(behind, 1e-9) - near / max(input.clip.z, 1e-9);
	// by the distance to that surface either side of it: a particle behind a SurfaceAppearance in
	// AlphaMode Transparency shows in the samples it leaves open, faded by how far behind it is
	// (particle-sa-coverage: 0.5, 1 and 2 studs behind a quad covering 2-3 of the 4 samples, the panel
	// 2.5, 2 and 1 behind them, it shows 0.18, 0.48 and 2.2 times as much as faded by the panel)
	let soft = saturate(abs(gap) / max(input.normal.w, 1e-4));
	let alpha = saturate(input.color.a * particle_coverage(input, texel)) * soft;
	let opacity = saturate(input.color.a) * soft;
	// lit like beams (`particle_lit`), and by the local lights' voxels read 6 studs towards the camera
	// (light-particle-field)
	var lit = input.lit;
	let read_at = input.world_position + normalize(frame.camera_position.xyz - input.world_position) * 6.0;
	if (input.params.y > 0.0 && distance(read_at, frame.camera_position.xyz) < VOXEL_LIGHT_REACH) {
		lit += voxel_lights(read_at) * VOXEL_LIGHT_PARTICLES;
	}
	let weight = sqrt(input.params.y);
	// capped per channel like transparent Neon once the texture's alpha is in, before Transparency: √x
	// times the texel's alpha at most √7.84 (particle-bright: faint red squares stop brightening at
	// Brightness 4, x = 7.74, alpha and additive alike; particle-sheets: an effect's soft blue sheets at
	// Brightness 10-25 keep their hue and read 0.68-0.77 of Studio's light under a plain cap, 0.90-1.02
	// with this one, 46-73 % of their pixels within 8 against 9-35 %); a legacy effect's cap is beyond
	// any (its emitter's `extra.x`). The lit share alone passes it: particles near a bright light
	// (light-particle-voxels b10: LightInfluence 1 at 0.96 of 19.8 shows 253 at -2 EV where the cap
	// gave 223), while the mix stays capped (vfx-lit: orange at Brightness 15, LightInfluence 0.25-0.75
	// in daylight)
	let cap = input.extra.x / max(texel.a * texel.a, 1e-4);
	let lit_share = albedo * lit * weight;
	let radiance = max(min(albedo * input.params.x * (1.0 - weight) + lit_share, vec3<f32>(cap)), lit_share);
	return ParticleLight(radiance, alpha, opacity);
}

// Where the texel covers nothing the particle's alpha is 0 and both its outputs are exact zeros, which
// blending (One / OneMinusSrcAlpha, the legacy layer's Zero / OneMinusSrcAlpha) leaves the target as it
// was: such pixels return them before the light, the soft depth and the fog (an aura at 1650
// px, its textures' clear corners: its particles 3.28 -> 2.67 ms a view, a map's at 4K 1.45 ->
// 1.17)
@fragment
fn fs_particle(input: ParticleOutput) -> @location(0) vec4<f32> {
	let texel = particle_texel(input);
	if (particle_coverage(input, texel) == 0.0) {
		return vec4<f32>(0.0);
	}
	let light = particle_light(input, texel);
	// fog fades what it shows and covers alike (fog_kept)
	let alpha = light.alpha * fog_kept(frame, input.world_position);
	// the background keeps 1 - min(1, opacity · (1 - LightEmission)): negative LightEmission makes a
	// particle more opaque, never darker than its own colour (particle-basic: negative-half is 129
	// over black and white)
	return vec4<f32>(target_steps(sqrt(max(light.radiance, vec3<f32>(0.0))) * alpha), saturate(alpha * (1.0 - input.params.z)));
}

// What one particle adds lands on Studio's target in whole steps: its scene target keeps √(x / 7.74)
// in 10 bits (particle-quantize: stacks of 50 faint additive squares add their light in multiples of
// √7.74 / 1023, each particle's rounded to the nearest, at Brightness 0.25-4 alike; the 7.74 is the
// cap particle-bright found). ror's target is float: the particle's own light is rounded so.
const TARGET_STEP: f32 = 2.7821 / 1023.0;

fn target_steps(light: vec3<f32>) -> vec3<f32> {
	return round(light / TARGET_STEP) * TARGET_STEP;
}

// Legacy effects (notes/render.md "Legacy effects") blend in linear light: premultiplied radiance over
// a layer of their own (rgb their light, a the transmittance of what is behind), composited over the
// scene (shaders/legacy.wgsl). They take the surfaces' fog, additive light fading (not measured).
@fragment
fn fs_particle_linear(input: ParticleOutput) -> @location(0) vec4<f32> {
	let texel = particle_texel(input);
	if (particle_coverage(input, texel) == 0.0) {
		return vec4<f32>(0.0);
	}
	let light = particle_light(input, texel);
	var radiance = light.radiance;
	if (frame.atmosphere.w > 0.0) {
		radiance = atmosphere_fog(frame, faded_map, linear_sampler, input.world_position, radiance, saturate(input.params.z));
	} else if (frame.fog.z > 0.5) {
		radiance = linear_fog(frame, input.world_position, radiance, saturate(input.params.z));
	}
	return vec4<f32>(max(radiance, vec3<f32>(0.0)) * light.alpha, saturate(light.alpha * (1.0 - input.params.z)));
}
