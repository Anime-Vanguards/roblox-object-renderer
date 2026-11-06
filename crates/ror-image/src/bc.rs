//! BCn block decoding (BC1-BC5, BC7) to RGBA8 and exact alpha scans.
//!
//! The arithmetic (565 expansion, interpolation rounding) follows bcdec
//! (<https://github.com/iOrange/bcdec>), so the decoders are bit-exact against it; the tests compare
//! against `bcdec_rs` on random and real blocks. Hardware decoders differ from this by at most one
//! step in some interpolated values.
//!
//! Output conventions for [`crate::to_rgba8`]: BC4 → (r, 0, 0, 255) and BC5 → (r, g, 0, 255), the
//! values a GPU returns when sampling the block format.

use crate::PixelFormat;

/// Decodes one level (or one 2D surface) of block data into tightly packed RGBA8.
///
/// `data` must hold `ceil(width / 4) * ceil(height / 4)` blocks; `out` must be `width * height * 4`
/// bytes. `bc1_alpha` selects BC1 RGBA semantics (punch-through texels transparent black) instead of
/// BC1 RGB (punch-through texels opaque black). Returns false when the sizes do not match.
pub(crate) fn decode_surface(format: PixelFormat, data: &[u8], width: u32, height: u32, out: &mut [u8], bc1_alpha: bool) -> bool {
	let Some(block_bytes) = format.block_bytes() else {
		return false;
	};
	let (w, h) = (width as usize, height as usize);
	let (bw, bh) = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
	if data.len() < bw * bh * block_bytes || out.len() != w * h * 4 {
		return false;
	}

	let mut texels = [[0u8; 4]; 16];
	for by in 0..bh {
		for bx in 0..bw {
			let offset = (by * bw + bx) * block_bytes;
			let block = &data[offset..offset + block_bytes];
			decode_block(format, block, &mut texels, bc1_alpha);

			// Copy the in-bounds part of the 4x4 block (edge blocks may be partial)
			let x0 = bx * 4;
			let y0 = by * 4;
			let cols = (w - x0).min(4);
			let rows = (h - y0).min(4);
			for row in 0..rows {
				let dst = ((y0 + row) * w + x0) * 4;
				for col in 0..cols {
					out[dst + col * 4..dst + col * 4 + 4].copy_from_slice(&texels[row * 4 + col]);
				}
			}
		}
	}
	true
}

/// Decodes one block into 16 RGBA texels (row-major). `block` has the format's block size.
#[inline]
pub(crate) fn decode_block(format: PixelFormat, block: &[u8], texels: &mut [[u8; 4]; 16], bc1_alpha: bool) {
	match format {
		PixelFormat::Bc1 => color_block(block, texels, false, bc1_alpha),
		PixelFormat::Bc2 => {
			color_block(&block[8..16], texels, true, false);
			for (i, texel) in texels.iter_mut().enumerate() {
				let nibble = (block[i / 2] >> ((i & 1) * 4)) & 0x0f;
				texel[3] = nibble * 17;
			}
		}
		PixelFormat::Bc3 => {
			color_block(&block[8..16], texels, true, false);
			let palette = bc3_alpha_palette(block[0], block[1]);
			let mut indices = read_u64(block) >> 16;
			for texel in texels.iter_mut() {
				texel[3] = palette[(indices & 7) as usize];
				indices >>= 3;
			}
		}
		PixelFormat::Bc4 => {
			let mut values = [0u8; 16];
			bc4_channel(block, &mut values);
			for (texel, v) in texels.iter_mut().zip(values) {
				*texel = [v, 0, 0, 255];
			}
		}
		PixelFormat::Bc5 => {
			let mut red = [0u8; 16];
			let mut green = [0u8; 16];
			bc4_channel(&block[0..8], &mut red);
			bc4_channel(&block[8..16], &mut green);
			for i in 0..16 {
				texels[i] = [red[i], green[i], 0, 255];
			}
		}
		PixelFormat::Bc7 => bc7_block(block, texels),
		PixelFormat::Rgba8 => {}
	}
}

