// The fullscreen triangle shared by the sky and resolve passes.

struct FullscreenOutput {
	@builtin(position) clip: vec4<f32>,
	@location(0) ndc: vec2<f32>,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> FullscreenOutput {
	let x = f32(i32(index & 1u) * 4 - 1);
	let y = f32(i32(index & 2u) * 2 - 1);
	var output: FullscreenOutput;
	output.clip = vec4<f32>(x, y, 0.0, 1.0);
	output.ndc = vec2<f32>(x, y);
	return output;
}
