// The sun's shadow cascades as Studio builds them (notes/render.md "Shadows"): a cascade's depth
// from the sun turned into exponentially warped moments (EVSM: e^(c z), e^(2 c z), -e^(-c z),
// e^(-2 c z)) and blurred by a separable Gaussian, horizontally from the depth map, then vertically
// into the cascade's column of the moment atlas.

struct SunBlur {
	// x the Gaussian's sigma (texels), y taps on each side, z the warp exponent c, w unused
	params: vec4<f32>,
	// x the depth pass's far plane (studs), y its eye's distance from the cascade's centre, z the
	// cascade's radius, w unused
	depth: vec4<f32>,
	// x the cascade's side (texels), y its column in the atlas, zw unused
	region: vec4<f32>,
}

@group(0) @binding(0) var<uniform> blur: SunBlur;
@group(0) @binding(1) var sun_depth: texture_depth_2d;
@group(0) @binding(2) var sun_rows: texture_2d<f32>;

// The warped moments of a depth (0..1 over the depth pass's range): relative to the cascade's
// sphere, -1 at its sunward side and +1 at the far one; casters in front of the sphere flatten onto
// its sunward side (Studio's "pancaking")
fn warped_moments(depth: f32) -> vec4<f32> {
	let z = clamp((depth * blur.depth.x - blur.depth.y) / blur.depth.z, -1.0, 1.0);
	let positive = exp(blur.params.z * z);
	let negative = -exp(-blur.params.z * z);
	return vec4<f32>(positive, positive * positive, negative, negative * negative);
}

fn gaussian(offset: i32) -> f32 {
	let sigma = blur.params.x;
	return exp(-0.5 * f32(offset * offset) / (sigma * sigma));
}

@fragment
fn fs_sun_moments(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let texel = vec2<i32>(floor(input.clip.xy));
	let last = i32(blur.region.x) - 1;
	let taps = i32(blur.params.y);
	var sum = vec4<f32>(0.0);
	var weights = 0.0;
	for (var offset = -taps; offset <= taps; offset++) {
		let weight = gaussian(offset);
		let depth = textureLoad(sun_depth, vec2<i32>(clamp(texel.x + offset, 0, last), texel.y), 0);
		sum += weight * warped_moments(depth);
		weights += weight;
	}
	return sum / weights;
}

@fragment
fn fs_sun_blur(input: FullscreenOutput) -> @location(0) vec4<f32> {
	// drawn into the cascade's column of the atlas
	let texel = vec2<i32>(floor(input.clip.xy)) - vec2<i32>(i32(blur.region.y), 0);
	let last = i32(blur.region.x) - 1;
	let taps = i32(blur.params.y);
	var sum = vec4<f32>(0.0);
	var weights = 0.0;
	for (var offset = -taps; offset <= taps; offset++) {
		let weight = gaussian(offset);
		sum += weight * textureLoad(sun_rows, vec2<i32>(texel.x, clamp(texel.y + offset, 0, last)), 0);
		weights += weight;
	}
	return sum / weights;
}
