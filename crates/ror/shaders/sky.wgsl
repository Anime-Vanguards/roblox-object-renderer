// The skybox behind the scene (or the flat / transparent background), with the sun and the moon.

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var sky_cube: texture_cube<f32>;
@group(0) @binding(2) var sky_sampler: sampler;

struct SkyParams {
	// x radiance multiplier, y draw sky (1) or transparent background (0), z 1 when the background is a
	// ViewportFrame's clear colour, written as it is (not premultiplied: Background::Frame)
	params: vec4<f32>,
	// background colour when the sky is off
	background: vec4<f32>,
	// the sun's direction and the tangent of half its angular size (the texture's square)
	sun: vec4<f32>,
	// the moon's
	moon: vec4<f32>,
	// x the sun's weight, y the moon's: what a texel of value 1 adds to the √ radiance (0: not drawn)
	bodies: vec4<f32>,
	// the skybox's own images: x 1 when drawn from them (else the cube), z the radiance of a texel
	// of 1
	images: vec4<f32>,
	// SkyboxOrientation's rotation by columns: a direction looks along rotation · direction in them
	rotation: array<vec4<f32>, 3>,
	// the time of day's display tints above and below the horizon, w 1 when tinted
	// (render::environment::tint_at)
	tint_above: vec4<f32>,
	tint_below: vec4<f32>,
	// the Atmosphere (render::environment::AtmosphereSky): haze (τ0, b, floor, 1 when on), its colour,
	// its light along +X, -X, +Y, -Y, +Z, -Z, the glare, its exponents, towards the sun
	haze: vec4<f32>,
	haze_color: vec4<f32>,
	haze_light: array<vec4<f32>, 6>,
	glare: vec4<f32>,
	decay: vec4<f32>,
	glare_sun: vec4<f32>,
	// the camera's rays: its rotation and projection inverted, the eye at the origin
	// (render::camera::Camera::ray_unprojection)
	rays: mat4x4<f32>,
	// Terrain Clouds (render::clouds::uniform): x 1 when drawn, y the shape's threshold (Cover), z
	// Density, w the march's steps; Color (linear, w the detail's amplitude); the detail's scale, the
	// march's start within the layer, the coverage under which a pixel shows the sky alone, w 1 for
	// plain alpha blending; their light (render::clouds::Light): the sun's, the sky's and the flat
	// ambient's weights, the sun's optical depth's scale; the sky light's gray floor and its weight
	// by the sun's height, the multiple scattering's weight
	clouds: vec4<f32>,
	cloud_color: vec4<f32>,
	cloud_shape_params: vec4<f32>,
	cloud_light: vec4<f32>,
	cloud_sky: vec4<f32>,
	cloud_spare: vec4<f32>,
	// the stars (render::star_uniform): x how many, y their light as a share of midnight's, z a star's
	// side in the target's pixels
	stars: vec4<f32>,
}
@group(0) @binding(3) var<uniform> sky: SkyParams;
@group(0) @binding(4) var sun_texture: texture_2d<f32>;
@group(0) @binding(5) var moon_texture: texture_2d<f32>;
@group(0) @binding(6) var sky_images: texture_2d_array<f32>;
@group(0) @binding(7) var cloud_shape: texture_3d<f32>;
@group(0) @binding(8) var cloud_sampler: sampler;
@group(0) @binding(9) var cloud_detail: texture_3d<f32>;


// A celestial body's texture along `direction`: a square of the body's angular size facing the
// viewer, upright and mirrored left to right (sky-elements: a white square sun texture covers 10.5
// degrees each way at SunAngularSize 21; moon-black-size30 shows the moon texture's right edge on
// the left)
fn body(texture: texture_2d<f32>, centre: vec4<f32>, direction: vec3<f32>) -> vec3<f32> {
	let facing = dot(direction, centre.xyz);
	if (facing <= 0.0 || centre.w <= 0.0) {
		return vec3<f32>(0.0);
	}
	let helper = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(centre.y) > 0.999);
	let right = normalize(cross(centre.xyz, helper));
	let up = cross(right, centre.xyz);
	let uv = vec2<f32>(-dot(direction, right), -dot(direction, up)) / (facing * centre.w);
	if (any(abs(uv) > vec2<f32>(1.0))) {
		return vec3<f32>(0.0);
	}
	return textureSampleLevel(texture, sky_sampler, uv * 0.5 + 0.5, 0.0).rgb;
}

