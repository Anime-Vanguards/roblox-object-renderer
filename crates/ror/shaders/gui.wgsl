// SurfaceGuis and BillboardGuis (notes/render.md "GUIs"): rur's image of a GUI's content on a quad
// (a part's face, or facing the camera at its anchor's depth), lit by LightInfluence and blended over
// the √-space scene target; AlwaysOnTop ones are drawn over the finished image as they are.

@group(0) @binding(0) var<uniform> frame: Frame;
// the view's local lights (lights.wgsl)
@group(0) @binding(7) var<storage, read> lights: array<Light>;
// the sky chain the Atmosphere's fog fades into
@group(0) @binding(19) var faded_map: texture_cube<f32>;
@group(0) @binding(5) var linear_sampler: sampler;

@group(1) @binding(0) var gui_texture: texture_2d<f32>;
@group(1) @binding(1) var gui_sampler: sampler;

// A ViewportFrame's world (scene::guis::Viewport), straight alpha in display values: a window's as
// this view's camera draws it, the view's size, with the share of each pixel an opaque surface of it
// covers; a framed one's as its own camera draws it, the frame's proportions (mode.x 1). The frame's
// rect in the quad's uv and the image's tint (ImageColor3, its opacity in w: 0 for a GUI that shows no
// world); a framed one's UIGradient: t = plane.x · x + plane.y · y + plane.z over the frame (x, y its
// fractions), mode.y colour keypoints (t, r, g, b) and mode.z transparency ones (t, value); corner.xy
// its UICorner's radius in the quad's uv (0: square corners)
struct GuiWindow {
	rect: vec4<f32>,
	tint: vec4<f32>,
	mode: vec4<f32>,
	plane: vec4<f32>,
	colors: array<vec4<f32>, 20>,
	transparency: array<vec4<f32>, 20>,
	corner: vec4<f32>,
}
@group(2) @binding(0) var window_texture: texture_2d<f32>;
@group(2) @binding(1) var window_sampler: sampler;
@group(2) @binding(2) var<uniform> gui_window: GuiWindow;
@group(2) @binding(3) var window_share: texture_2d<f32>;

struct GuiInput {
	@location(0) position: vec3<f32>,
	@location(1) uv: vec2<f32>,
	@location(2) normal: vec3<f32>,
	// x LightInfluence, y Brightness, z and w unused
	@location(3) params: vec4<f32>,
}

struct GuiOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) uv: vec2<f32>,
	@location(1) normal: vec3<f32>,
	@location(2) @interpolate(flat) params: vec4<f32>,
	@location(3) world_position: vec3<f32>,
}

// A SurfaceGui lies on its part's face: drawn a hair nearer (relative depth), it passes the depth test
// the coplanar face set
const GUI_DEPTH_BIAS: f32 = 1e-5;
// An unlit GUI colour c shows at radiance GUI_RADIANCE · Brightness · c², like a sky texel (gui-surface-
// basics: white 223 of 255 at Brightness 1; gui-brightness: linear over 0.5-8 at -3 and -2 EV)
const GUI_RADIANCE: f32 = 1.935;
// The lit colour: what a white Lambert surface facing the GUI's normal takes, times GUI_LIT_SCALE
// (fixtures/world-guis.luau in a Roblox client: frames facing the sun, under flat ambient, the sky and beside
// local lights; Studio's gui-light: a white frame facing a Brightness-1 sun reads 0.98, under Ambient 0.5 0.482
// where a part reads 0.495), no specular
const GUI_LIT_SCALE: f32 = 0.98;
// lit from behind at 0.41 of |n·l|, with GlobalShadows or not (fixtures/world-guis.luau "light" in a Roblox client: a
// slab turned 120° from the sun 0.40 / 0.41 in white / grey without them, a billboard seen from the side away from the
// sun 0.42 either way; Studio's edit viewport: 0.39 without them, none with them)
const GUI_BACK_LIGHT: f32 = 0.41;
// Minified, Studio's SurfaceGuis are as sharp as their full-size image sampled bilinearly (gui-content
// surface-far, 2.4 times smaller: 67.5 % of the GUI's pixels within 2 with the mips, 94.0 % from a bias
// of -1.5 on, the same at -6; gui-surface-basics light-*: one point better); -1.5 keeps the full-size
// image down to 2.8 times smaller, the mips beyond (not measured)
const GUI_MIP_BIAS: f32 = -1.5;

