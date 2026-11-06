// Finished display RGB to limited-range BT.709. Each invocation owns one packed word;
// no two invocations write different bytes of the same storage-buffer word.
struct Settings {
	width: u32,
	height: u32,
	flatten: u32,
	padding: u32,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> settings: Settings;

fn rgb(position: vec2<u32>) -> vec3<f32> {
	let pixel = vec4<u32>(round(textureLoad(image, vec2<i32>(position), 0) * 255.0));
	if settings.flatten != 0u {
		// Preserve the integer alpha flattening used by the RGBA MP4 path.
		return vec3<f32>((pixel.rgb * pixel.a + vec3<u32>(127u)) / 255u);
	}
	return vec3<f32>(pixel.rgb);
}

fn luma(value: vec3<f32>) -> f32 {
	return dot(value, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn byte(value: f32) -> u32 {
	return u32(clamp(floor(value + 0.5), 0.0, 255.0));
}

fn chroma(pair: u32) -> u32 {
	if pair >= settings.width * settings.height / 4u {
		return 0u;
	}
	let position = vec2<u32>(pair % (settings.width / 2u), pair / (settings.width / 2u)) * 2u;
	let average = (rgb(position) + rgb(position + vec2<u32>(1u, 0u))
		+ rgb(position + vec2<u32>(0u, 1u)) + rgb(position + vec2<u32>(1u, 1u))) * 0.25;
	let y = luma(average);
	let u = byte(128.0 + (average.b - y) * (224.0 / (255.0 * 1.8556)));
	let v = byte(128.0 + (average.r - y) * (224.0 / (255.0 * 1.5748)));
	return u | (v << 8u);
}

@compute @workgroup_size(64)
fn convert(@builtin(global_invocation_id) invocation: vec3<u32>) {
	// Dispatch in rows of at most 256 workgroups, also supporting large output sizes.
	let word = invocation.x + invocation.y * 16384u;
	let pixels = settings.width * settings.height;
	if word >= (pixels * 3u / 2u + 3u) / 4u {
		return;
	}
	if word < pixels / 4u {
		var packed = 0u;
		for (var index = 0u; index < 4u; index++) {
			let pixel = word * 4u + index;
			let value = rgb(vec2<u32>(pixel % settings.width, pixel / settings.width));
			packed |= byte(16.0 + luma(value) * (219.0 / 255.0)) << (index * 8u);
		}
		output[word] = packed;
	} else {
		let pair = (word - pixels / 4u) * 2u;
		output[word] = chroma(pair) | (chroma(pair + 1u) << 16u);
	}
}
