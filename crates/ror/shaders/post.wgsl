// Image-space post effects (notes/render.md "Post effects"): BlurEffect on the shown image,
// DepthOfFieldEffect on the scene's radiance before the bloom. Their blurs run at reduced resolution
// with few taps a pixel and are sampled back bilinearly, so no pass grows long on large renders.

struct PostParams {
	// x Gaussian sigma in input texels, y direction (0 horizontal, 1 vertical), z kernel radius
	blur: vec4<f32>,
	// depth of field: x FocusDistance, y InFocusRadius, z FarIntensity, w NearIntensity
	focus: vec4<f32>,
	// depth of field: x the camera's near plane (reversed-Z depth → view depth), y the largest blur's
	// sigma, z the middle level's sigma, w the widest level's sigma (render pixels)
	lens: vec4<f32>,
	// x, y, w the middle, widest and near levels' texel sizes in render pixels (x alone: the blurred
	// image's when presenting); z the box factor of fs_post_radiance
	texels: vec4<f32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> post: PostParams;
@group(0) @binding(2) var post_sampler: sampler;
@group(0) @binding(3) var level_mid: texture_2d<f32>;
@group(0) @binding(4) var level_wide: texture_2d<f32>;
@group(0) @binding(5) var scene_depth: texture_depth_2d;
@group(0) @binding(6) var level_near: texture_2d<f32>;

// A 2 x 2 box down to the next level, the frame's edge repeated
@fragment
fn fs_post_down(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let size = vec2<i32>(textureDimensions(source));
	let origin = vec2<i32>(input.clip.xy) * 2;
	var sum = vec4<f32>(0.0);
	for (var y = 0; y < 2; y++) {
		for (var x = 0; x < 2; x++) {
			sum += textureLoad(source, min(origin + vec2<i32>(x, y), size - 1), 0);
		}
	}
	return sum * 0.25;
}

// One direction of a Gaussian, the frame's edge repeated
@fragment
fn fs_post_blur(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let center = vec2<i32>(input.clip.xy);
	let size = vec2<i32>(textureDimensions(source));
	let sigma = post.blur.x;
	let radius = i32(post.blur.z);
	let step = select(vec2<i32>(1, 0), vec2<i32>(0, 1), post.blur.y > 0.5);
	var sum = vec4<f32>(0.0);
	var total = 0.0;
	for (var i = -radius; i <= radius; i++) {
		let weight = exp(-f32(i * i) / (2.0 * sigma * sigma));
		sum += textureLoad(source, clamp(center + step * i, vec2<i32>(0), size - 1), 0) * weight;
		total += weight;
	}
	return sum / total;
}

// The blurred image back at the output's size, bilinearly
@fragment
fn fs_post_present(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let uv = input.clip.xy / (vec2<f32>(textureDimensions(source)) * post.texels.x);
	return textureSampleLevel(source, post_sampler, uv, 0.0);
}

// Depth of field (post-depth: checkers 5-160 studs away under ten settings; the blur is of the
// radiance, before the tone curve: a Gaussian of the radiance fits Studio within 2-5 of 255 where
// one of the shown values misses by 10-21). Past FocusDistance + InFocusRadius (or nearer than
// FocusDistance - InFocusRadius) the blur grows with that share of the view depth, c = (depth - far
// edge) / depth: sigma = 6.6 px (at 1116 px) · smoothstep(0.08, 1, c) · √FarIntensity (c 0.25 / 0.5 /
// 0.75 / 0.94 give 0.9 / 2.4 / 5.4 / 6.6 px at intensity 0.75, the fit 0.6 / 2.9 / 5.4 / 6.5; 0.25 half
// that; post-depth-halo: the halo an in-focus tile leaves grows from FarIntensity 1 to 0.5 to 0.25 as
// the middle layer takes more of the background, as √ has it). In front, c = (near edge - depth) /
// depth, faster: smoothstep(0, 0.8, c) · min(1, 2 · NearIntensity) (c 0.5 / 0.75 / 1+ give 3.8 / 5.9 /
// 6.2). Returns (far, near) sigmas as shares of the largest.
fn dof_sigmas(pixel: vec2<i32>) -> vec2<f32> {
	let depth_value = textureLoad(scene_depth, pixel, 0);
	let depth = select(1e9, post.lens.x / depth_value, depth_value > 0.0);
	let far_edge = post.focus.x + post.focus.y;
	let near_edge = post.focus.x - post.focus.y;
	let far = saturate((depth - far_edge) / max(depth, 1e-3));
	let near = select(0.0, saturate((near_edge - depth) / max(depth, 1e-3)), near_edge > 0.0);
	return vec2<f32>(smoothstep(0.08, 1.0, far) * sqrt(post.focus.z), smoothstep(0.0, 0.8, near) * min(1.0, 2.0 * post.focus.w));
}

fn premultiplied_radiance(texel: vec4<f32>) -> vec3<f32> {
	// the scene target holds premultiplied √x
	let straight = select(vec3<f32>(0.0), texel.rgb / texel.a, texel.a > 1e-5);
	return straight * straight * texel.a;
}

// The blur is layered by how much each pixel blurs, each layer spreading by its own sigma (as a
// lens spreads every point by its own circle; a gather with the background's wider blur pulls the
// sharper foreground into a wide halo): pixels behind focus belong to the sharp image, the middle
// and the widest layer by their sigma (between two layers in proportion), those in front of it to the
// near layer. The widest and near layers hold radiance times membership and the membership; the
// middle one the plain radiance of every pixel, sharp ones too, and the membership: where it lies over
// the background beside an in-focus outline it carries the outline's light out by its 3 px blur
// (post-depth-grid, -halo, a place: Studio's halo outside in-focus tiles, the same width whatever the
// background's blur, as strong as the middle layer's share of it; a black tile darkens a light
// background alike). They are laid back to front: the widest, the middle, the pixel's own sharp share,
// the near layer.
fn far_memberships(sigma: f32) -> vec3<f32> {
	// (sharp, middle, widest)
	if (sigma <= post.lens.z) {
		let t = sigma / post.lens.z;
		return vec3<f32>(1.0 - t, t, 0.0);
	}
	let t = saturate((sigma - post.lens.z) / max(post.lens.w - post.lens.z, 1e-3));
	return vec3<f32>(0.0, 1.0 - t, t);
}

fn level_colour(level: vec4<f32>, fallback: vec3<f32>) -> vec3<f32> {
	return select(fallback, level.rgb / level.a, level.a > 1e-4);
}

@fragment
fn fs_post_dof(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let pixel = vec2<i32>(input.clip.xy);
	let sharp = premultiplied_radiance(textureLoad(source, pixel, 0));
	let sigmas = dof_sigmas(pixel) * post.lens.y;
	let position = input.clip.xy;
	let mid = textureSampleLevel(level_mid, post_sampler, position / (vec2<f32>(textureDimensions(level_mid)) * post.texels.x), 0.0);
	let wide = textureSampleLevel(level_wide, post_sampler, position / (vec2<f32>(textureDimensions(level_wide)) * post.texels.y), 0.0);
	let near = textureSampleLevel(level_near, post_sampler, position / (vec2<f32>(textureDimensions(level_near)) * post.texels.w), 0.0);
	var color = level_colour(wide, sharp);
	color = mix(color, mid.rgb, saturate(mid.a));
	color = mix(color, sharp, far_memberships(sigmas.x).x);
	color = mix(color, level_colour(near, color), saturate(near.a * 2.0));
	return vec4<f32>(sqrt(max(color, vec3<f32>(0.0))), 1.0);
}

// What blends laid over the depth of field's image: the layer `level_mid` (its texels texels.x render
// pixels, at least 1) holds it premultiplied in √x over transparent black, as the scene would have
// taken it, read back bilinearly
@fragment
fn fs_post_over(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let pixel = vec2<i32>(input.clip.xy);
	let below = textureLoad(source, pixel, 0);
	let layer = textureSampleLevel(level_mid, post_sampler, input.clip.xy / (vec2<f32>(textureDimensions(level_mid)) * max(post.texels.x, 1.0)), 0.0);
	return vec4<f32>(layer.rgb + below.rgb * (1.0 - layer.a), layer.a + below.a * (1.0 - layer.a));
}

// The scene's radiance boxed down by an integer factor (texels.z render pixels a side) with each pixel's
// membership of the layer post.blur.w names (0 middle, 1 widest, 2 near): rgb the radiance (the widest
// and near layers' weighted by it), a the membership
@fragment
fn fs_post_radiance(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let factor = i32(post.texels.z);
	let size = vec2<i32>(textureDimensions(source));
	let origin = vec2<i32>(input.clip.xy) * factor;
	var sum = vec4<f32>(0.0);
	for (var y = 0; y < factor; y++) {
		for (var x = 0; x < factor; x++) {
			let pixel = min(origin + vec2<i32>(x, y), size - 1);
			let sigmas = dof_sigmas(pixel) * post.lens.y;
			let far = far_memberships(sigmas.x);
			var weight = select(far.y, far.z, post.blur.w > 0.5);
			if (post.blur.w > 1.5) {
				weight = saturate(sigmas.y / max(post.lens.y, 1e-3) * 8.0);
			}
			let radiance = premultiplied_radiance(textureLoad(source, pixel, 0));
			sum += vec4<f32>(select(radiance * weight, radiance, post.blur.w < 0.5), weight);
		}
	}
	return sum / f32(factor * factor);
}