// The stars (notes/render.md "Sky elements"; the Roblox client 0.747 at midnight over a dark skybox,
// two sessions): StarCount stars (up to 5,000) strewn over the whole sky, 0.6 of them above the
// horizon and 0.4 below, each a square one pixel across at any field of view, its light spread over
// the pixels it covers. Every StarCount change strews them anew in Roblox, so only their statistics
// can match: they are strewn here once and for all, a star in some of the cells of a 64 x 64 grid on
// each face of a cube, each cell's chance its solid angle times the density.
const STAR_CELLS: u32 = 64u;
const STAR_ABOVE: f32 = 0.6;
// A star's light (the tone curve's input it adds, summed over its pixels, at midnight and 0 EV; log10)
// at the quantiles below: from every star more than 50 degrees up in eight views (3,980 stars),
// 0.17 its median
const STAR_QUANTILES = array<f32, 10>(0.0, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 0.95, 0.99, 1.0);
const STAR_LOG_LIGHT = array<f32, 10>(-1.75, -1.387, -1.283, -1.059, -0.773, -0.475, -0.273, -0.17, 0.029, 0.12);
// Their colours, four kinds (rgb over their mean, each within 0.008 of it): white, blue, purple and
// orange, and the shares of stars up to each (0.391, 0.237, 0.221, 0.15)
const STAR_COLOURS = array<vec3<f32>, 4>(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(0.708, 0.908, 1.384), vec3<f32>(0.929, 0.455, 1.616), vec3<f32>(1.366, 0.91, 0.724));
const STAR_SHARES = array<f32, 3>(0.391, 0.628, 0.849);
// A pixel takes a quarter of a star's light for each of these four points (pixels, y down, from its
// centre: the usual 4x multisampling pattern) the star's square covers: so Studio's stars cover 1, 2,
// 3 and 4 pixels 6.5, 40.7, 47.5 and 5.4 % of the time (sky-elements stars-3000; the client 0.747 6.0,
// 38.0, 49.9, 6.1), where the share of the square over each pixel gives 0.8, 15.8, 17.4, 65.9 % (by a
// model of the stars' light through the tone curve; these points 6.1, 37.5, 50.2, 6.1). Supersampled
// (a star's side over one pixel) the share over each pixel stands in.
const STAR_SAMPLES = array<vec2<f32>, 4>(vec2<f32>(-0.125, -0.375), vec2<f32>(0.375, -0.125), vec2<f32>(-0.375, 0.125), vec2<f32>(0.125, 0.375));

fn star_hash(value: u32) -> u32 {
	let state = value * 747796405u + 2891336453u;
	let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
	return (word >> 22u) ^ word;
}

fn star_unit(value: u32) -> f32 {
	return f32(value >> 8u) / 16777216.0;
}

// The cube face a direction falls on (render::environment::cube_lookup's faces) and its (s, t) there,
// -1 to 1
fn star_face(direction: vec3<f32>) -> vec3<f32> {
	let a = abs(direction);
	if (a.x >= a.y && a.x >= a.z) {
		if (direction.x > 0.0) {
			return vec3<f32>(-direction.z / a.x, -direction.y / a.x, 0.0);
		}
		return vec3<f32>(direction.z / a.x, -direction.y / a.x, 1.0);
	}
	if (a.y >= a.z) {
		if (direction.y > 0.0) {
			return vec3<f32>(direction.x / a.y, direction.z / a.y, 2.0);
		}
		return vec3<f32>(direction.x / a.y, -direction.z / a.y, 3.0);
	}
	if (direction.z > 0.0) {
		return vec3<f32>(direction.x / a.z, -direction.y / a.z, 4.0);
	}
	return vec3<f32>(-direction.x / a.z, -direction.y / a.z, 5.0);
}

// The direction of (s, t) on a face (render::environment::cube_direction)
fn star_direction(face: u32, s: f32, t: f32) -> vec3<f32> {
	switch (face) {
		case 0u: {
			return vec3<f32>(1.0, -t, -s);
		}
		case 1u: {
			return vec3<f32>(-1.0, -t, s);
		}
		case 2u: {
			return vec3<f32>(s, 1.0, t);
		}
		case 3u: {
			return vec3<f32>(s, -1.0, -t);
		}
		case 4u: {
			return vec3<f32>(s, -t, 1.0);
		}
		default: {
			return vec3<f32>(-s, -t, -1.0);
		}
	}
}

