// Legacy effects' layer (render::particles, notes/render.md "Legacy effects"): their light (rgb) and
// the transmittance of what is behind them (a), blended in linear light, over the scene's
// premultiplied √ radiance; the result in the scene's encoding.

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var layer: texture_2d<f32>;

@fragment
fn fs_legacy_composite(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let position = vec2<i32>(input.clip.xy);
	let below = textureLoad(scene, position, 0);
	let above = textureLoad(layer, position, 0);
	// the scene holds premultiplied √x, light that covers nothing included: squared, what it shows over
	// black (an opaque background's alpha is 1, a transparent one's the coverage); the layer's light
	// over that, and what either covers
	let radiance = above.rgb + above.a * below.rgb * below.rgb;
	let alpha = 1.0 - above.a * (1.0 - below.a);
	return vec4<f32>(sqrt(max(radiance, vec3<f32>(0.0))), alpha);
}