#[inline]
fn read_u64(block: &[u8]) -> u64 {
	let mut bytes = [0u8; 8];
	bytes.copy_from_slice(&block[0..8]);
	u64::from_le_bytes(bytes)
}

/// BC1 colour palette: entries 2 and 3 interpolated (four-colour mode) or the average and black
/// (three-colour mode, only when `c0 <= c1` and `force_four` is false).
#[inline]
fn color_palette(block: &[u8], force_four: bool, punch_alpha: bool) -> [[u8; 4]; 4] {
	let c0 = u16::from_le_bytes([block[0], block[1]]) as u32;
	let c1 = u16::from_le_bytes([block[2], block[3]]) as u32;
	let (r0, g0, b0) = ((c0 >> 11) & 0x1f, (c0 >> 5) & 0x3f, c0 & 0x1f);
	let (r1, g1, b1) = ((c1 >> 11) & 0x1f, (c1 >> 5) & 0x3f, c1 & 0x1f);

	let expand = |r: u32, g: u32, b: u32| [((r * 527 + 23) >> 6) as u8, ((g * 259 + 33) >> 6) as u8, ((b * 527 + 23) >> 6) as u8, 255];
	let mut palette = [expand(r0, g0, b0), expand(r1, g1, b1), [0; 4], [0; 4]];

	if c0 > c1 || force_four {
		palette[2] = [
			(((2 * r0 + r1) * 351 + 61) >> 7) as u8,
			(((2 * g0 + g1) * 2763 + 1039) >> 11) as u8,
			(((2 * b0 + b1) * 351 + 61) >> 7) as u8,
			255,
		];
		palette[3] = [
			(((r0 + 2 * r1) * 351 + 61) >> 7) as u8,
			(((g0 + 2 * g1) * 2763 + 1039) >> 11) as u8,
			(((b0 + 2 * b1) * 351 + 61) >> 7) as u8,
			255,
		];
	} else {
		palette[2] = [
			(((r0 + r1) * 1053 + 125) >> 8) as u8,
			(((g0 + g1) * 4145 + 1019) >> 11) as u8,
			(((b0 + b1) * 1053 + 125) >> 8) as u8,
			255,
		];
		palette[3] = if punch_alpha { [0, 0, 0, 0] } else { [0, 0, 0, 255] };
	}
	palette
}

#[inline]
fn color_block(block: &[u8], texels: &mut [[u8; 4]; 16], force_four: bool, punch_alpha: bool) {
	let palette = color_palette(block, force_four, punch_alpha);
	let mut indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
	for texel in texels.iter_mut() {
		*texel = palette[(indices & 3) as usize];
		indices >>= 2;
	}
}

#[inline]
fn bc3_alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
	let (a0, a1) = (a0 as u32, a1 as u32);
	if a0 > a1 {
		[
			a0 as u8,
			a1 as u8,
			((6 * a0 + a1 + 1) / 7) as u8,
			((5 * a0 + 2 * a1 + 1) / 7) as u8,
			((4 * a0 + 3 * a1 + 1) / 7) as u8,
			((3 * a0 + 4 * a1 + 1) / 7) as u8,
			((2 * a0 + 5 * a1 + 1) / 7) as u8,
			((a0 + 6 * a1 + 1) / 7) as u8,
		]
	} else {
		[
			a0 as u8,
			a1 as u8,
			((4 * a0 + a1 + 1) / 5) as u8,
			((3 * a0 + 2 * a1 + 1) / 5) as u8,
			((2 * a0 + 3 * a1 + 1) / 5) as u8,
			((a0 + 4 * a1 + 1) / 5) as u8,
			0,
			255,
		]
	}
}

