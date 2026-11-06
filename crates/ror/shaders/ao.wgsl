// Studio's ambient occlusion (notes/render.md "Ambient occlusion"): screen-space, a disk of a fixed
// screen radius around each opaque pixel C with face normal n; each sample's surface point P counts as
// if t studs lower than it is over the pixel's plane, t growing with the pixel's view depth (ao-depth,
// ao-dolly, ao-size, ao-relief: shallow features lose their occlusion with distance and in narrow
// fields of view, where the disk spans little depth), v = P - n t - C:
// A = max(0, 1 - sigma mean(max(0, v^.n - bias) (1 - |P - C|^2 / r^2)^2))^k, faded out with the view
// depth from ~120 to ~140 studs (ao-fade), then a blur among neighbours within a tolerance of the
// pixel's view depth that grows as its square (ao-relief, ao-thin). Its input is the opaque parts'
// prepass taken with 4 samples a pixel and resolved to the farthest covered one: a slot a fifth of a
// pixel wide still darkens (ao-thin).

struct AoParams {
	// xyz camera position, w the depth tolerance's factor (studs per stud^power)
	eye: vec4<f32>,
	// xyz camera forward, w the depth tolerance's power of the view depth
	forward: vec4<f32>,
	// x disk radius in AO pixels, y sigma, z contrast k, w bias
	params: vec4<f32>,
	// x range r (studs), y sample count, z the blur's tolerance (studs at 50 studs), w its half size
	// (AO pixels)
	extra: vec4<f32>,
	// x the fade's depth, y its width (studs), z the blur tolerance's power of the view depth, w its
	// measure (0: off the pixel's plane, 1: the view depth's difference)
	fade: vec4<f32>,
	// the view's rotation and projection inverted, the eye at the origin (the resolve's positions)
	unprojection: mat4x4<f32>,
}

// each pixel's surface depth (the prepass's, the farthest covered sample's with several; 0: none),
// its world position made from it where read (`surface`)
@group(0) @binding(0) var depths: texture_2d<f32>;
@group(0) @binding(1) var normals: texture_2d<f32>;
@group(0) @binding(2) var<uniform> ao: AoParams;
@group(0) @binding(3) var occlusion: texture_2d<f32>;
// the multisampled prepass (fs_ao_resolve)
@group(0) @binding(5) var depth_samples: texture_depth_multisampled_2d;
@group(0) @binding(6) var normal_samples: texture_multisampled_2d<f32>;
// each pixel's view depth as fs_ao found it (EMPTY where no surface is), for the blur
@group(0) @binding(7) var view_depths: texture_2d<f32>;

const EMPTY: f32 = -1.0;

fn view_depth(p: vec3<f32>) -> f32 {
	return dot(p - ao.eye.xyz, ao.forward.xyz);
}

// The surface at a pixel: its world position (w 1), or w 0 where there is none. Made from its depth
// wherever it is read, as the resolve made the positions it once wrote (16 bytes a texel, read 17
// times a pixel: 4 bytes now, the disk 0.59 -> 0.31 ms a 1080p frame): the same
// operations on the same values, so the same position.
fn surface(pixel: vec2<i32>) -> vec4<f32> {
	let depth = textureLoad(depths, pixel, 0).x;
	if (depth <= 0.0) {
		return vec4<f32>(0.0);
	}
	let size = vec2<f32>(textureDimensions(depths));
	let clip = vec2<f32>(pixel) + 0.5;
	let ndc = vec2<f32>((clip.x / size.x) * 2.0 - 1.0, 1.0 - (clip.y / size.y) * 2.0);
	let h = ao.unprojection * vec4<f32>(ndc, depth, 1.0);
	return vec4<f32>(ao.eye.xyz + h.xyz / h.w, 1.0);
}

// The occlusion, and the pixel's view depth (a surface's is above 0) the blur compares instead of
// reading 16-byte positions 81 times a pixel
struct AoOutput {
	@location(0) occlusion: vec4<f32>,
	@location(1) depth: vec4<f32>,
}

