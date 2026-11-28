//! ETC2 and EAC blocks (Khronos Data Format Specification, "ETC2 Compressed Texture Image Formats"),
//! decoded to RGBA8 on the CPU: desktop GPUs have no ETC2. Roblox serves ETC2 texture transcodes to
//! devices without BC formats (phones; notes/image.md "Mobile transcodes").
//!
//! - ETC2 RGB (vkFormat 147 / 148): the individual and differential modes ETC1 has, plus T, H and
//!   planar; 8 bytes a block.
//! - ETC2 RGBA (151 / 152): an EAC alpha block (8 bits) before the colour block; 16 bytes.
//! - ETC2 RGB with punch-through alpha (149 / 150): the differential bit is the opaque bit.
//! - EAC R11 (153) and RG11 (155): one or two 11-bit channels, rounded to 8 bits; 8 / 16 bytes.
//!
//! Texels are numbered down the columns (`x * 4 + y`) as the format stores them.

/// The kinds of ETC block this module decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Etc {
	Rgb,
	RgbPunchThrough,
	Rgba,
	R11,
	Rg11,
}

impl Etc {
	pub(crate) fn block_bytes(self) -> usize {
		match self {
			Etc::Rgb | Etc::RgbPunchThrough | Etc::R11 => 8,
			Etc::Rgba | Etc::Rg11 => 16,
		}
	}

	/// The format has no alpha channel: alpha reads as opaque.
	pub(crate) fn opaque(self) -> bool {
		!matches!(self, Etc::RgbPunchThrough | Etc::Rgba)
	}
}

/// ETC1's intensity modifiers by table, for pixel index (msb << 1) | lsb.
const MODIFIERS: [[i32; 4]; 8] = [[2, 8, -2, -8], [5, 17, -5, -17], [9, 29, -9, -29], [13, 42, -13, -42], [18, 60, -18, -60], [24, 80, -24, -80], [33, 106, -33, -106], [47, 183, -47, -183]];

/// T and H modes' distances.
const DISTANCES: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

/// EAC's modifiers by table, for a 3-bit index.
const EAC: [[i32; 8]; 16] = [
	[-3, -6, -9, -15, 2, 5, 8, 14],
	[-3, -7, -10, -13, 2, 6, 9, 12],
	[-2, -5, -8, -13, 1, 4, 7, 12],
	[-2, -4, -6, -13, 1, 3, 5, 12],
	[-3, -6, -8, -12, 2, 5, 7, 11],
	[-3, -7, -9, -11, 2, 6, 8, 10],
	[-4, -7, -8, -11, 3, 6, 7, 10],
	[-3, -5, -8, -11, 2, 4, 7, 10],
	[-2, -6, -8, -10, 1, 5, 7, 9],
	[-2, -5, -8, -10, 1, 4, 7, 9],
	[-2, -4, -8, -10, 1, 3, 7, 9],
	[-2, -5, -7, -10, 1, 4, 6, 9],
	[-3, -4, -7, -10, 2, 3, 6, 9],
	[-1, -2, -3, -10, 0, 1, 2, 9],
	[-4, -6, -8, -9, 3, 5, 7, 8],
	[-3, -5, -7, -9, 2, 4, 6, 8],
];

fn clamp8(value: i32) -> u8 {
	value.clamp(0, 255) as u8
}

fn extend4(value: u8) -> i32 {
	((value << 4) | value) as i32
}

fn extend5(value: i32) -> i32 {
	(value << 3) | (value >> 2)
}

fn extend6(value: u32) -> i32 {
	((value << 2) | (value >> 4)) as i32
}

fn extend7(value: u32) -> i32 {
	((value << 1) | (value >> 6)) as i32
}

/// A 3-bit two's complement difference.
fn delta(bits: u8) -> i32 {
	let bits = (bits & 7) as i32;
	if bits >= 4 { bits - 8 } else { bits }
}