#[inline]
fn bc4_palette(a0: u8, a1: u8) -> [u8; 8] {
	const W6: [u32; 6] = [9363, 18724, 28086, 37450, 46812, 56173];
	const W4: [u32; 4] = [13107, 26215, 39321, 52429];
	let (a0, a1) = (a0 as u32, a1 as u32);
	let mix = |wa: u32, wb: u32| ((wa * a0 + wb * a1 + 32768) >> 16) as u8;
	if a0 > a1 {
		[a0 as u8, a1 as u8, mix(W6[5], W6[0]), mix(W6[4], W6[1]), mix(W6[3], W6[2]), mix(W6[2], W6[3]), mix(W6[1], W6[4]), mix(W6[0], W6[5])]
	} else {
		[a0 as u8, a1 as u8, mix(W4[3], W4[0]), mix(W4[2], W4[1]), mix(W4[1], W4[2]), mix(W4[0], W4[3]), 0, 255]
	}
}

#[inline]
fn bc4_channel(block: &[u8], values: &mut [u8; 16]) {
	let palette = bc4_palette(block[0], block[1]);
	let mut indices = read_u64(block) >> 16;
	for v in values.iter_mut() {
		*v = palette[(indices & 7) as usize];
		indices >>= 3;
	}
}

// BC7

/// BC7 two-subset partitions: 2 bits per texel (texel 0 in the low bits).
const BC7_P2: [u32; 64] = [
	0x50505050, 0x40404040, 0x54545454, 0x54505040, 0x50404000, 0x55545450, 0x55545040, 0x54504000,
	0x50400000, 0x55555450, 0x55544000, 0x54400000, 0x55555440, 0x55550000, 0x55555500, 0x55000000,
	0x55150100, 0x00004054, 0x15010000, 0x00405054, 0x00004050, 0x15050100, 0x05010000, 0x40505054,
	0x00404050, 0x05010100, 0x14141414, 0x05141450, 0x01155440, 0x00555500, 0x15014054, 0x05414150,
	0x44444444, 0x55005500, 0x11441144, 0x05055050, 0x05500550, 0x11114444, 0x41144114, 0x44111144,
	0x15055054, 0x01055040, 0x05041050, 0x05455150, 0x14414114, 0x50050550, 0x41411414, 0x00141400,
	0x00041504, 0x00105410, 0x10541000, 0x04150400, 0x50410514, 0x41051450, 0x05415014, 0x14054150,
	0x41050514, 0x41505014, 0x40011554, 0x54150140, 0x50505500, 0x00555050, 0x15151010, 0x54540404,
];

/// Anchor texel of subset 1 for each two-subset partition.
const BC7_ANCHOR2: [u8; 64] = [
	15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
	15, 2, 8, 2, 2, 8, 8, 15, 2, 8, 2, 2, 8, 8, 2, 2,
	15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6,
	6, 2, 6, 8, 15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
];

/// BC7 three-subset partitions: 2 bits per texel (texel 0 in the low bits).
const BC7_P3: [u32; 64] = [
	0xaa685050, 0x6a5a5040, 0x5a5a4200, 0x5450a0a8, 0xa5a50000, 0xa0a05050, 0x5555a0a0, 0x5a5a5050,
	0xaa550000, 0xaa555500, 0xaaaa5500, 0x90909090, 0x94949494, 0xa4a4a4a4, 0xa9a59450, 0x2a0a4250,
	0xa5945040, 0x0a425054, 0xa5a5a500, 0x55a0a0a0, 0xa8a85454, 0x6a6a4040, 0xa4a45000, 0x1a1a0500,
	0x0050a4a4, 0xaaa59090, 0x14696914, 0x69691400, 0xa08585a0, 0xaa821414, 0x50a4a450, 0x6a5a0200,
	0xa9a58000, 0x5090a0a8, 0xa8a09050, 0x24242424, 0x00aa5500, 0x24924924, 0x24499224, 0x50a50a50,
	0x500aa550, 0xaaaa4444, 0x66660000, 0xa5a0a5a0, 0x50a050a0, 0x69286928, 0x44aaaa44, 0x66666600,
	0xaa444444, 0x54a854a8, 0x95809580, 0x96969600, 0xa85454a8, 0x80959580, 0xaa141414, 0x96960000,
	0xaaaa1414, 0xa05050a0, 0xa0a5a5a0, 0x96000000, 0x40804080, 0xa9a8a9a8, 0xaaaaaa44, 0x2a4a5254,
];

