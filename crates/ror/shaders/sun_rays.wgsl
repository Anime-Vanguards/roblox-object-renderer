// SunRaysEffect (notes/render.md "Post effects"; render::post): what the rays add to the scene's
// radiance, drawn at a quarter of the resolution and read back bilinearly by the resolve. In open
// sky the rays follow the angle from the sun alone (post-sunrays-time: the same at 25, 50 and 90
// degree views), their profile set by Spread; what covers the path from the pixel towards the sun
// shades them, the half of the path nearer the sun weighing most (post-sunrays: the shadow behind a
// slab over the sun; post-sunrays-sky: the wall below it).

struct SunRays {
	// the camera's rays: its rotation and projection inverted, the eye at the origin
	// (render::camera::Camera::ray_unprojection)
	rays: mat4x4<f32>,
	// xyz towards the sun
	sun: vec4<f32>,
	// xy the sun's place on the view (uv, may lie outside 0..1), z the fade for a sun off the view
	screen: vec4<f32>,
	// rgb Intensity times the rays' colour
	color: vec4<f32>,
	// the open-sky profile: its angles (degrees) and values, 22 of each
	angles: array<vec4<f32>, 6>,
	profile: array<vec4<f32>, 6>,
}
@group(0) @binding(1) var<uniform> rays: SunRays;
@group(0) @binding(5) var scene_depth: texture_depth_2d;

const SUN_RAYS_SAMPLES: i32 = 32;
// a sample's weight along the path, f from the pixel (0) to the sun (1): (f / 0.33)^1.72 below 0.33,
// 1 beyond (fitted to both shadows within 0.06 of the open sky's rays)
const SUN_RAYS_RAMP: f32 = 0.33;
const SUN_RAYS_RAMP_POWER: f32 = 1.72;

fn profile_angle(index: u32) -> f32 {
	return rays.angles[index / 4u][index % 4u];
}

fn profile_value(index: u32) -> f32 {
	return rays.profile[index / 4u][index % 4u];
}

// the profile between its angles, geometric (it falls about exponentially)
fn open_sky(angle: f32) -> f32 {
	for (var index = 1u; index < 22u; index++) {
		let next = profile_angle(index);
		if (angle <= next) {
			let previous = profile_angle(index - 1u);
			let t = (angle - previous) / max(next - previous, 1e-4);
			return exp(mix(log(max(profile_value(index - 1u), 1e-8)), log(max(profile_value(index), 1e-8)), t));
		}
	}
	return profile_value(21u);
}

@fragment
fn fs_sun_rays(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let point = rays.rays * vec4<f32>(input.ndc, 0.5, 1.0);
	let direction = normalize(point.xyz / point.w);
	let angle = degrees(acos(clamp(dot(direction, rays.sun.xyz), -1.0, 1.0)));
	// the share of the path to the sun the opaque parts leave open; past the view's edge the edge
	// stands in (post-sunrays-sky beside: a wall running off the view towards the sun stays dark)
	let uv = vec2<f32>(input.ndc.x * 0.5 + 0.5, 0.5 - input.ndc.y * 0.5);
	let size = vec2<i32>(textureDimensions(scene_depth));
	var total = 0.0;
	var open = 0.0;
	for (var i = 0; i < SUN_RAYS_SAMPLES; i++) {
		let f = (f32(i) + 0.5) / f32(SUN_RAYS_SAMPLES);
		let weight = select(1.0, pow(f / SUN_RAYS_RAMP, SUN_RAYS_RAMP_POWER), f < SUN_RAYS_RAMP);
		let along = clamp(vec2<i32>(mix(uv, rays.screen.xy, f) * vec2<f32>(size)), vec2<i32>(0), size - 1);
		// reversed Z: nothing drawn reads 0
		let sky = select(0.0, 1.0, textureLoad(scene_depth, along, 0) <= 0.0);
		total += weight;
		open += weight * sky;
	}
	let shade = open / max(total, 1e-6);
	return vec4<f32>(rays.color.rgb * open_sky(angle) * shade * rays.screen.z, 1.0);
}