@fragment
fn fs_ao(input: FullscreenOutput) -> AoOutput {
	var output: AoOutput;
	output.occlusion = vec4<f32>(1.0);
	output.depth = vec4<f32>(EMPTY, 0.0, 0.0, 0.0);
	let pixel = vec2<i32>(input.clip.xy);
	let size = vec2<i32>(textureDimensions(depths));
	let centre = surface(pixel);
	if (centre.w < 0.5) {
		return output;
	}
	let depth = view_depth(centre.xyz);
	output.depth.x = depth;
	// the fade with the view depth: none left beyond ~150 studs
	let fade = 1.0 / (1.0 + exp((depth - ao.fade.x) / ao.fade.y));
	if (fade < 0.002) {
		return output;
	}
	let n = textureLoad(normals, pixel, 0).xyz;
	// the samples held the depth tolerance below the pixel's plane
	let lower = n * (ao.eye.w * pow(max(depth, 1e-3), ao.forward.w));
	// a golden-angle spiral, area-uniform, turned per pixel (interleaved gradient noise); the blur
	// averages the turns
	let turn = fract(52.9829189 * fract(0.06711056 * f32(pixel.x) + 0.00583715 * f32(pixel.y))) * 6.2831853;
	let count = i32(ao.extra.y);
	let range2 = ao.extra.x * ao.extra.x;
	var sum = 0.0;
	for (var i = 0; i < count; i++) {
		let radius = sqrt((f32(i) + 0.5) / f32(count)) * ao.params.x;
		let angle = f32(i) * 2.3999632 + turn;
		let q = pixel + vec2<i32>(round(vec2<f32>(cos(angle), sin(angle)) * radius));
		if (any(q < vec2<i32>(0)) || any(q >= size)) {
			continue;
		}
		let p = surface(q);
		if (p.w < 0.5) {
			continue;
		}
		let w = p.xyz - centre.xyz;
		let v = w - lower;
		let falloff = max(0.0, 1.0 - dot(w, w) / range2);
		sum += max(0.0, dot(v, n) * inverseSqrt(max(dot(v, v), 1e-12)) - ao.params.w) * falloff * falloff;
	}
	let a = pow(max(0.0, 1.0 - ao.params.y * sum / f32(count)), ao.params.z);
	output.occlusion = vec4<f32>(1.0 - fade * (1.0 - a), 0.0, 0.0, 1.0);
	return output;
}

@fragment
fn fs_ao_blur(input: FullscreenOutput) -> @location(0) vec4<f32> {
	let pixel = vec2<i32>(input.clip.xy);
	let size = vec2<i32>(textureDimensions(depths));
	let depth = textureLoad(view_depths, pixel, 0).x;
	if (depth < 0.0) {
		return vec4<f32>(1.0);
	}
	// neighbours on the pixel's own surface: off its view depth (or its plane) by less than the
	// tolerance, which grows with the view depth: a step of 1-2 studs blurs over at 80 studs, not at 15
	let tolerance = ao.extra.z * pow(max(depth, 1e-3) / 50.0, ao.fade.z);
	var sum = 0.0;
	var weight = 0.0;
	let reach = i32(ao.extra.w);
	if (ao.fade.w > 0.5) {
		for (var dy = -reach; dy <= reach; dy++) {
			for (var dx = -reach; dx <= reach; dx++) {
				let q = clamp(pixel + vec2<i32>(dx, dy), vec2<i32>(0), size - 1);
				let d = textureLoad(view_depths, q, 0).x;
				if (d < 0.0 || abs(d - depth) > tolerance) {
					continue;
				}
				sum += textureLoad(occlusion, q, 0).r;
				weight += 1.0;
			}
		}
		return vec4<f32>(sum / max(weight, 1.0), 0.0, 0.0, 1.0);
	}
	// off the pixel's plane (`ROR_AO` blur_depth=0)
	let centre = surface(pixel);
	let n = textureLoad(normals, pixel, 0).xyz;
	for (var dy = -reach; dy <= reach; dy++) {
		for (var dx = -reach; dx <= reach; dx++) {
			let q = clamp(pixel + vec2<i32>(dx, dy), vec2<i32>(0), size - 1);
			let p = surface(q);
			if (p.w < 0.5) {
				continue;
			}
			if (abs(dot(p.xyz - centre.xyz, n)) > tolerance) {
				continue;
			}
			sum += textureLoad(occlusion, q, 0).r;
			weight += 1.0;
		}
	}
	return vec4<f32>(sum / max(weight, 1.0), 0.0, 0.0, 1.0);
}