/// Anchor texels of subsets 1 and 2 for each three-subset partition.
const BC7_ANCHOR3: [[u8; 2]; 64] = [
	[3, 15], [3, 8], [15, 8], [15, 3], [8, 15], [3, 15], [15, 3], [15, 8],
	[8, 15], [8, 15], [6, 15], [6, 15], [6, 15], [5, 15], [3, 15], [3, 8],
	[3, 15], [3, 8], [8, 15], [15, 3], [3, 15], [3, 8], [6, 15], [10, 8],
	[5, 3], [8, 15], [8, 6], [6, 10], [8, 15], [5, 15], [15, 10], [15, 8],
	[8, 15], [15, 3], [3, 15], [5, 10], [6, 10], [10, 8], [8, 9], [15, 10],
	[15, 6], [3, 15], [15, 8], [5, 15], [15, 3], [15, 6], [15, 6], [15, 8],
	[3, 15], [15, 3], [5, 15], [5, 15], [5, 15], [8, 15], [5, 15], [10, 15],
	[5, 15], [10, 15], [8, 15], [13, 15], [15, 3], [12, 15], [3, 15], [3, 8],
];

const BC7_WEIGHTS2: [u32; 4] = [0, 21, 43, 64];
const BC7_WEIGHTS3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
const BC7_WEIGHTS4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

struct Bc7Mode {
	subsets: usize,
	partition_bits: u32,
	rotation_bits: u32,
	index_selection_bits: u32,
	color_bits: u32,
	alpha_bits: u32,
	/// One P-bit per endpoint
	endpoint_pbits: bool,
	/// One P-bit per subset (shared by its two endpoints)
	shared_pbits: bool,
	index_bits: u32,
	index2_bits: u32,
}

const BC7_MODES: [Bc7Mode; 8] = [
	Bc7Mode { subsets: 3, partition_bits: 4, rotation_bits: 0, index_selection_bits: 0, color_bits: 4, alpha_bits: 0, endpoint_pbits: true, shared_pbits: false, index_bits: 3, index2_bits: 0 },
	Bc7Mode { subsets: 2, partition_bits: 6, rotation_bits: 0, index_selection_bits: 0, color_bits: 6, alpha_bits: 0, endpoint_pbits: false, shared_pbits: true, index_bits: 3, index2_bits: 0 },
	Bc7Mode { subsets: 3, partition_bits: 6, rotation_bits: 0, index_selection_bits: 0, color_bits: 5, alpha_bits: 0, endpoint_pbits: false, shared_pbits: false, index_bits: 2, index2_bits: 0 },
	Bc7Mode { subsets: 2, partition_bits: 6, rotation_bits: 0, index_selection_bits: 0, color_bits: 7, alpha_bits: 0, endpoint_pbits: true, shared_pbits: false, index_bits: 2, index2_bits: 0 },
	Bc7Mode { subsets: 1, partition_bits: 0, rotation_bits: 2, index_selection_bits: 1, color_bits: 5, alpha_bits: 6, endpoint_pbits: false, shared_pbits: false, index_bits: 2, index2_bits: 3 },
	Bc7Mode { subsets: 1, partition_bits: 0, rotation_bits: 2, index_selection_bits: 0, color_bits: 7, alpha_bits: 8, endpoint_pbits: false, shared_pbits: false, index_bits: 2, index2_bits: 2 },
	Bc7Mode { subsets: 1, partition_bits: 0, rotation_bits: 0, index_selection_bits: 0, color_bits: 7, alpha_bits: 7, endpoint_pbits: true, shared_pbits: false, index_bits: 4, index2_bits: 0 },
	Bc7Mode { subsets: 2, partition_bits: 6, rotation_bits: 0, index_selection_bits: 0, color_bits: 5, alpha_bits: 5, endpoint_pbits: true, shared_pbits: false, index_bits: 2, index2_bits: 0 },
];

struct Bits {
	value: u128,
	pos: u32,
}

