// Local lights per pixel, shared by the part and GUI passes (notes/render.md "Local lights"): how far a light
// reaches, its cone, its shadow and its grazing darkening, the view's light clusters, and what one light brings to a
// surface point before the surface's own shading (mesh.wgsl `direct_light`, gui.wgsl's Lambert). Needs the frame,
// `lights` and shadow.wgsl's `shadow_sampler` declared.

// local lights with Shadows: six depth faces each (+X, -X, +Y, -Y, +Z, -Z) and their view-projections
@group(0) @binding(13) var light_shadow_maps: texture_depth_2d_array;
@group(0) @binding(14) var<storage, read> light_shadow_matrices: array<mat4x4<f32>>;
// the view's lights by cluster (render::light_clusters): a bit per light, frame.light_clusters.w u32s a
// cluster
@group(0) @binding(20) var<storage, read> light_clusters: array<u32>;

// Local lights (notes/render.md "Local lights"). No inverse square: full strength out to 0.71 ·
// Range, then (1 - smoothstep(0.71 R, R, d)) / (1 + ((d - 0.71 R) / 10.5)²), steeper the longer the
// Range (light-range: Range 4-120 within 0.005 of the plateau).
const LIGHT_FADE_START: f32 = 0.71;
const LIGHT_FADE_LENGTH: f32 = 10.5;

fn light_fade(distance: f32, range: f32) -> f32 {
	let start = LIGHT_FADE_START * range;
	let beyond = max(distance - start, 0.0) / LIGHT_FADE_LENGTH;
	return (1.0 - smoothstep(start, range, distance)) / (1.0 + beyond * beyond);
}

// SpotLight and SurfaceLight cones: t², t = (cos θ - cos(Angle / 2)) / (1 - cos(Angle / 2)) (light-spot:
// Angle 30-180)
fn light_cone(cos_angle: f32, cos_half: f32) -> f32 {
	if (cos_half >= 1.0) {
		return 0.0;
	}
	let t = saturate((cos_angle - cos_half) / (1.0 - cos_half));
	return t * t;
}

// The share of a shadowed light reaching `world_position`: its shadow face by the major axis from
// the light (a SurfaceLight's: from light.shadow.z behind its face, render::light_data), the position
// pushed off the surface by a texel and a half, 16 compares over a disk of LIGHT_SHADOW_FILTER of a
// face's 1024 texels whatever its size (light.shadow.w; light-materials g2: Studio's edge on the floor
// 8 studs from the light is ~0.22 studs from 10 to 90 %; 10 texels gave 0.34).
const LIGHT_SHADOW_TEXELS: f32 = 1024.0;
const LIGHT_SHADOW_FILTER: f32 = 6.5;