// The cell a direction falls in, numbered over the six faces
fn star_cell(direction: vec3<f32>) -> u32 {
	let at = star_face(direction);
	let cells = f32(STAR_CELLS);
	let column = u32(clamp(floor((at.x * 0.5 + 0.5) * cells), 0.0, cells - 1.0));
	let row = u32(clamp(floor((at.y * 0.5 + 0.5) * cells), 0.0, cells - 1.0));
	return (u32(at.z) * STAR_CELLS + row) * STAR_CELLS + column;
}

// What the haze leaves of a star along `direction` (its share of the sky h, through_atmosphere's):
// all of it below h 0.17, 0.49 to 0.53, 0.13 to 0.855, none above, so none below the
// horizon (Haze 0-5 at midnight over a black haze: stars at h 0.04-0.1 kept 1.0-1.1, at 0.25-0.49
// 0.45-0.5, at 0.58-0.85 0.12-0.13, at 0.86 and up none)
fn star_haze(direction: vec3<f32>) -> f32 {
	if (sky.haze.w < 0.5) {
		return 1.0;
	}
	var haze = 1.0;
	if (direction.y > 0.0) {
		haze = 1.0 - sky.haze.z * exp(-sky.haze.x * pow(direction.y, -sky.haze.y));
	}
	if (haze < 0.17) {
		return 1.0;
	}
	if (haze < 0.53) {
		return 0.49;
	}
	if (haze < 0.855) {
		return 0.13;
	}
	return 0.0;
}

// The radiance the stars add to the pixel at `ndc` looking along `direction` (`ddx`, `ddy` its change
// to the next pixel): each star near enough to cover part of it, in the cells under the corners of
// the square a star's side out from it
fn stars(direction: vec3<f32>, ddx: vec3<f32>, ddy: vec3<f32>, ndc: vec2<f32>) -> vec3<f32> {
	let count = sky.stars.x;
	if (count < 0.5) {
		return vec3<f32>(0.0);
	}
	let side = sky.stars.z;
	var seen = array<u32, 4>(0xffffffffu, 0xffffffffu, 0xffffffffu, 0xffffffffu);
	var light = vec3<f32>(0.0);
	let cells = f32(STAR_CELLS);
	let cell_angle = 4.0 / (cells * cells);
	for (var corner = 0u; corner < 4u; corner++) {
		let sx = select(-side, side, (corner & 1u) != 0u);
		let sy = select(-side, side, (corner & 2u) != 0u);
		let cell = star_cell(direction + ddx * sx + ddy * sy);
		var repeated = false;
		for (var k = 0u; k < corner; k++) {
			repeated = repeated || seen[k] == cell;
		}
		seen[corner] = cell;
		if (repeated) {
			continue;
		}
		let r0 = star_hash(cell ^ 0x5bd1e995u);
		let r1 = star_hash(r0);
		let r2 = star_hash(r1);
		let r3 = star_hash(r2);
		let r4 = star_hash(r3);
		let face = cell / (STAR_CELLS * STAR_CELLS);
		let column = cell % STAR_CELLS;
		let row = (cell / STAR_CELLS) % STAR_CELLS;
		let s = (f32(column) + star_unit(r1)) / cells * 2.0 - 1.0;
		let t = (f32(row) + star_unit(r2)) / cells * 2.0 - 1.0;
		let star = normalize(star_direction(face, s, t));
		// the cell's chance: its solid angle at the star times the stars a steradian there
		let solid = cell_angle / pow(1.0 + s * s + t * t, 1.5);
		let density = count * select(1.0 - STAR_ABOVE, STAR_ABOVE, star.y > 0.0) / 6.2831853;
		if (star_unit(r0) >= density * solid) {
			continue;
		}
		// where it lands, in pixels from this one's centre, against the square it covers
		let clip = frame.view_proj * vec4<f32>(star, 0.0);
		if (clip.w <= 0.0) {
			continue;
		}
		let offset = (clip.xy / clip.w - ndc) * vec2<f32>(0.5, -0.5) * frame.viewport.xy / side;
		var cover = 0.0;
		if (side <= 1.0) {
			for (var sample = 0u; sample < 4u; sample++) {
				let inside = abs(offset - STAR_SAMPLES[sample]) <= vec2<f32>(0.5);
				cover += select(0.0, 0.25, inside.x && inside.y);
			}
		} else {
			cover = max(1.0 - abs(offset.x), 0.0) * max(1.0 - abs(offset.y), 0.0);
		}
		if (cover <= 0.0) {
			continue;
		}
		let quantile = star_unit(r3);
		var index = 1u;
		for (; index < 9u && STAR_QUANTILES[index] < quantile; index++) {
		}
		let span = (quantile - STAR_QUANTILES[index - 1u]) / (STAR_QUANTILES[index] - STAR_QUANTILES[index - 1u]);
		let own = pow(10.0, mix(STAR_LOG_LIGHT[index - 1u], STAR_LOG_LIGHT[index], span));
		let kind = star_unit(r4);
		var colour = STAR_COLOURS[3];
		if (kind < STAR_SHARES[0]) {
			colour = STAR_COLOURS[0];
		} else if (kind < STAR_SHARES[1]) {
			colour = STAR_COLOURS[1];
		} else if (kind < STAR_SHARES[2]) {
			colour = STAR_COLOURS[2];
		}
		// dimmer towards the horizon, above it and below alike (by the stars' light in elevation
		// bands over six views: 0.22 at the horizon, 0.5 at 16 degrees, 0.93 at 40)
		let elevation = abs(degrees(asin(clamp(star.y, -1.0, 1.0))));
		let fade = 1.0 - 0.78 * exp(-pow(elevation / 24.7, 1.87));
		light += colour * (own * cover * fade * star_haze(star));
	}
	return light * sky.stars.y;
}