impl Bits {
	#[inline]
	fn read(&mut self, count: u32) -> u32 {
		if count == 0 || self.pos >= 128 {
			return 0;
		}
		let v = (self.value >> self.pos) as u32 & ((1u32 << count) - 1);
		self.pos += count;
		v
	}
}

#[inline]
fn bc7_weights(bits: u32) -> &'static [u32] {
	match bits {
		2 => &BC7_WEIGHTS2,
		3 => &BC7_WEIGHTS3,
		_ => &BC7_WEIGHTS4,
	}
}

#[inline]
fn bc7_interpolate(e0: u32, e1: u32, weight: u32) -> u8 {
	((e0 * (64 - weight) + e1 * weight + 32) >> 6) as u8
}

/// BC7 mode of a block (number of zero bits before the first set bit), `None` for the reserved
/// all-zero mode byte.
#[inline]
pub(crate) fn bc7_mode(block: &[u8]) -> Option<usize> {
	let first = block[0];
	if first == 0 {
		None
	} else {
		Some(first.trailing_zeros() as usize)
	}
}

fn bc7_block(block: &[u8], texels: &mut [[u8; 4]; 16]) {
	let mut raw = [0u8; 16];
	raw.copy_from_slice(&block[0..16]);
	let mut bits = Bits { value: u128::from_le_bytes(raw), pos: 0 };

	let Some(mode_index) = bc7_mode(block) else {
		// Reserved mode: transparent black
		*texels = [[0; 4]; 16];
		return;
	};
	let mode = &BC7_MODES[mode_index];
	bits.pos = mode_index as u32 + 1;

	let partition = bits.read(mode.partition_bits) as usize;
	let rotation = bits.read(mode.rotation_bits);
	let index_selection = bits.read(mode.index_selection_bits);

	// Endpoints: R for every endpoint, then G, then B, then A
	let endpoint_count = mode.subsets * 2;
	let mut endpoints = [[0u32; 4]; 6];
	for channel in 0..3 {
		for endpoint in endpoints.iter_mut().take(endpoint_count) {
			endpoint[channel] = bits.read(mode.color_bits);
		}
	}
	if mode.alpha_bits > 0 {
		for endpoint in endpoints.iter_mut().take(endpoint_count) {
			endpoint[3] = bits.read(mode.alpha_bits);
		}
	}

	// P-bits extend every component by one low bit
	let has_pbit = mode.endpoint_pbits || mode.shared_pbits;
	if mode.endpoint_pbits {
		for endpoint in endpoints.iter_mut().take(endpoint_count) {
			let p = bits.read(1);
			for c in endpoint.iter_mut() {
				*c = (*c << 1) | p;
			}
		}
	} else if mode.shared_pbits {
		for subset in 0..mode.subsets {
			let p = bits.read(1);
			for endpoint in &mut endpoints[subset * 2..subset * 2 + 2] {
				for c in endpoint.iter_mut() {
					*c = (*c << 1) | p;
				}
			}
		}
	}

	// Expand to 8 bits by replicating the high bits into the low bits
	let color_precision = mode.color_bits + has_pbit as u32;
	let alpha_precision = mode.alpha_bits + has_pbit as u32;
	for endpoint in endpoints.iter_mut().take(endpoint_count) {
		for c in endpoint.iter_mut().take(3) {
			*c <<= 8 - color_precision;
			*c |= *c >> color_precision;
		}
		if mode.alpha_bits > 0 {
			endpoint[3] <<= 8 - alpha_precision;
			endpoint[3] |= endpoint[3] >> alpha_precision;
		} else {
			endpoint[3] = 255;
		}
	}

	// Subset of each texel and the anchor texels (their index has one bit less)
	let (subset_of, anchors): (u32, [usize; 3]) = match mode.subsets {
		2 => (BC7_P2[partition], [0, BC7_ANCHOR2[partition] as usize, usize::MAX]),
		3 => (BC7_P3[partition], [0, BC7_ANCHOR3[partition][0] as usize, BC7_ANCHOR3[partition][1] as usize]),
		_ => (0, [0, usize::MAX, usize::MAX]),
	};

	let mut indices = [0u32; 16];
	for (texel, index) in indices.iter_mut().enumerate() {
		let anchor = anchors.contains(&texel);
		*index = bits.read(mode.index_bits - anchor as u32);
	}
	let mut indices2 = [0u32; 16];
	if mode.index2_bits > 0 {
		for (texel, index) in indices2.iter_mut().enumerate() {
			*index = bits.read(mode.index2_bits - (texel == 0) as u32);
		}
	}

	let weights = bc7_weights(mode.index_bits);
	let weights2 = bc7_weights(mode.index2_bits);
	for texel in 0..16 {
		let subset = ((subset_of >> (texel * 2)) & 3) as usize;
		let e0 = endpoints[subset * 2];
		let e1 = endpoints[subset * 2 + 1];

		let (color_weight, alpha_weight) = if mode.index2_bits == 0 {
			let w = weights[indices[texel] as usize];
			(w, w)
		} else if index_selection == 0 {
			(weights[indices[texel] as usize], weights2[indices2[texel] as usize])
		} else {
			(weights2[indices2[texel] as usize], weights[indices[texel] as usize])
		};

		let mut rgba = [
			bc7_interpolate(e0[0], e1[0], color_weight),
			bc7_interpolate(e0[1], e1[1], color_weight),
			bc7_interpolate(e0[2], e1[2], color_weight),
			bc7_interpolate(e0[3], e1[3], alpha_weight),
		];
		match rotation {
			1 => rgba.swap(0, 3),
			2 => rgba.swap(1, 3),
			3 => rgba.swap(2, 3),
			_ => {}
		}
		texels[texel] = rgba;
	}
}

