// Studio's built-in bloom (notes/render.md "Bloom"), as measured on lines and dots at every phase of
// the pixel grid: the exposed scene (each sample clamped before the MSAA resolve, mesh.wgsl) clamped
// again, then a chain of half-resolution levels down and back up. Going down, each level texel weighs
// the 4 x 4 finer texels around its centre, the outer ring more than the inner and each the less the
// brighter it is; the first level keeps only its brightest channel's excess over the threshold. Going
// up, each level adds the coarser one, upsampled bilinearly and blurred by a tent. Every level is
// stored as Studio stores it, in R11G11B10 floats. The resolve samples the first level bilinearly.
// The chain runs on a working grid 1116 pixels tall (the capture viewport's), so the halo keeps its
// size relative to the image.

struct BloomParams {
	// x exposure multiplier, y threshold, z cap, w 1: keep only the excess over the threshold
	source: vec4<f32>,
	// x, y render pixels per working pixel (fs_extract's footprint), z the threshold's soft knee,
	// w how much fs_down lowers a texel's weight with its brightness
	footprint: vec4<f32>,
	// x tent radius in texels, y direction (0 horizontal, 1 vertical), z weight of the added level
	// (fs_blur_add) or of the upsampled one (fs_up); w (fs_down) the weight of the outer two of the
	// four finer texels per axis
	blur: vec4<f32>,
}

@group(0) @binding(0) var input: texture_2d<f32>;
@group(0) @binding(1) var<uniform> bloom: BloomParams;
// fs_blur_add: the level the blurred one is added to
@group(0) @binding(2) var addend: texture_2d<f32>;

// what Studio's bloom textures keep of a texel: R11G11B10 floats, rounded towards zero (6 mantissa
// bits for red and green, 5 for blue), each level written once on the way down (fs_down) and once on
// the way up (fs_blur_add). Around white sources Studio's halo has blue / green 0.97 1-3 px out and
// 0.96 24-48 px out, as this gives (bloom-size, bloom-kernel, bloom-lines)
fn stored(color: vec3<f32>) -> vec4<f32> {
	let bits = bitcast<vec3<u32>>(max(color, vec3<f32>(0.0)));
	return vec4<f32>(bitcast<vec3<f32>>(bits & vec3<u32>(0xfffe0000u, 0xfffe0000u, 0xfffc0000u)), 1.0);
}

@fragment
fn fs_extract(input_position: FullscreenOutput) -> @location(0) vec4<f32> {
	// the working pixel's footprint on the render, its pixels weighted by how much of them it covers,
	// averaged in the scene target's √x as Studio's MSAA resolve averages its samples
	let start = floor(input_position.clip.xy) * bloom.footprint.xy;
	let end = start + bloom.footprint.xy;
	let first = vec2<i32>(floor(start));
	let last = vec2<i32>(ceil(end)) - 1;
	let size = vec2<i32>(textureDimensions(input));
	var sum = vec4<f32>(0.0);
	var total = 0.0;
	for (var y = first.y; y <= last.y; y++) {
		let weight_y = min(f32(y + 1), end.y) - max(f32(y), start.y);
		for (var x = first.x; x <= last.x; x++) {
			let weight = weight_y * (min(f32(x + 1), end.x) - max(f32(x), start.x));
			sum += textureLoad(input, clamp(vec2<i32>(x, y), vec2<i32>(0), size - 1), 0) * weight;
			total += weight;
		}
	}
	let mean = sum / total;
	// the scene target holds premultiplied √x, light that covers nothing included: squared, what shows
	// over black (an opaque background's alpha is 1), so over a transparent background what glows is
	// what glows over black (notes/render.md "Transparent backgrounds")
	return vec4<f32>(min(mean.rgb * mean.rgb * bloom.source.x, vec3<f32>(bloom.source.z)), 1.0);
}