// The blur again as a compute pass, the same arithmetic in the same order: each 16 x 16 group reads its
// pixels' view depths and occlusion and a 4-pixel border round them once into workgroup memory (576
// texels each), where `fs_ao_blur` read 81 of each from the textures for every pixel (0.37 of a 1440p
// frame's 2.0 GPU ms). Its 32-bit result goes to the 8-bit occlusion through `fs_ao_blurred`, as
// `fs_ao_blur`'s did: the same conversion, so the same bytes. For blur reaches up to BLUR_BORDER.
@group(0) @binding(8) var blurred: texture_storage_2d<r32float, write>;

const BLUR_TILE: i32 = 16;
const BLUR_BORDER: i32 = 4;
const BLUR_SPAN: i32 = 24;
// each texel's view depth and occlusion side by side: one read a tap
var<workgroup> tile: array<vec2<f32>, 576>;

@compute @workgroup_size(16, 16)
fn cs_ao_blur(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>, @builtin(local_invocation_index) index: u32) {
	blur_tile(group.xy, local, index);
}

// the blur of the 16 x 16 pixels of tile `at` (its place in tiles)
fn blur_tile(at: vec2<u32>, local: vec3<u32>, index: u32) {
	let size = vec2<i32>(textureDimensions(view_depths));
	let origin = vec2<i32>(at) * BLUR_TILE - BLUR_BORDER;
	for (var i = i32(index); i < BLUR_SPAN * BLUR_SPAN; i += BLUR_TILE * BLUR_TILE) {
		let q = clamp(origin + vec2<i32>(i % BLUR_SPAN, i / BLUR_SPAN), vec2<i32>(0), size - 1);
		tile[i] = vec2<f32>(textureLoad(view_depths, q, 0).x, textureLoad(occlusion, q, 0).r);
	}
	workgroupBarrier();
	let pixel = vec2<i32>(at) * BLUR_TILE + vec2<i32>(local.xy);
	if (any(pixel >= size)) {
		return;
	}
	let centre = vec2<i32>(local.xy) + BLUR_BORDER;
	let depth = tile[centre.y * BLUR_SPAN + centre.x].x;
	if (depth < 0.0) {
		textureStore(blurred, pixel, vec4<f32>(1.0));
		return;
	}
	let tolerance = ao.extra.z * pow(max(depth, 1e-3) / 50.0, ao.fade.z);
	var sum = 0.0;
	var weight = 0.0;
	let reach = i32(ao.extra.w);
	for (var dy = -reach; dy <= reach; dy++) {
		for (var dx = -reach; dx <= reach; dx++) {
			let texel = tile[(centre.y + dy) * BLUR_SPAN + centre.x + dx];
			if (texel.x < 0.0 || abs(texel.x - depth) > tolerance) {
				continue;
			}
			sum += texel.y;
			weight += 1.0;
		}
	}
	textureStore(blurred, pixel, vec4<f32>(sum / max(weight, 1.0), 0.0, 0.0, 1.0));
}

// The compute blur's result (bound as `occlusion`) written to the 8-bit occlusion
@fragment
fn fs_ao_blurred(input: FullscreenOutput) -> @location(0) vec4<f32> {
	return vec4<f32>(textureLoad(occlusion, vec2<i32>(input.clip.xy), 0).x, 0.0, 0.0, 1.0);
}

// The multisampled prepass resolved to one sample per pixel: the farthest covered sample's depth and
// normal; a pixel no sample covers stays empty (depth 0)
struct Resolved {
	@location(0) depth: vec4<f32>,
	@location(1) normal: vec4<f32>,
}