// Roblox's skybox layout (render::environment::roblox_sky_direction): (u, v, image), images Bk, Dn,
// Ft, Lf, Rt, Up = 0..5
fn roblox_sky(direction: vec3<f32>) -> vec3<f32> {
	let a = abs(direction);
	var at: vec3<f32>;
	if (a.x >= a.y && a.x >= a.z) {
		if (direction.x > 0.0) {
			at = vec3<f32>(direction.z / a.x, -direction.y / a.x, 3.0);
		} else {
			at = vec3<f32>(-direction.z / a.x, -direction.y / a.x, 4.0);
		}
	} else if (a.y >= a.z) {
		if (direction.y > 0.0) {
			at = vec3<f32>(-direction.z / a.y, -direction.x / a.y, 5.0);
		} else {
			at = vec3<f32>(-direction.z / a.y, direction.x / a.y, 1.0);
		}
	} else if (direction.z > 0.0) {
		at = vec3<f32>(-direction.x / a.z, -direction.y / a.z, 0.0);
	} else {
		at = vec3<f32>(direction.x / a.z, -direction.y / a.z, 2.0);
	}
	return vec3<f32>(at.xy * 0.5 + 0.5, at.z);
}

// `direction`'s (u, v) on the given image of the layout, as `roblox_sky` maps the directions it
// assigns to that image
fn roblox_sky_on(direction: vec3<f32>, image: f32) -> vec2<f32> {
	var at: vec2<f32>;
	switch (i32(image)) {
		case 3: {
			at = vec2<f32>(direction.z, -direction.y) / direction.x;
		}
		case 4: {
			at = vec2<f32>(direction.z, direction.y) / direction.x;
		}
		case 5: {
			at = vec2<f32>(-direction.z, -direction.x) / direction.y;
		}
		case 1: {
			at = vec2<f32>(direction.z, -direction.x) / direction.y;
		}
		case 0: {
			at = vec2<f32>(-direction.x, -direction.y) / direction.z;
		}
		default: {
			at = vec2<f32>(-direction.x, direction.y) / direction.z;
		}
	}
	return at * 0.5 + 0.5;
}

// The time of day's radiance tint along `direction` (render::environment::tint_at)
fn tint_at(direction: vec3<f32>) -> vec3<f32> {
	let side = max(abs(direction.x), abs(direction.z));
	var blend = select(1.0, 0.0, direction.y > 0.0);
	if (side > 0.0) {
		blend = saturate(0.75 * (1.0 - direction.y / side));
	}
	let shown = mix(sky.tint_above.rgb, sky.tint_below.rgb, blend);
	return shown * shown;
}