// Alpha scans

/// True when any texel of a level chain is less than fully opaque; every 2D surface of every level
/// (all layers and slices, see [`crate::Image::levels`]) is checked. Texels outside the image in
/// edge blocks are ignored (encoders fill them arbitrarily).
pub(crate) fn levels_transparent(format: PixelFormat, levels: &[Vec<u8>], size: (u32, u32, u32), bc1_alpha: bool) -> bool {
	levels.iter().enumerate().any(|(level, data)| {
		let (w, h, _) = crate::layout::level_dims(size.0, size.1, size.2, level as u32);
		let Some(surface) = crate::layout::block_surface_bytes(format, w, h) else {
			return false;
		};
		data.chunks_exact(surface).any(|s| surface_transparent(format, s, w, h, bc1_alpha))
	})
}

/// True when any in-bounds texel of one 2D surface of blocks is less than fully opaque.
/// `bc1_alpha` selects BC1 RGBA semantics (with BC1 RGB every texel is opaque). Exact: decodes
/// only what decides the answer.
pub(crate) fn surface_transparent(format: PixelFormat, data: &[u8], width: u32, height: u32, bc1_alpha: bool) -> bool {
	let Some(block_bytes) = format.block_bytes() else {
		return false;
	};
	let check: fn(&[u8], u16) -> bool = match format {
		PixelFormat::Bc1 if bc1_alpha => bc1_block_transparent,
		PixelFormat::Bc2 => bc2_block_transparent,
		PixelFormat::Bc3 => bc3_block_transparent,
		PixelFormat::Bc7 => bc7_block_transparent,
		_ => return false,
	};
	let (w, h) = (width as usize, height as usize);
	let (bw, bh) = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
	if data.len() < bw * bh * block_bytes {
		return false;
	}
	for by in 0..bh {
		let rows = (h - (by * 4).min(h)).min(4);
		for bx in 0..bw {
			let cols = (w - (bx * 4).min(w)).min(4);
			let mask = if rows == 4 && cols == 4 { 0xffff } else { texel_mask(cols, rows) };
			let offset = (by * bw + bx) * block_bytes;
			if check(&data[offset..offset + block_bytes], mask) {
				return true;
			}
		}
	}
	false
}

