// A window's world (ViewRequest::opaque_share, render::guis windows): the share of each pixel's samples
// an opaque surface covers, from the depth after the opaque parts (reversed: 0 where nothing is
// drawn). The GUI composite shows the frame's own background only in the rest: an opaque surface of
// vertex alpha 0 leaves a hole through it.

@group(0) @binding(0) var depth_single: texture_depth_2d;
@group(0) @binding(1) var depth_samples: texture_depth_multisampled_2d;

@fragment
fn fs_opaque_share(input: FullscreenOutput) -> @location(0) vec4<f32> {
	return vec4<f32>(select(0.0, 1.0, textureLoad(depth_single, vec2<i32>(input.clip.xy), 0) > 0.0));
}

@fragment
fn fs_opaque_share_samples(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let pixel = vec2<i32>(input.clip.xy);
	let count = i32(textureNumSamples(depth_samples));
	var covered = 0.0;
	for (var s = 0; s < count; s++) {
		covered += select(0.0, 1.0, textureLoad(depth_samples, pixel, s) > 0.0);
	}
	return vec4<f32>(covered / f32(count));
}