/// Decodes an ETC2 RGB block into `out` (16 texels, RGBA8, row-major: `(y * 4 + x) * 4`). With
/// `punch_through` the differential bit says the block is opaque, and index 2 of a block that is not
/// is transparent black. Alpha is written 255 elsewhere.
pub(crate) fn decode_rgb(block: &[u8], punch_through: bool, out: &mut [u8; 64]) {
	let b = block;
	let differential = punch_through || b[3] & 2 != 0;
	let opaque = !punch_through || b[3] & 2 != 0;
	let indices = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
	let index = |x: usize, y: usize| -> usize {
		let bit = x * 4 + y;
		(((indices >> (bit + 16)) & 1) << 1 | ((indices >> bit) & 1)) as usize
	};
	let mut put = |x: usize, y: usize, rgb: [i32; 3], transparent: bool| {
		let at = (y * 4 + x) * 4;
		if transparent {
			out[at..at + 4].copy_from_slice(&[0, 0, 0, 0]);
		} else {
			out[at..at + 4].copy_from_slice(&[clamp8(rgb[0]), clamp8(rgb[1]), clamp8(rgb[2]), 255]);
		}
	};
	// the second differential colour; out of 0-31 in red, green or blue: T, H or planar mode
	let second = |byte: u8| (byte >> 3) as i32 + delta(byte);
	let (r2, g2, b2) = (second(b[0]), second(b[1]), second(b[2]));
	if !differential || (0..32).contains(&r2) && (0..32).contains(&g2) && (0..32).contains(&b2) {
		// individual or differential: two subblocks, each a base colour and an intensity table
		let colors: [[i32; 3]; 2] = if differential {
			let first = [b[0] >> 3, b[1] >> 3, b[2] >> 3].map(|c| extend5(c as i32));
			let second = [r2, g2, b2].map(extend5);
			[first, second]
		} else {
			[[b[0] >> 4, b[1] >> 4, b[2] >> 4].map(extend4), [b[0] & 15, b[1] & 15, b[2] & 15].map(extend4)]
		};
		let tables = [(b[3] >> 5) as usize, ((b[3] >> 2) & 7) as usize];
		let flipped = b[3] & 1 != 0;
		for x in 0..4 {
			for y in 0..4 {
				let sub = if flipped { usize::from(y >= 2) } else { usize::from(x >= 2) };
				let idx = index(x, y);
				// punch-through blocks that are not opaque: index 2 is transparent, 0 has no modifier
				let modifier = if !opaque && (idx == 0 || idx == 2) { 0 } else { MODIFIERS[tables[sub]][idx] };
				put(x, y, colors[sub].map(|c| c + modifier), !opaque && idx == 2);
			}
		}
	} else if !(0..32).contains(&r2) {
		// T mode
		let first = [((b[0] >> 3) & 3) << 2 | (b[0] & 3), b[1] >> 4, b[1] & 15].map(extend4);
		let second = [b[2] >> 4, b[2] & 15, b[3] >> 4].map(extend4);
		let distance = DISTANCES[(((b[3] >> 2) & 3) << 1 | (b[3] & 1)) as usize];
		let paint = [first, second.map(|c| c + distance), second, second.map(|c| c - distance)];
		for x in 0..4 {
			for y in 0..4 {
				let idx = index(x, y);
				put(x, y, paint[idx], !opaque && idx == 2);
			}
		}
	} else if !(0..32).contains(&g2) {
		// H mode
		let first = [(b[0] >> 3) & 15, (b[0] & 7) << 1 | (b[1] >> 4) & 1, (b[1] & 8) | (b[1] & 3) << 1 | b[2] >> 7].map(extend4);
		let second = [(b[2] >> 3) & 15, (b[2] & 7) << 1 | b[3] >> 7, (b[3] >> 3) & 15].map(extend4);
		let value = |c: [i32; 3]| (c[0] << 16) | (c[1] << 8) | c[2];
		let order = usize::from(value(first) >= value(second));
		let distance = DISTANCES[(b[3] & 4) as usize | ((b[3] & 1) << 1) as usize | order];
		let paint = [first.map(|c| c + distance), first.map(|c| c - distance), second.map(|c| c + distance), second.map(|c| c - distance)];
		for x in 0..4 {
			for y in 0..4 {
				let idx = index(x, y);
				put(x, y, paint[idx], !opaque && idx == 2);
			}
		}
	} else {
		// planar: three colours, the origin, the right edge and the bottom edge
		let w = u64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8]));
		let field = |high: u32, bits: u32| ((w >> (high + 1 - bits)) & ((1 << bits) - 1)) as u32;
		let origin = [extend6(field(62, 6)), extend7(field(56, 1) << 6 | field(54, 6)), extend6(field(48, 1) << 5 | field(44, 2) << 3 | field(41, 3))];
		let right = [extend6(field(38, 5) << 1 | field(32, 1)), extend7(field(31, 7)), extend6(field(24, 6))];
		let down = [extend6(field(18, 6)), extend7(field(12, 7)), extend6(field(5, 6))];
		for x in 0..4 {
			for y in 0..4 {
				let (xi, yi) = (x as i32, y as i32);
				let rgb = std::array::from_fn(|c| (xi * (right[c] - origin[c]) + yi * (down[c] - origin[c]) + 4 * origin[c] + 2) >> 2);
				put(x, y, rgb, false);
			}
		}
	}
}