/// Bit `y * 4 + x` set for the texels of the top-left `cols` x `rows` corner of a block.
#[inline]
fn texel_mask(cols: usize, rows: usize) -> u16 {
	let row_bits = (1u16 << cols) - 1;
	(0..rows).fold(0, |m, y| m | (row_bits << (y * 4)))
}

#[inline]
fn bc1_block_transparent(block: &[u8], mask: u16) -> bool {
	let c0 = u16::from_le_bytes([block[0], block[1]]);
	let c1 = u16::from_le_bytes([block[2], block[3]]);
	if c0 > c1 {
		return false;
	}
	// Three-colour mode: index 3 is transparent black
	let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
	(0..16).any(|i| mask >> i & 1 != 0 && (indices >> (i * 2)) & 3 == 3)
}

#[inline]
fn bc2_block_transparent(block: &[u8], mask: u16) -> bool {
	(0..16).any(|i| mask >> i & 1 != 0 && (block[i / 2] >> ((i & 1) * 4)) & 0x0f != 0x0f)
}

#[inline]
fn bc3_block_transparent(block: &[u8], mask: u16) -> bool {
	let (a0, a1) = (block[0], block[1]);
	let indices = read_u64(block) >> 16;
	if a0 == 255 && a1 == 255 {
		// Six-value mode with both endpoints opaque: only index 6 (zero) is transparent
		return (0..16).any(|i| mask >> i & 1 != 0 && (indices >> (i * 3)) & 7 == 6);
	}
	let palette = bc3_alpha_palette(a0, a1);
	(0..16).any(|i| mask >> i & 1 != 0 && palette[((indices >> (i * 3)) & 7) as usize] != 255)
}