@fragment
fn fs_down(input_position: FullscreenOutput) -> @location(0) vec4<f32> {
	// the next level: texel j from finer texels 2j - 1 .. 2j + 2 per axis (clamped to the edge), each
	// weighed down by its brightness, so a source covering part of the footprint counts less than its
	// mean (a weighted Karis average; uniform areas keep their value)
	let origin = vec2<i32>(input_position.clip.xy) * 2 - 1;
	let size = vec2<i32>(textureDimensions(input));
	// the per-axis weights of the 4 finer texels around the texel's centre, outer ones first (render/mod.rs
	// BLOOM_TAPS: the first two levels' outer-heavy, the deeper ones' (1, 3, 3, 1) / 8)
	let outer = bloom.blur.w;
	var taps = array<f32, 4>(outer, 0.5 - outer, 0.5 - outer, outer);
	var color = vec3<f32>(0.0);
	var total = 0.0;
	for (var y = 0; y < 4; y++) {
		for (var x = 0; x < 4; x++) {
			let position = clamp(origin + vec2<i32>(x, y), vec2<i32>(0), size - 1);
			let texel = textureLoad(input, position, 0).rgb;
			let weight = taps[x] * taps[y] / (1.0 + bloom.footprint.w * max(texel.r, max(texel.g, texel.b)));
			color += texel * weight;
			total += weight;
		}
	}
	color /= total;
	if (bloom.source.w > 0.5) {
		// the brightest channel's excess over the threshold, with a soft knee, the colour's ratios kept
		// (an orange source's halo keeps the clamped colour's ratios: bloom-colors)
		let peak = max(color.r, max(color.g, color.b));
		let knee = bloom.footprint.z;
		var excess = max(peak - bloom.source.y, 0.0);
		if (knee > 0.0) {
			let soft = clamp(peak - bloom.source.y + knee, 0.0, 2.0 * knee);
			excess = max(excess, soft * soft / (4.0 * knee));
		}
		color = select(vec3<f32>(0.0), color * (excess / peak), peak > 1e-5);
	}
	return stored(color);
}

@fragment
fn fs_up(input_position: FullscreenOutput) -> @location(0) vec4<f32> {
	// the coarser level bilinearly at this texel's centre (clamped to the edge), weighted
	let size = vec2<i32>(textureDimensions(input));
	let position = input_position.clip.xy * 0.5 - 0.5;
	let base = floor(position);
	let f = position - base;
	let corner = vec2<i32>(base);
	let a = textureLoad(input, clamp(corner, vec2<i32>(0), size - 1), 0).rgb;
	let b = textureLoad(input, clamp(corner + vec2<i32>(1, 0), vec2<i32>(0), size - 1), 0).rgb;
	let c = textureLoad(input, clamp(corner + vec2<i32>(0, 1), vec2<i32>(0), size - 1), 0).rgb;
	let d = textureLoad(input, clamp(corner + vec2<i32>(1, 1), vec2<i32>(0), size - 1), 0).rgb;
	return vec4<f32>(mix(mix(a, b, f.x), mix(c, d, f.x), f.y) * bloom.blur.z, 1.0);
}

// a tent of radius bloom.blur.x texels along one axis, the edge texels repeated beyond it
fn tent(center: vec2<i32>) -> vec3<f32> {
	let size = vec2<i32>(textureDimensions(input));
	let radius = bloom.blur.x;
	let reach = i32(ceil(radius)) - 1;
	let step = select(vec2<i32>(1, 0), vec2<i32>(0, 1), bloom.blur.y > 0.5);
	var sum = vec3<f32>(0.0);
	var total = 0.0;
	for (var i = -reach; i <= reach; i++) {
		let weight = 1.0 - abs(f32(i)) / radius;
		sum += textureLoad(input, clamp(center + step * i, vec2<i32>(0), size - 1), 0).rgb * weight;
		total += weight;
	}
	return sum / total;
}

@fragment
fn fs_blur(input_position: FullscreenOutput) -> @location(0) vec4<f32> {
	return vec4<f32>(tent(vec2<i32>(input_position.clip.xy)), 1.0);
}

@fragment
fn fs_blur_add(input_position: FullscreenOutput) -> @location(0) vec4<f32> {
	// the blurred upsampled levels plus this level's own texels, weighted
	let center = vec2<i32>(input_position.clip.xy);
	return stored(tent(center) + textureLoad(addend, center, 0).rgb * bloom.blur.z);
}