fn light_shadow(light: Light, world_position: vec3<f32>, geometric_normal: vec3<f32>) -> f32 {
	let from_light = world_position - (light.position.xyz - light.direction.xyz * light.shadow.z);
	let a = abs(from_light);
	var face = 4u + select(1u, 0u, from_light.z > 0.0);
	if (a.x >= a.y && a.x >= a.z) {
		face = select(1u, 0u, from_light.x > 0.0);
	} else if (a.y >= a.z) {
		face = 2u + select(1u, 0u, from_light.y > 0.0);
	}
	// a texel at this distance: 2 tan(50°) / size (the faces' 100° field of view), and one of 1024;
	// a narrow SurfaceLight's one face aimed down its normal, 2 tan(its half-angle) (render::light_data)
	var reach = max(a.x, max(a.y, a.z)) * 2.3835;
	if (light.extra.z > 0.0) {
		face = 0u;
		reach = max(dot(from_light, light.direction.xyz), 1e-3) * 2.0 * light.extra.z;
	}
	let texel = reach / light.shadow.w;
	let disk = reach / LIGHT_SHADOW_TEXELS * LIGHT_SHADOW_FILTER;
	let layer = u32(light.shadow.x) + face;
	// off the surface, and towards the light by as much as the surface recedes across the filter
	// disk (the slope of a surface the light grazes would shadow itself otherwise), up to tan θ 50
	// (light-graze-kinds: capped at 10, lights 0.05-0.2 studs over a floor shadowed it to 0.53-0.95
	// of itself where Studio keeps 0.66-1.03 as `light_graze` gives). A surface turned away from a
	// SurfaceLight (its size term lights it) is not lifted: what stands between it and the light shadows
	// it (fixtures/local-lights.luau "backs": the far side of a board 1 or 2 studs thick before a
	// SurfaceLight keeps none of the light in a Roblox client, where the lift crossed the board; one 0.2
	// thick keeps 0.58 at Angle 0, 0.26 at 30 and 0.10 at 90 and 180, not modelled)
	let l = -from_light / max(length(from_light), 1e-4);
	let facing = dot(geometric_normal, l);
	let n_dot_l = clamp(facing, 0.02, 1.0);
	let slope = select(0.0, sqrt(1.0 - n_dot_l * n_dot_l) / n_dot_l, facing >= 0.0 || u32(light.position.w) != 2u);
	let lifted = world_position + geometric_normal * (texel * 1.5) + l * (disk * slope);
	let clip = light_shadow_matrices[layer] * vec4<f32>(lifted, 1.0);
	let ndc = clip.xyz / clip.w;
	let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
	let taps = array<vec2<f32>, 16>(
		vec2<f32>(-0.94201624, -0.39906216), vec2<f32>(0.94558609, -0.76890725),
		vec2<f32>(-0.094184101, -0.92938870), vec2<f32>(0.34495938, 0.29387760),
		vec2<f32>(-0.91588581, 0.45771432), vec2<f32>(-0.81544232, -0.87912464),
		vec2<f32>(-0.38277543, 0.27676845), vec2<f32>(0.97484398, 0.75648379),
		vec2<f32>(0.44323325, -0.97511554), vec2<f32>(0.53742981, -0.47373420),
		vec2<f32>(-0.26496911, -0.41893023), vec2<f32>(0.79197514, 0.19090188),
		vec2<f32>(-0.24188840, 0.99706507), vec2<f32>(-0.81409955, 0.91437590),
		vec2<f32>(0.19984126, 0.78641367), vec2<f32>(0.14383161, -0.14100790),
	);
	var sum = 0.0;
	for (var i = 0; i < 16; i++) {
		sum += textureSampleCompareLevel(light_shadow_maps, shadow_sampler, uv + taps[i] * (LIGHT_SHADOW_FILTER / LIGHT_SHADOW_TEXELS), layer, ndc.z);
	}
	return sum / 16.0;
}

// What a shadowed light keeps of itself on a surface it grazes, Studio's own self-shadowing that the
// lifted lookup above avoids (light-graze, -graze-more, -graze-kinds, -graze-pairs: PointLights
// 0.05-4 studs over floors at Range 4-120, shadowed over unshadowed, rings at least a fifth of their
// radius in from the floor's edge, where the darkening thins): a function of u = (d / Range)^1.5 tan θ
// alone, θ from the surface's normal to the light, 1 - 0.258 x^1.84 / (1 + x^1.84), x = max(u -
// 0.59, 0) / 7.96 (rms 0.008 over 660 rings, Brightness 1-100 alike; whole to u 0.6, 0.95 at 4.4,
// 0.86 at 9, 0.78 at 20). `range` is the longest of the shadowed lights at the light's position
// (render::shared_graze_ranges). A SpotLight's u is 1.63 times a PointLight's (light-graze-kinds:
// Angle 90 and 150 along a floor 0.5 and 2 studs up, rms 0.037 -> 0.014). A SurfaceLight's goes by
// the point its shadow is cast from, behind the face (render::light_data), its u 0.26 times a
// PointLight's (light-graze-kinds: 2-stud faces of Angle 180 along a floor with their centre 0.5 and 2
// studs up, the first's face crossing the floor: 0.98 of the unshadowed 8 studs out to 0.90 at 15,
// rms 0.058 -> 0.027 over its floor, 0.012 -> 0.011 at 2 up; facing down 0.5 and 2 up, 0.0015 and
// 0.006 either way; the rule at the face's nearest point gave 0.74 everywhere).
const LIGHT_GRAZE_SPOT: f32 = 1.63;
const LIGHT_GRAZE_SURFACE: f32 = 0.26;