@fragment
fn fs_ao_resolve(input: FullscreenOutput) -> Resolved {
	let pixel = vec2<i32>(input.clip.xy);
	var resolved: Resolved;
	resolved.depth = vec4<f32>(0.0);
	resolved.normal = vec4<f32>(0.0);
	// reversed depth: 0 uncovered, smaller further
	var chosen = 2.0;
	for (var s = 0; s < i32(textureNumSamples(depth_samples)); s++) {
		let d = textureLoad(depth_samples, pixel, s);
		if (d > 0.0 && d < chosen) {
			chosen = d;
			resolved.normal = vec4<f32>(textureLoad(normal_samples, pixel, s).xyz, 1.0);
		}
	}
	if (chosen > 1.5) {
		return resolved;
	}
	resolved.depth = vec4<f32>(chosen, 0.0, 0.0, 0.0);
	return resolved;
}

// A clip's occlusion kept from frame to frame while its camera holds still (render::ao::AoHistory). A
// pixel's occlusion is a function of the AO's parameters, the surface depths within the disk's reach
// and its own normal, then of the occlusion and view depths within the blur's: where none of those
// changed, the kept occlusion is this frame's. Tiles of 16 x 16 AO pixels whose depths or normals
// changed bit for bit (`cs_ao_changed`, against the copies `cs_ao_keep` keeps) are listed with every
// tile within the blur's reach of them and the disk's reach of those (`cs_ao_tiles`); the disk and the
// blur run on those alone (`vs_ao_tile` places `fs_ao`'s and `fs_ao_blurred`'s quads, `cs_ao_blur_list`
// takes the blur's tiles from the list), the same arithmetic on the same values.

const HISTORY_TILE: u32 = 16u;
// where the disk's list of tiles starts in `tile_lists` (words)
const DISK_LIST: u32 = 64u;

// the last kept frame's surface depths and normals
@group(0) @binding(9) var kept_depths: texture_2d<f32>;
@group(0) @binding(10) var kept_normals: texture_2d<f32>;
// one word a tile: whether its depths or normals changed
@group(0) @binding(11) var<storage, read_write> tile_changed: array<u32>;
var<workgroup> group_changed: atomic<u32>;

@compute @workgroup_size(16, 16)
fn cs_ao_changed(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>, @builtin(local_invocation_index) index: u32, @builtin(num_workgroups) groups: vec3<u32>) {
	if (index == 0u) {
		atomicStore(&group_changed, 0u);
	}
	workgroupBarrier();
	let size = vec2<u32>(textureDimensions(depths));
	let pixel = group.xy * HISTORY_TILE + local.xy;
	if (all(pixel < size)) {
		let at = vec2<i32>(pixel);
		let depth = bitcast<u32>(textureLoad(depths, at, 0).x) != bitcast<u32>(textureLoad(kept_depths, at, 0).x);
		let normal = any(bitcast<vec4<u32>>(textureLoad(normals, at, 0)) != bitcast<vec4<u32>>(textureLoad(kept_normals, at, 0)));
		if (depth || normal) {
			atomicOr(&group_changed, 1u);
		}
	}
	workgroupBarrier();
	if (index == 0u) {
		tile_changed[group.y * groups.x + group.x] = atomicLoad(&group_changed);
	}
}

// this frame's depths and normals kept for the next: the changed tiles' (`keep_all` in `history.x`: all)
struct History {
	// x 1: keep every tile; y the tiles across; z the disk's reach and w the blur's, in tiles
	params: vec4<u32>,
	// the AO targets' size in pixels
	size: vec4<f32>,
	// x where the blur's list starts in `tile_lists` (a word, aligned for its binding), y the tiles
	lists: vec4<u32>,
}
@group(0) @binding(12) var<uniform> history: History;
@group(0) @binding(13) var keep_depths: texture_storage_2d<r32float, write>;
@group(0) @binding(14) var keep_normals: texture_storage_2d<rgba16float, write>;
@group(0) @binding(15) var<storage, read> changed_tiles: array<u32>;