#[inline]
fn bc7_block_transparent(block: &[u8], mask: u16) -> bool {
	match bc7_mode(block) {
		// Modes 0-3 store no alpha
		Some(0..=3) => false,
		Some(_) | None => {
			// Mode 4-7, or the reserved mode (transparent black)
			let mut texels = [[0u8; 4]; 16];
			bc7_block(block, &mut texels);
			texels.iter().enumerate().any(|(i, t)| mask >> i & 1 != 0 && t[3] != 255)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Small deterministic generator (xorshift64*) so the tests need no extra dependency.
	struct Rng(u64);

	impl Rng {
		fn next(&mut self) -> u64 {
			self.0 ^= self.0 >> 12;
			self.0 ^= self.0 << 25;
			self.0 ^= self.0 >> 27;
			self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
		}

		fn fill(&mut self, out: &mut [u8]) {
			for chunk in out.chunks_mut(8) {
				let v = self.next().to_le_bytes();
				chunk.copy_from_slice(&v[..chunk.len()]);
			}
		}
	}

	fn reference(format: PixelFormat, block: &[u8]) -> [[u8; 4]; 16] {
		let mut out = [0u8; 64];
		match format {
			PixelFormat::Bc1 => bcdec_rs::bc1(block, &mut out, 16),
			PixelFormat::Bc2 => bcdec_rs::bc2(block, &mut out, 16),
			PixelFormat::Bc3 => bcdec_rs::bc3(block, &mut out, 16),
			PixelFormat::Bc7 => bcdec_rs::bc7(block, &mut out, 16),
			PixelFormat::Bc4 => {
				let mut r = [0u8; 16];
				bcdec_rs::bc4(block, &mut r, 4, false);
				for i in 0..16 {
					out[i * 4..i * 4 + 4].copy_from_slice(&[r[i], 0, 0, 255]);
				}
			}
			PixelFormat::Bc5 => {
				let mut rg = [0u8; 32];
				bcdec_rs::bc5(block, &mut rg, 8, false);
				for i in 0..16 {
					out[i * 4..i * 4 + 4].copy_from_slice(&[rg[i * 2], rg[i * 2 + 1], 0, 255]);
				}
			}
			PixelFormat::Rgba8 => {}
		}
		let mut texels = [[0u8; 4]; 16];
		for i in 0..16 {
			texels[i].copy_from_slice(&out[i * 4..i * 4 + 4]);
		}
		texels
	}

	fn check_random(format: PixelFormat, count: usize, seed: u64) {
		let size = format.block_bytes().unwrap();
		let mut rng = Rng(seed);
		let mut block = vec![0u8; size];
		for n in 0..count {
			rng.fill(&mut block);
			// Bias BC7 towards every mode (random bytes pick mode 0 half the time)
			if format == PixelFormat::Bc7 {
				let mode = (n % 9) as u32;
				block[0] = if mode == 8 { 0 } else { (block[0] & !((2u8 << mode) - 1)) | (1 << mode) };
			}
			let mut ours = [[0u8; 4]; 16];
			decode_block(format, &block, &mut ours, true);
			let theirs = reference(format, &block);
			assert_eq!(ours, theirs, "{format:?} block {block:02x?}");

			// The alpha scan agrees with the decoded texels, also for partial edge blocks
			let transparent = theirs.iter().any(|t| t[3] != 255);
			assert_eq!(surface_transparent(format, &block, 4, 4, true), transparent, "{format:?} alpha scan {block:02x?}");
			let (cols, rows) = (1 + n % 3, 1 + (n / 3) % 3);
			let corner = (0..rows).any(|y| (0..cols).any(|x| theirs[y * 4 + x][3] != 255));
			assert_eq!(surface_transparent(format, &block, cols as u32, rows as u32, true), corner, "{format:?} {cols}x{rows} alpha scan");
		}
	}

	#[test]
	fn bc1_matches_bcdec() {
		check_random(PixelFormat::Bc1, 200_000, 1);
	}

	#[test]
	fn bc2_matches_bcdec() {
		check_random(PixelFormat::Bc2, 200_000, 2);
	}

	#[test]
	fn bc3_matches_bcdec() {
		check_random(PixelFormat::Bc3, 200_000, 3);
	}

	#[test]
	fn bc4_bc5_match_bcdec() {
		check_random(PixelFormat::Bc4, 200_000, 4);
		check_random(PixelFormat::Bc5, 200_000, 5);
	}

	#[test]
	fn bc7_matches_bcdec() {
		check_random(PixelFormat::Bc7, 900_000, 7);
	}

	#[test]
	fn bc1_rgb_ignores_punch_through() {
		// c0 <= c1 selects three-colour mode; all indices 3
		let block = [0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
		let mut texels = [[0u8; 4]; 16];
		decode_block(PixelFormat::Bc1, &block, &mut texels, false);
		assert!(texels.iter().all(|t| *t == [0, 0, 0, 255]));
		decode_block(PixelFormat::Bc1, &block, &mut texels, true);
		assert!(texels.iter().all(|t| *t == [0, 0, 0, 0]));
		assert!(surface_transparent(PixelFormat::Bc1, &block, 4, 4, true));
		assert!(!surface_transparent(PixelFormat::Bc1, &block, 4, 4, false));
	}

	#[test]
	fn partial_edge_blocks() {
		// 5x3 image: 2x1 blocks, the second block contributes one column
		let mut rng = Rng(9);
		let mut data = vec![0u8; 2 * 16];
		rng.fill(&mut data);
		let mut out = vec![0u8; 5 * 3 * 4];
		assert!(decode_surface(PixelFormat::Bc3, &data, 5, 3, &mut out, true));
		let left = reference(PixelFormat::Bc3, &data[0..16]);
		let right = reference(PixelFormat::Bc3, &data[16..32]);
		for y in 0..3 {
			for x in 0..5 {
				let expected = if x < 4 { left[y * 4 + x] } else { right[y * 4 + (x - 4)] };
				assert_eq!(out[(y * 5 + x) * 4..(y * 5 + x) * 4 + 4], expected);
			}
		}

		// Wrong sizes are rejected, not panicked on
		assert!(!decode_surface(PixelFormat::Bc3, &data[..16], 5, 3, &mut out, true));
		assert!(!decode_surface(PixelFormat::Bc3, &data, 5, 3, &mut out[..8], true));
	}
}