// The skybox seen through the Atmosphere (render::environment::AtmosphereSky::sky)
fn through_atmosphere(direction: vec3<f32>, skybox: vec3<f32>) -> vec3<f32> {
	var haze = 1.0;
	if (direction.y > 0.0) {
		haze = 1.0 - sky.haze.z * exp(-sky.haze.x * pow(direction.y, -sky.haze.y));
	}
	if (haze <= 0.0) {
		return skybox;
	}
	let n2 = direction * direction;
	let light_x = select(sky.haze_light[1].rgb, sky.haze_light[0].rgb, direction.x > 0.0);
	let light_y = select(sky.haze_light[3].rgb, sky.haze_light[2].rgb, direction.y > 0.0);
	let light_z = select(sky.haze_light[5].rgb, sky.haze_light[4].rgb, direction.z > 0.0);
	let light = (light_x * n2.x + light_y * n2.y + light_z * n2.z) / max(n2.x + n2.y + n2.z, 1e-8);
	let lobe = max(0.5 + 0.5 * dot(direction, sky.glare_sun.xyz), 1e-6);
	let glare = sky.glare.rgb * pow(vec3<f32>(lobe), sky.decay.rgb);
	return skybox * (1.0 - haze) + sky.haze_color.rgb * light * haze + glare * (haze * haze);
}

// The skybox drawn from its own images, as the cube would hold it with all their detail. Each
// pixel's mip from its own footprint on the image it samples, as a GPU picks it: the direction's
// screen derivatives `ddx` and `ddy` projected onto that image (continuous across the images' seams,
// where the (u, v)'s own derivatives jump). A view at an angle to the images minifies them in parts
// and Studio blends in coarser mips there (a skybox survey: 99.26 → 99.44 % of pixels within 2
// of 255 against one level for the whole view)
fn skybox(direction: vec3<f32>, ddx: vec3<f32>, ddy: vec3<f32>) -> vec3<f32> {
	let rotation = mat3x3<f32>(sky.rotation[0].xyz, sky.rotation[1].xyz, sky.rotation[2].xyz);
	let turned = rotation * direction;
	let at = roblox_sky(turned);
	let du = roblox_sky_on(turned + rotation * ddx, at.z) - at.xy;
	let dv = roblox_sky_on(turned + rotation * ddy, at.z) - at.xy;
	let texel = textureSampleGrad(sky_images, sky_sampler, at.xy, i32(at.z), du, dv).rgb;
	var radiance = texel * texel * sky.images.z;
	if (sky.tint_above.w > 0.5) {
		radiance *= tint_at(direction);
	}
	if (sky.haze.w > 0.5) {
		radiance = through_atmosphere(direction, radiance);
	}
	return radiance;
}

@fragment
fn fs_sky(input: FullscreenOutput) -> @location(0) vec4<f32> {
	// the scene target holds √x (mesh.wgsl)
	if (sky.params.z > 0.5) {
		return vec4<f32>(sqrt(sky.background.rgb), sky.background.a);
	}
	if (sky.params.y < 0.5) {
		return vec4<f32>(sqrt(sky.background.rgb) * sky.background.a, sky.background.a);
	}
	// reversed infinite projection: depth 0.5 is a finite point on the pixel's ray
	let point = sky.rays * vec4<f32>(input.ndc, 0.5, 1.0);
	let direction = normalize(point.xyz / point.w);
	let ddx = dpdx(direction);
	let ddy = dpdy(direction);
	let cloud = clouds(direction);
	var color: vec3<f32>;
	if (sky.images.x > 0.5) {
		color = skybox(direction, ddx, ddy);
	} else {
		color = textureSampleLevel(sky_cube, sky_sampler, direction, 0.0).rgb;
	}
	// the stars add to the sky's radiance, under the exposure
	color += stars(direction, ddx, ddy, input.ndc);
	// the sun and the moon add to the √ radiance (sky-sun: over skyboxes of gray 0 to 255 the sun's
	// centre is (√sky + 1.11)², its texture's value times the weight, at any exposure), the clouds over
	// them (the skybox and its haze behind: clouds stay white over an Atmosphere)
	var shown = sqrt(max(color * sky.params.x, vec3<f32>(0.0)));
	shown += body(sun_texture, sky.sun, direction) * sky.bodies.x;
	shown += body(moon_texture, sky.moon, direction) * sky.bodies.y;
	return vec4<f32>(over_clouds(shown, cloud), 1.0);
}