fn light_graze(range: f32, distance: f32, n_dot_l: f32, scale: f32) -> f32 {
	let c = max(n_dot_l, 1e-3);
	let u = scale * pow(distance / range, 1.5) * sqrt(max(1.0 - c * c, 0.0)) / c;
	let x = pow(max(u - 0.59, 0.0) / 7.96, 1.84);
	return 1.0 - 0.258 * x / (1.0 + x);
}

// render::light_clusters' depth slices (SLICES, FIRST_DEPTH 1 stud)
const LIGHT_CLUSTER_SLICES: f32 = 32.0;

// The cluster of the view's pixel `pixel` (in the target's pixels) at `world_position`: its tile, and
// its slice by the logarithm of its view depth, as render::light_clusters finds it (that marks a light
// one cluster further each way, for the rounding)
fn light_cluster(world_position: vec3<f32>, pixel: vec2<f32>) -> u32 {
	let grid = frame.light_clusters;
	let depth = dot(world_position - frame.camera_position.xyz, frame.light_cluster_view.xyz);
	let slice = u32(clamp(floor(log(max(depth, 1e-6)) * frame.light_cluster_view.w), 0.0, LIGHT_CLUSTER_SLICES - 1.0));
	let tile = vec2<u32>(min(floor(pixel / grid.x), grid.yz - 1.0));
	return (slice * u32(grid.z) + tile.y) * u32(grid.y) + tile.x;
}

// Whether every surface takes its local light from the lights' voxels, near and far (LightingStyle
// Soft, a graphics quality level lighting none per pixel; render::lights_from_voxels decides it,
// frame.counts.w bit 1)
fn lights_from_voxels() -> bool {
	return (frame.counts.w & 1u) != 0u;
}

// What one light brings to a surface point before the surface's own shading: `voxels`, what its voxels give the
// far surfaces (the reader scales it, VOXEL_LIGHT_SURFACES); per pixel the direction `l` to it, its colour times its
// strength (`radiance`: fade, cone, shadow and grazing darkening; 0 beyond its Range, or where its voxels alone light
// the point) and `wrap`, how far a SurfaceLight's size raises the diffuse n·l. `far`: how far the surface is into the
// voxels' handover, `distant`: into 580-600 studs (`local_light_reach`)
struct LightShare {
	voxels: vec3<f32>,
	l: vec3<f32>,
	radiance: vec3<f32>,
	wrap: f32,
}