/// An EAC block's 16 values: 8-bit alpha (`eleven` false), else 11-bit (0-2047).
fn eac(block: &[u8], eleven: bool) -> [i32; 16] {
	let base = block[0] as i32;
	let multiplier = (block[1] >> 4) as i32;
	let table = &EAC[(block[1] & 15) as usize];
	let bits = u64::from_be_bytes([0, 0, block[2], block[3], block[4], block[5], block[6], block[7]]);
	let mut out = [0i32; 16];
	for x in 0..4 {
		for y in 0..4 {
			let idx = ((bits >> (45 - 3 * (x * 4 + y))) & 7) as usize;
			out[y * 4 + x] = if eleven {
				let scale = if multiplier == 0 { 1 } else { multiplier * 8 };
				(base * 8 + 4 + table[idx] * scale).clamp(0, 2047)
			} else {
				(base + table[idx] * multiplier).clamp(0, 255)
			};
		}
	}
	out
}

/// An 11-bit EAC value rounded to 8 bits.
fn eleven_to_8(value: i32) -> u8 {
	((value * 255 + 1023) / 2047) as u8
}

/// Decodes one block of `kind` into 16 RGBA8 texels (row-major). EAC R11 reads (r, 0, 0, 255) and
/// RG11 (r, g, 0, 255), as a GPU samples them.
pub(crate) fn decode_block(kind: Etc, block: &[u8], out: &mut [u8; 64]) {
	match kind {
		Etc::Rgb => decode_rgb(block, false, out),
		Etc::RgbPunchThrough => decode_rgb(block, true, out),
		Etc::Rgba => {
			decode_rgb(&block[8..16], false, out);
			for (texel, alpha) in eac(&block[..8], false).into_iter().enumerate() {
				out[texel * 4 + 3] = alpha as u8;
			}
		}
		Etc::R11 | Etc::Rg11 => {
			let red = eac(&block[..8], true);
			let green = if kind == Etc::Rg11 { eac(&block[8..16], true) } else { [0; 16] };
			for texel in 0..16 {
				out[texel * 4..texel * 4 + 4].copy_from_slice(&[eleven_to_8(red[texel]), eleven_to_8(green[texel]), 0, 255]);
			}
		}
	}
}

