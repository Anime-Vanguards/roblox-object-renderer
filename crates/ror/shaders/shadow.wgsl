// The sun's shadow, shared by the part and GUI passes: Studio's cascaded exponential variance shadow
// maps (notes/render.md "Shadows"). Each view's cascades are spheres along its axis; their blurred
// warped moments (shadow_blur.wgsl) sit side by side in one atlas.

@group(0) @binding(2) var sun_moments: texture_2d<f32>;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;

// A cascade's blurred moments at `uv` (0..1 over its square), bilinear between its texels
fn sun_moments_at(region: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
	let side = region.y;
	let position = uv * side - 0.5;
	let base = floor(position);
	let f = position - base;
	let last = i32(side) - 1;
	let column = i32(region.x);
	let x0 = clamp(i32(base.x), 0, last) + column;
	let x1 = clamp(i32(base.x) + 1, 0, last) + column;
	let y0 = clamp(i32(base.y), 0, last);
	let y1 = clamp(i32(base.y) + 1, 0, last);
	let top = mix(textureLoad(sun_moments, vec2<i32>(x0, y0), 0), textureLoad(sun_moments, vec2<i32>(x1, y0), 0), f.x);
	let bottom = mix(textureLoad(sun_moments, vec2<i32>(x0, y1), 0), textureLoad(sun_moments, vec2<i32>(x1, y1), 0), f.x);
	return mix(top, bottom, f.y);
}

// Chebyshev's bound on the lit share of a receiver at warped depth `depth` behind the moments (mean,
// mean square); the variance is at least frame.shadow.z of the depth squared (Studio's leak near
// occluders, shadow-gap)
fn chebyshev(moments: vec2<f32>, depth: f32) -> f32 {
	if (depth <= moments.x) {
		return 1.0;
	}
	let variance = max(moments.y - moments.x * moments.x, frame.shadow.z * depth * depth);
	let d = depth - moments.x;
	return variance / (variance + d * d);
}

fn shadow_factor(world_position: vec3<f32>, normal: vec3<f32>) -> f32 {
	if (frame.sun_direction.w < 0.5) {
		return 1.0;
	}
	// a surface turned away from the sun is in its own shadow (R15's torso front under the default
	// sky, where its normal map faces the sun)
	if (dot(normal, frame.sun_direction.xyz) <= 0.0) {
		return 0.0;
	}
	// the first cascade whose sphere holds the point; no shadow beyond the last
	var cascade = -1;
	for (var index = 0; index < 4; index++) {
		let candidate = frame.shadow_spheres[index];
		if (candidate.w > 0.0 && distance(world_position, candidate.xyz) < candidate.w) {
			cascade = index;
			break;
		}
	}
	if (cascade < 0) {
		return 1.0;
	}
	let sphere = frame.shadow_spheres[cascade];
	let clip = frame.shadow_cascades[cascade] * vec4<f32>(world_position, 1.0);
	let uv = vec2<f32>(clip.x * 0.5 + 0.5, 0.5 - clip.y * 0.5);
	let moments = sun_moments_at(frame.shadow_regions[cascade], uv);
	// the receiver's depth as the moments warp it: -1 at the sphere's sunward side, +1 at the far one
	let z = clamp(-dot(world_position - sphere.xyz, frame.sun_direction.xyz) / sphere.w, -1.0, 1.0);
	let exponent = frame.shadow.y;
	let lit = min(chebyshev(moments.xy, exp(exponent * z)), chebyshev(moments.zw, -exp(-exponent * z)));
	// Studio darkens what the bound lets through (its leak reduction): lit shares up to frame.shadow.w
	// go black, the rest rescaled
	return clamp((lit - frame.shadow.w) / (1.0 - frame.shadow.w), 0.0, 1.0);
}

// The share of the sun's light Terrain Clouds leave at `world_position`: their dimming holds within
// 585 studs of the camera across (either way) and fades out by 700, as Studio's light grid carries it
// (notes/render.md "Clouds")
fn cloud_sun(world_position: vec3<f32>) -> f32 {
	let across = max(abs(world_position.x - frame.cloud_sun.y), abs(world_position.z - frame.cloud_sun.z));
	return 1.0 - frame.cloud_sun.x * (1.0 - saturate((across - 585.0) / 115.0));
}