fn local_light_share(index: u32, world_position: vec3<f32>, geometric_normal: vec3<f32>, far: f32, distant: f32, read_at: vec3<f32>) -> LightShare {
	var share = LightShare(vec3<f32>(0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0), 0.0);
	let light = lights[index];
	if (lights_from_voxels()) {
		// LightingStyle Soft, a level lighting none per pixel: the voxels alone, near and far; a light
		// placed with a playback lights the near surfaces only, as it lights particles near the camera at
		// once (light-latency)
		share.voxels = voxel_light(light, read_at) * select(1.0 - distant, 1.0, light.extra.x > 0.0);
		return share;
	}
	if (far > 0.0) {
		if (light.extra.x > 0.0) {
			share.voxels = voxel_light(light, read_at);
		} else if (frame.local_light_far.y > 0.0 && distant < 1.0) {
			// past a lower level's per-pixel reach a placed light's voxels light what is short of 580-600
			// studs (the client's `lightline` at levels 13-18: a script's lamps lit along the whole rail)
			share.voxels = voxel_light(light, read_at) * (1.0 - distant);
		}
	}
	if (far >= 1.0) {
		return share;
	}
	let kind = u32(light.position.w);
	var source = light.position.xyz;
	var straight = false;
	if (kind == 2u) {
		// a SurfaceLight shines from the point of its face nearest the surface (light-surface)
		let offset = world_position - light.position.xyz;
		let across = vec2<f32>(dot(offset, light.right.xyz), dot(offset, light.up.xyz));
		source += light.right.xyz * clamp(across.x, -light.right.w, light.right.w);
		source += light.up.xyz * clamp(across.y, -light.up.w, light.up.w);
		straight = abs(across.x) <= light.right.w && abs(across.y) <= light.up.w && dot(offset, light.direction.xyz) > 0.0;
	}
	let to_light = source - world_position;
	let distance = length(to_light);
	let range = light.color.w;
	if (distance >= range || distance < 1e-4) {
		return share;
	}
	let l = to_light / distance;
	var strength = light_fade(distance, range);
	if (kind != 0u) {
		// straight ahead of a SurfaceLight's face the cone is whole at any Angle, 0 included (light-angle-small)
		strength *= select(light_cone(dot(-l, light.direction.xyz), light.direction.w), 1.0, straight);
	}
	if (light.shadow.x >= 0.0 && strength > 0.0) {
		strength *= light_shadow(light, world_position, geometric_normal);
		if (kind == 2u) {
			// (a surface turned away from that point casts nothing there to shadow itself with: the size
			// term lights it whole, light-attack's floor-level face of Angle 180 over a unit)
			let to_apex = light.position.xyz - light.direction.xyz * light.shadow.z - world_position;
			let apart = max(length(to_apex), 1e-4);
			let facing = dot(geometric_normal, to_apex / apart);
			strength *= select(1.0, light_graze(light.extra.y, apart, facing, LIGHT_GRAZE_SURFACE), facing > 0.0);
		} else {
			strength *= light_graze(light.extra.y, distance, dot(geometric_normal, l), select(1.0, LIGHT_GRAZE_SPOT, kind == 1u));
		}
	}
	// a SurfaceLight's diffuse n·l (to the nearest point) is raised by the face's size over the
	// distance, at most 1: r / d, r half the square root of its area (light-surface, -floor,
	// -rect: per case r 1.02-1.07 times that for faces of 1, 6 and 12 studs, 2.50-2.53 for 12 x 2
	// faces whichever way round, against 2.45; walls parallel to large faces show no n·l, small
	// ones far away nearly all of it, a floor at right angles reads n·l times the face centre's
	// height over the nearest edge's, and Angle 180 falls off beyond 55 degrees)
	var wrap = 0.0;
	if (kind == 2u) {
		wrap = sqrt(light.right.w * light.up.w) / distance;
	}
	share.l = l;
	share.radiance = light.color.xyz * strength;
	share.wrap = wrap;
	return share;
}

// How far a surface point is into the local lights' handover to their voxels: x `far`, from 580 to 600 studs from
// the camera (a graphics quality level lighting them per pixel nearer: its own, render::local_light_far), y
// `distant`, from 580 to 600 studs, past which a light placed with a playback lights nothing (light-latency)
fn local_light_reach(world_position: vec3<f32>) -> vec2<f32> {
	let distant = saturate((distance(world_position, frame.camera_position.xyz) - LOCAL_LIGHT_FAR_START) / (LOCAL_LIGHT_FAR_END - LOCAL_LIGHT_FAR_START));
	var far = distant;
	if (frame.local_light_far.y > 0.0) {
		// a graphics quality level lighting them per pixel nearer (render::local_light_far)
		far = saturate((distance(world_position, frame.camera_position.xyz) - frame.local_light_far.x) / (frame.local_light_far.y - frame.local_light_far.x));
	}
	return vec2<f32>(far, distant);
}