@vertex
fn vs_gui(input: GuiInput) -> GuiOutput {
	var output: GuiOutput;
	let clip = frame.view_proj * vec4<f32>(input.position, 1.0);
	output.clip = vec4<f32>(clip.xy, clip.z * (1.0 + GUI_DEPTH_BIAS), clip.w);
	output.uv = input.uv;
	output.normal = input.normal;
	output.params = input.params;
	output.world_position = input.position;
	return output;
}

// The local lights on a GUI at `world_position` facing `n` (a billboard's normal faces the camera): a white Lambert
// surface's diffuse there, per pixel as parts take them (with their shadows; their voxels far from the camera and
// under LightingStyle Soft), the view's clusters walked as mesh.wgsl `local_lights` walks them (fixtures/world-guis.luau
// "lights" in a Roblox client: frames beside a PointLight, a SpotLight and a SurfaceLight read the bare part's
// diffuse along the light's whole fade; a billboard with the light in its plane takes none of it, one before it all;
// a light behind a SurfaceGui's face none)
fn gui_local_lights(world_position: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> vec3<f32> {
	let reach = local_light_reach(world_position);
	let far = reach.x;
	let distant = reach.y;
	let read_at = world_position + n * 6.0;
	if (lights_from_voxels() && far <= 0.0) {
		let field = voxel_field_light(read_at);
		if (field.w > 0.0) {
			return field.rgb * VOXEL_LIGHT_SURFACES;
		}
	}
	var sum = vec3<f32>(0.0);
	var voxels = vec3<f32>(0.0);
	let count = frame.counts.x;
	let clustered = frame.light_clusters.w > 0.0;
	let words = select((count + 31u) / 32u, u32(frame.light_clusters.w), clustered);
	let first = select(0u, light_cluster(world_position, pixel) * words, clustered);
	for (var word = 0u; word < words; word++) {
		let left = count - word * 32u;
		var bits = select(select(0xffffffffu, (1u << left) - 1u, left < 32u), light_clusters[first + word], clustered);
		while (bits != 0u) {
			let i = word * 32u + firstTrailingBit(bits);
			bits &= bits - 1u;
			let share = local_light_share(i, world_position, n, far, distant, read_at);
			voxels += share.voxels;
			sum += share.radiance / PI * max(min(dot(n, share.l) + share.wrap, 1.0), 0.0);
		}
	}
	return sum * (1.0 - far) + voxels * (VOXEL_LIGHT_SURFACES * select(far, 1.0, lights_from_voxels()));
}

// The sky's light on a GUI: the same whichever way it faces, the mean of the ambient cube's six colours (common.wgsl
// `sky_light`; fixtures/world-guis.luau "sky": frames and billboards facing each axis read 0.98 of the mean of what a
// white part facing each axis reads, under the default sky and skies white in one image only, where the sky's
// average radiance fell 1-3.6 % short in green and blue)
fn gui_sky_light() -> vec3<f32> {
	var sum = vec3<f32>(0.0);
	for (var axis = 0; axis < 6; axis++) {
		sum += frame.sky_cube[axis].xyz;
	}
	return sum / 6.0;
}

fn gui_light(input: GuiOutput) -> vec3<f32> {
	let n = normalize(input.normal);
	let n_dot_l = dot(n, frame.sun_direction.xyz);
	// the sun through its shadow as parts take it, SurfaceGuis and billboards alike (fixtures/world-guis.luau "light" in
	// a Roblox client: with GlobalShadows frames on slabs facing the sun read as without them, whatever holds them
	// (opaque, invisible, half transparent, without CastShadow, a block, a 0.05 plate), a roof's shadow falls across
	// a frame as across the part beside it, and 700 studs off the light grid's sun shades both alike; Studio's edit
	// viewport lights them from its light grid instead); from behind, the shadow as the side facing the sun takes it
	let facing = n_dot_l >= 0.0;
	let shade = sun_shade(input.world_position, select(-n, n, facing)) * cloud_sun(input.world_position);
	let sun = frame.sun_color.rgb / PI * select(-n_dot_l * GUI_BACK_LIGHT, n_dot_l, facing) * shade;
	// OutdoorAmbient and the sky's light as far as the light grid sees the sky, as parts take them (fixtures/world-guis.luau
	// "light": frames under a roof keep 0.61 of OutdoorAmbient and 0.70 / 0.65 / 0.64 of the sky's light where the part
	// beside them keeps 0.61 and 0.68 / 0.64 / 0.63)
	let sky_open = light_grid_visibility(input.world_position, n);
	let flat_ambient = max(frame.ambient.rgb, frame.outdoor_ambient.rgb * (sky_open * frame.light_grid_scale.w));
	let sky = mix(LIGHT_GRID_SKY_CLOSED, gui_sky_light(), sky_open) * frame.environment.x;
	return (sun + flat_ambient + sky + gui_local_lights(input.world_position, n, input.clip.xy)) * GUI_LIT_SCALE;
}

// A UIGradient's keypoints at t, as rur interpolates them: the first's value before it, the last's
// after, linear between
fn gradient_colour(t: f32) -> vec3<f32> {
	let count = u32(gui_window.mode.y);
	var colour = gui_window.colors[0].yzw;
	for (var key = 1u; key < count; key++) {
		let a = gui_window.colors[key - 1u];
		let b = gui_window.colors[key];
		if (t > a.x) {
			colour = select(b.yzw, mix(a.yzw, b.yzw, (t - a.x) / (b.x - a.x)), t < b.x && b.x > a.x);
		}
	}
	return select(vec3<f32>(1.0), colour, count > 0u);
}

fn gradient_opacity(t: f32) -> f32 {
	let count = u32(gui_window.mode.z);
	var value = gui_window.transparency[0].y;
	for (var key = 1u; key < count; key++) {
		let a = gui_window.transparency[key - 1u];
		let b = gui_window.transparency[key];
		if (t > a.x) {
			value = select(b.y, mix(a.y, b.y, (t - a.x) / (b.x - a.x)), t < b.x && b.x > a.x);
		}
	}
	return 1.0 - select(0.0, value, count > 0u);
}

// The share of this pixel inside the frame's rounded corners: a UICorner clips the frame's world as it
// clips its background (anti-aliased over about a pixel); 1 everywhere with square corners.
fn corner_coverage(uv: vec2<f32>) -> f32 {
	let radius = max(gui_window.corner.xy, vec2<f32>(1e-6));
	let centre = (gui_window.rect.xy + gui_window.rect.zw) * 0.5;
	let half = (gui_window.rect.zw - gui_window.rect.xy) * 0.5;
	let distance = length(max(abs(uv - centre) - (half - radius), vec2<f32>(0.0)) / radius);
	let rounded = clamp((1.0 - distance) / max(fwidth(distance), 1e-6) + 0.5, 0.0, 1.0);
	return select(1.0, rounded, gui_window.corner.x > 0.0 && gui_window.corner.y > 0.0);
}

// What the GUI draw shows at this pixel (premultiplied display colour, c · a), its image `texel` with
// the world of its ViewportFrame, if it shows one.
fn gui_texel(input: GuiOutput, texel_in: vec4<f32>) -> vec4<f32> {
	var texel = texel_in;
	let rounded = corner_coverage(input.uv);
	if (gui_window.mode.x > 0.5) {
		// A framed ViewportFrame's picture, laid in under what the GUI draws over the frame: this draw
		// goes over the GUI's image so far, R, and `texel` is what draws over the frame, O, so it blends
		// coverage · O + (1 - O.a) · picture at alpha coverage, which leaves (1 - coverage) · R + coverage
		// · O + (1 - O.a) · picture: the picture between what is under the frame and what is over it. The
		// picture fills the frame's rect, clipped to it, tinted by ImageColor3, ImageTransparency and its
		// UIGradient as an image (rur's ImageLabel)
		let size = max(gui_window.rect.zw - gui_window.rect.xy, vec2<f32>(1e-6));
		let place = (input.uv - gui_window.rect.xy) / size;
		let inside = all(place >= vec2<f32>(0.0)) && all(place <= vec2<f32>(1.0));
		let world = textureSampleLevel(window_texture, window_sampler, clamp(place, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
		let t = dot(gui_window.plane.xy, place) + gui_window.plane.z;
		let coverage = select(0.0, world.a * gui_window.tint.a * gradient_opacity(t) * rounded, inside);
		let picture = world.rgb * gui_window.tint.rgb * gradient_colour(t) * coverage;
		return vec4<f32>(texel.rgb * coverage + picture * (1.0 - texel.a), coverage);
	}
	// a window's world over the GUI's own image (the frame's background) inside the frame's rect: the
	// world as seen through the face, read at this pixel of the view (a window player's camera
	// follows the player's so that its frame shows exactly that). An opaque surface replaces the
	// frame's background with its colour at its vertex alpha (mesh.wgsl fs_opaque): at alpha 0 a hole
	// through it, decals drawn over it (Studio captures of an effect's rift, crater and
	// portal: their masks' footprints show the ground under the window's orange)
	let at = input.clip.xy * frame.viewport.zw;
	let world = textureSampleLevel(window_texture, window_sampler, at, 0.0);
	let opaque = textureSampleLevel(window_share, window_sampler, at, 0.0).r;
	let inside = all(input.uv >= gui_window.rect.xy) && all(input.uv <= gui_window.rect.zw);
	let opacity = select(0.0, gui_window.tint.a * rounded, inside);
	let coverage = world.a * opacity;
	let hole = opaque * (1.0 - world.a) * opacity;
	return vec4<f32>(world.rgb * gui_window.tint.rgb * coverage + texel.rgb * (1.0 - coverage - hole), coverage + texel.a * (1.0 - coverage - hole));
}

@fragment
fn fs_gui(input: GuiOutput) -> @location(0) vec4<f32> {
	// premultiplied display colour (c · a)
	let texel = gui_texel(input, textureSampleBias(gui_texture, gui_sampler, input.uv, GUI_MIP_BIAS));
	if (texel.a <= 0.0) {
		discard;
	}
	let c = texel.rgb / texel.a;
	// LightInfluence mixes the unlit and the lit colour by its square root, like a beam's (gui-light:
	// the unlit share at LightInfluence 0.25 / 0.5 / 0.75 is 0.50 / 0.29 / 0.13 with no light)
	let weight = sqrt(saturate(input.params.x));
	let unlit = GUI_RADIANCE * input.params.y * (1.0 - weight);
	var radiance = c * c * (vec3<f32>(unlit) + gui_light(input) * weight);
	if (frame.atmosphere.w > 0.0) {
		radiance = atmosphere_fog(frame, faded_map, linear_sampler, input.world_position, radiance, 0.0);
	} else if (frame.fog.z > 0.5) {
		radiance = linear_fog(frame, input.world_position, radiance, 0.0);
	}
	// at most EXPOSED_CAP once exposed, as every surface before it blends (gui-bloom-layers: a white
	// Brightness-15 frame under a black one at BackgroundTransparency 0.5 shows 224 of 255 and no halo,
	// (0.5 √7.84)², where ror drew 255 and a halo of 129; gui-brightness-alpha ev0: a frame at 0.3 shows
	// (0.7 √7.84)² = 3.84, just under the bloom's threshold, and glows not at all)
	radiance = min(radiance, vec3<f32>(EXPOSED_CAP / frame.environment.z));
	return vec4<f32>(sqrt(max(radiance, vec3<f32>(0.0))) * texel.a, texel.a);
}

// AlwaysOnTop: over the finished image, the GUI's own colours (gui-surface-basics: alwaysontop, white
// 255 behind a wall)
@fragment
fn fs_gui_overlay(input: GuiOutput) -> @location(0) vec4<f32> {
	let texel = textureSampleBias(gui_texture, gui_sampler, input.uv, GUI_MIP_BIAS);
	// a framed ViewportFrame's picture laid in (gui_texel); no window draws here
	if (gui_window.mode.x > 0.5) {
		return gui_texel(input, texel);
	}
	return texel;
}