/// A `width` x `height` surface of `kind` blocks as RGBA8 (`data` holds whole blocks, rows of
/// `ceil(width / 4)`; texels past the edge dropped).
pub(crate) fn to_rgba8(kind: Etc, data: &[u8], width: u32, height: u32) -> Vec<u8> {
	let (width, height) = (width as usize, height as usize);
	let blocks_x = width.div_ceil(4);
	let mut out = vec![0u8; width * height * 4];
	let mut texels = [0u8; 64];
	for (index, block) in data.chunks_exact(kind.block_bytes()).enumerate().take(blocks_x * height.div_ceil(4)) {
		let (bx, by) = ((index % blocks_x) * 4, (index / blocks_x) * 4);
		decode_block(kind, block, &mut texels);
		for y in 0..4.min(height - by) {
			let row = ((by + y) * width + bx) * 4;
			let count = 4.min(width - bx) * 4;
			out[row..row + count].copy_from_slice(&texels[y * 16..y * 16 + count]);
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decode(kind: Etc, block: &[u8]) -> [u8; 64] {
		let mut out = [0u8; 64];
		decode_block(kind, block, &mut out);
		out
	}

	fn texel(out: &[u8; 64], x: usize, y: usize) -> [u8; 4] {
		let at = (y * 4 + x) * 4;
		[out[at], out[at + 1], out[at + 2], out[at + 3]]
	}

	#[test]
	fn individual_mode_subblocks_and_modifiers() {
		// base colours 0x8 / 0x4 (136 / 68 grey), tables 0 and 7, not flipped: the left two columns
		// take the first subblock; indices all 0 (+2 / +47) but texel (3, 3): msb 1, lsb 1 (-183)
		let block = [0x84, 0x84, 0x84, 0x1c, 0x80, 0x00, 0x80, 0x00]; // byte 3: tables 0 and 7, individual, not flipped
		let out = decode(Etc::Rgb, &block);
		assert_eq!(texel(&out, 0, 0), [138, 138, 138, 255]);
		assert_eq!(texel(&out, 2, 0), [115, 115, 115, 255]);
		assert_eq!(texel(&out, 3, 3), [0, 0, 0, 255]);
	}

	#[test]
	fn differential_mode_and_flip() {
		// first colour 16 (5 bits: 132), difference -1 (second 15: 123), flipped: rows 2-3 take the second
		let block = [0x87, 0x87, 0x87, 0x03, 0, 0, 0, 0]; // byte 3: tables 0 and 0, differential, flipped
		let out = decode(Etc::Rgb, &block);
		assert_eq!(texel(&out, 3, 1), [134, 134, 134, 255]);
		assert_eq!(texel(&out, 0, 2), [125, 125, 125, 255]);
	}

	#[test]
	fn t_h_and_planar_modes_are_chosen_by_overflow() {
		// red 31 + 3 overflows: T mode. Colour 1 = (R1a R1b) G1 B1, colour 2 from bytes 2-3, distance 3
		let t = [0xfb, 0x00, 0xff, 0xf2, 0, 0, 0, 0]; // 0xfb: red 31 and difference +3
		assert_eq!(texel(&decode(Etc::Rgb, &t), 0, 0), [0xff, 0, 0, 255]);
		// red in range, green 31 + 3 overflows: H mode; all indices 0 = colour 1 + distance
		let h = [0x00, 0xfb, 0x00, 0x02, 0, 0, 0, 0]; // green 31 and +3; byte 3 differential
		let out = decode(Etc::Rgb, &h);
		assert_eq!(texel(&out, 1, 1), texel(&out, 2, 3));
		// red and green in range, blue 31 + 3 overflows: planar. Origin only (H = V = O): flat
		let mut planar = [0u8; 8];
		planar[2] = 0xfb;
		let out = decode(Etc::Rgb, &planar);
		assert_eq!(texel(&out, 0, 0)[3], 255);
	}

	#[test]
	fn eac_alpha_and_eleven_bits() {
		// base 128, multiplier 2, table 0; indices: texel (0, 0) 4 (+2), the others 0 (-3)
		let mut alpha = [128u8, 0x20, 0, 0, 0, 0, 0, 0];
		alpha[2] = 0x80; // texel (0, 0) index 4
		let mut block = [0u8; 16];
		block[..8].copy_from_slice(&alpha);
		block[8..].copy_from_slice(&[0x84, 0x84, 0x84, 0, 0, 0, 0, 0]);
		let out = decode(Etc::Rgba, &block);
		assert_eq!(texel(&out, 0, 0)[3], 132);
		assert_eq!(texel(&out, 1, 0)[3], 122);
		// R11: base 255, multiplier 0 (modifier x 1): 2044 + 2 = 2046 -> 255; -3 -> 2041 -> 254
		let out = decode(Etc::R11, &[255, 0x00, 0x80, 0, 0, 0, 0, 0]);
		assert_eq!(texel(&out, 0, 0), [255, 0, 0, 255]);
		assert_eq!(texel(&out, 0, 1), [254, 0, 0, 255]);
	}

	#[test]
	fn random_blocks_decode_as_an_independent_decoder_does() {
		// 1,024 xorshift blocks of each kind (random bits take every mode: individual, differential, T,
		// H, planar); the expected hashes are of texture2ddecoder's output for the same blocks, which
		// also matched this decoder texel for texel on 2.2 M texels of Roblox's ETC2 transcodes
		for (kind, expected) in [(Etc::Rgb, 0xc8ce_881e_a183_b23au64), (Etc::Rgba, 0xa20f_ef3b_45e7_a86a)] {
			let mut state = 0x2545_f491_4f6c_dd1du64;
			let data: Vec<u8> = (0..32 * 32 * kind.block_bytes())
				.map(|_| {
					state ^= state << 13;
					state ^= state >> 7;
					state ^= state << 17;
					state as u8
				})
				.collect();
			let texels = to_rgba8(kind, &data, 128, 128);
			let hash = texels.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3));
			assert_eq!(hash, expected, "{kind:?}");
		}
	}

	#[test]
	fn surfaces_drop_texels_past_the_edge() {
		let block = [0x88, 0x88, 0x88, 0, 0, 0, 0, 0];
		let data: Vec<u8> = (0..4).flat_map(|_| block).collect();
		let out = to_rgba8(Etc::Rgb, &data, 6, 5);
		assert_eq!(out.len(), 6 * 5 * 4);
		assert!(out.chunks(4).all(|texel| texel == [138, 138, 138, 255]));
	}
}
