// The sun's shadow, shared by the part and GUI passes: Studio's cascaded exponential variance shadow
// maps (notes/render.md "Shadows"). Each view's cascades are spheres along its axis; their blurred
// warped moments (shadow_blur.wgsl) sit side by side in one atlas. And the light grid's reads (notes/render.md
// "Light grid"): the sky's visibility and the sun past the shadow maps.

@group(0) @binding(2) var sun_moments: texture_2d<f32>;
@group(0) @binding(3) var shadow_sampler: sampler_comparison;
// the light grid, one texel per 4-stud voxel (render::lightgrid): the sky's visibility in r, the sun's light through
// it in g (needs `linear_sampler` declared)
@group(0) @binding(15) var light_grid: texture_3d<f32>;

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
// mean square); the variance is at least `least` of the depth squared (Studio's leak near occluders,
// shadow-gap; the cascade's own: render::SunCascade::least_variance)
fn chebyshev(moments: vec2<f32>, depth: f32, least: f32) -> f32 {
	if (depth <= moments.x) {
		return 1.0;
	}
	let variance = max(moments.y - moments.x * moments.x, least * depth * depth);
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
	let region = frame.shadow_regions[cascade];
	// nothing further from the camera than the shadows' reach (render::SunShadowQuality::reach)
	if (distance(world_position, frame.camera_position.xyz) > region.z) {
		return 1.0;
	}
	let sphere = frame.shadow_spheres[cascade];
	let clip = frame.shadow_cascades[cascade] * vec4<f32>(world_position, 1.0);
	let uv = vec2<f32>(clip.x * 0.5 + 0.5, 0.5 - clip.y * 0.5);
	let moments = sun_moments_at(region, uv);
	// the receiver's depth as the moments warp it: -1 at the sphere's sunward side, +1 at the far one
	let z = clamp(-dot(world_position - sphere.xyz, frame.sun_direction.xyz) / sphere.w, -1.0, 1.0);
	let exponent = frame.shadow.y;
	let lit = min(chebyshev(moments.xy, exp(exponent * z), region.w), chebyshev(moments.zw, -exp(-exponent * z), region.w));
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

// A Roblox client lights a face turned away from the sun by SUN_BACK_LIGHT · |n·l| of the sun's diffuse light where it does
// not take the sun as round the focus: a GUI everywhere (gui.wgsl `gui_light`; fixtures/world-guis.luau "light": a slab
// turned 120 degrees from the sun 0.40 / 0.41, a billboard seen against it 0.42), a part past its graphics quality level's
// radius of the focus (mesh.wgsl `back_sun`; fixtures/quality-levels.luau `backs` under the sun alone: plates turned to
// n·l -0.25, -0.5, -0.75 and -0.95 read 0.664, 0.657, 0.650 and 0.656 of tone-curve input per unit |n·l|; `bounce` without
// GlobalShadows, a wall 0.404 and balls 0.398 of the sun's diffuse light)
const SUN_BACK_LIGHT: f32 = 0.41;

// The share of the sun's diffuse light a face at `n_dot_l` turned away from it takes (SUN_BACK_LIGHT · |n·l|), giving way
// to the sky's diffuse light as EnvironmentDiffuseScale e rises: times 1 - e (fixtures/quality-levels.luau `kinds` at level
// 4 under the sun: the shapes' far sides keep 0.92, 0.56 and none of it at e 0.1, 0.5 and 1 beside e times the sky's
// light; `guiback`: a white frame on a slab's face turned from the sun alike, at levels 4 and 21); 0 facing the sun
fn sun_back_share(n_dot_l: f32) -> f32 {
	return SUN_BACK_LIGHT * (1.0 - saturate(frame.environment.x)) * max(-n_dot_l, 0.0);
}

// what the sky's diffuse light falls to where the grid is closed: a fixed warm grey whatever the sky
// (lg-sky: on a white floor under a 48-stud roof the same for skies dark, white, default, white in
// the Up or the Dn image only and at midnight, half at EnvironmentDiffuseScale 0.5)
const LIGHT_GRID_SKY_CLOSED: vec3<f32> = vec3<f32>(0.104, 0.100, 0.092);
// how far along the normal a surface reads the grid (lg-height, lg-walls: 6 studs)
const LIGHT_GRID_OFFSET: f32 = 6.0;

// the light grid at a surface: trilinear between voxel centres, 6 studs along the normal; 1 without
// a grid
fn light_grid_visibility(position: vec3<f32>, normal: vec3<f32>) -> f32 {
	if (frame.light_grid.w < 0.5) {
		return 1.0;
	}
	let uvw = (position + normal * LIGHT_GRID_OFFSET - frame.light_grid.xyz) * frame.light_grid_scale.xyz;
	return textureSampleLevel(light_grid, linear_sampler, uvw, 0.0).r;
}

// the sun's light through the grid (frame.light_grid.w 2; render::lightgrid propagate_sun), trilinear between voxel
// centres 6 studs along the normal, as the sky's (fixtures/quality-levels.luau `sunfield`: a block one layer tall on a
// floor leaves the floor open, thin plates on it their own tops lit, a block two layers tall shades the floor beside
// it; faces turned from the sun read 6 studs out, the bounce walls' backs 35 / 65 / 103 of 255 1 / 4 / 7 studs up); 1
// without a grid or its sun channel
fn light_grid_sun(position: vec3<f32>, normal: vec3<f32>) -> f32 {
	if (frame.light_grid.w < 1.5) {
		return 1.0;
	}
	let point = position + normal * LIGHT_GRID_OFFSET;
	let uvw = (point - frame.light_grid.xyz) * frame.light_grid_scale.xyz;
	return textureSampleLevel(light_grid, linear_sampler, uvw, 0.0).g;
}

// what of the sun reaches a surface: the shadow maps within the view's graphics quality's reach of the camera,
// the grid's voxel sun past it, the one fading into the other between frame.shadow.x and frame.shadow.z studs
// from the camera (crate::quality::SunReach; both 0 at a level without the maps: the voxels everywhere). The
// client's rails 6 studs up cast their strips to 587.5 studs at level 21 and none past, fading out over the last
// ~14 studs below 19; past them its floors take the voxel sun at every level (notes/render.md "Graphics quality
// levels")
fn sun_shade(position: vec3<f32>, normal: vec3<f32>) -> f32 {
	let from_camera = distance(position, frame.camera_position.xyz);
	if (from_camera <= frame.shadow.x) {
		return shadow_factor(position, normal);
	}
	let voxels = light_grid_sun(position, normal);
	if (from_camera >= frame.shadow.z) {
		return voxels;
	}
	return mix(shadow_factor(position, normal), voxels, (from_camera - frame.shadow.x) / (frame.shadow.z - frame.shadow.x));
}