@compute @workgroup_size(16, 16)
fn cs_ao_keep(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
	if (history.params.x == 0u && changed_tiles[group.y * history.params.y + group.x] == 0u) {
		return;
	}
	let size = vec2<u32>(textureDimensions(depths));
	let pixel = group.xy * HISTORY_TILE + local.xy;
	if (any(pixel >= size)) {
		return;
	}
	let at = vec2<i32>(pixel);
	textureStore(keep_depths, at, textureLoad(depths, at, 0));
	textureStore(keep_normals, at, textureLoad(normals, at, 0));
}

// the indirect arguments ([0, 4) the disk's quads, [4, 8) the blur copy's, [8, 11) the blur's groups)
// and the lists of tiles (x | y << 16): the disk's from word 64, the blur's and the blur copy's from
// `history.lists.x` (each where a storage binding may start)
@group(0) @binding(16) var<storage, read_write> tile_lists: array<atomic<u32>>;

@compute @workgroup_size(64)
fn cs_ao_tiles(@builtin(global_invocation_id) id: vec3<u32>) {
	let across = history.params.y;
	let count = history.lists.y;
	let down = count / across;
	if (id.x == 0u) {
		atomicStore(&tile_lists[0], 6u);
		atomicStore(&tile_lists[2], 0u);
		atomicStore(&tile_lists[3], 0u);
		atomicStore(&tile_lists[4], 6u);
		atomicStore(&tile_lists[6], 0u);
		atomicStore(&tile_lists[7], 0u);
		atomicStore(&tile_lists[9], 1u);
		atomicStore(&tile_lists[10], 1u);
	}
	if (id.x >= count) {
		return;
	}
	let tile = vec2<i32>(i32(id.x % across), i32(id.x / across));
	let disk = i32(history.params.z);
	let blur = i32(history.params.w);
	// the nearest changed tile, in tiles each way (the larger of the two), up to the disk's reach
	var nearest = disk + 1;
	for (var dy = -disk; dy <= disk; dy++) {
		for (var dx = -disk; dx <= disk; dx++) {
			let other = tile + vec2<i32>(dx, dy);
			if (any(other < vec2<i32>(0)) || other.x >= i32(across) || other.y >= i32(down)) {
				continue;
			}
			if (changed_tiles[u32(other.y) * across + u32(other.x)] != 0u) {
				nearest = min(nearest, max(abs(dx), abs(dy)));
			}
		}
	}
	let packed = u32(tile.x) | (u32(tile.y) << 16u);
	if (nearest <= disk) {
		let slot = atomicAdd(&tile_lists[1], 1u);
		atomicStore(&tile_lists[DISK_LIST + slot], packed);
	}
	if (nearest <= blur) {
		let slot = atomicAdd(&tile_lists[5], 1u);
		atomicAdd(&tile_lists[8], 1u);
		atomicStore(&tile_lists[history.lists.x + slot], packed);
	}
}

// a tile's quad (two triangles over its 16 x 16 pixels; the target's edge clips the last row and column)
@vertex
fn vs_ao_tile(@builtin(vertex_index) index: u32, @location(0) tile: u32) -> FullscreenOutput {
	let corner = vec2<u32>(select(0u, 1u, index == 1u || index == 4u || index == 5u), select(0u, 1u, index == 2u || index == 3u || index == 5u));
	let pixel = vec2<f32>((vec2<u32>(tile & 0xffffu, tile >> 16u) + corner) * HISTORY_TILE);
	let x = pixel.x / history.size.x * 2.0 - 1.0;
	let y = 1.0 - pixel.y / history.size.y * 2.0;
	var output: FullscreenOutput;
	output.clip = vec4<f32>(x, y, 0.0, 1.0);
	output.ndc = vec2<f32>(x, y);
	return output;
}

// `cs_ao_blur` over the listed tiles: the n-th group blurs the n-th tile of the blur's list
@group(0) @binding(17) var<storage, read> blur_tiles: array<u32>;

@compute @workgroup_size(16, 16)
fn cs_ao_blur_list(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>, @builtin(local_invocation_index) index: u32) {
	let packed = blur_tiles[group.x];
	blur_tile(vec2<u32>(packed & 0xffffu, packed >> 16u), local, index);
}
