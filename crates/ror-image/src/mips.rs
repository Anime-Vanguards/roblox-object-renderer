//! Mip generation for RGBA8 images.
//!
//! Each level halves every dimension (rounded down, at least 1). The box filter averages 2 (2x2,
//! 2x2x2) texels for even sizes and uses the exact area weights for odd sizes (a 2n+1 → n
//! reduction has three taps (n-i)/(2n+1), n/(2n+1), (i+1)/(2n+1)), so no source texel is dropped.
//! The triangle filter weights source texels by `1 - d / s` (d: distance from the output texel
//! centre, s: the reduction factor, about 2), clamping at the edges; for even sizes that is the
//! classic [1, 3, 3, 1] / 8 kernel.

use std::sync::OnceLock;

use crate::{Image, MipFilter, PixelFormat, layout};

/// Where texel values are averaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Space {
	/// The stored values (gamma space for sRGB colour)
	Stored,
	/// Linear light: RGB decoded from sRGB, filtered, re-encoded; alpha as stored
	Linear,
}

pub(crate) fn generate(image: &mut Image, filter: MipFilter, space: Space) {
	if image.format != PixelFormat::Rgba8 || image.levels.is_empty() {
		return;
	}
	let (width, height, depth, layers) = (image.width, image.height, image.depth.max(1), image.layers.max(1) as usize);
	let Some(slice) = layout::surface_bytes(PixelFormat::Rgba8, width, height) else {
		return;
	};
	let layer_bytes = slice * depth as usize;
	if image.levels[0].len() != layer_bytes * layers {
		return;
	}

	let count = layout::full_chain_len(width, height, depth) as usize;
	image.levels.truncate(1);
	let mut chain: Vec<Vec<u8>> = vec![Vec::new(); count - 1];
	for layer in 0..layers {
		let base = &image.levels[0][layer * layer_bytes..(layer + 1) * layer_bytes];
		let levels = chain_for_layer(base, [width as usize, height as usize, depth as usize], count, filter, space);
		for (dst, level) in chain.iter_mut().zip(levels) {
			dst.extend_from_slice(&level);
		}
	}
	image.levels.extend(chain);
}

/// Levels 1.. of one layer. The float path keeps full precision between levels.
fn chain_for_layer(base: &[u8], dims: [usize; 3], count: usize, filter: MipFilter, space: Space) -> Vec<Vec<u8>> {
	let mut out: Vec<Vec<u8>> = Vec::with_capacity(count - 1);
	let mut dims = dims;

	// Integer fast path: box filter on stored values while the level is 2D with even (or unit) sides
	if filter == MipFilter::Box && space == Space::Stored {
		while out.len() < count - 1 && dims[2] == 1 && even_or_one(dims[0]) && even_or_one(dims[1]) {
			let src: &[u8] = out.last().map_or(base, |v| v.as_slice());
			let (next, next_dims) = halve_even_2d(src, dims[0], dims[1]);
			out.push(next);
			dims = next_dims;
		}
		if out.len() == count - 1 {
			return out;
		}
	}

	let to_linear = srgb_to_linear_table();
	let to_srgb = linear_to_srgb_table();
	let src: &[u8] = out.last().map_or(base, |v| v.as_slice());
	// the first float level is read from the bytes as it is filtered: a float copy of the source, four
	// times its size, was the largest of a GUI image's allocations (its odd sides take this path)
	let stored = unit_table();
	let tables = match space {
		Space::Stored => [stored; 4],
		Space::Linear => [to_linear, to_linear, to_linear, stored],
	};
	if out.len() >= count - 1 {
		return out;
	}
	let (mut next, mut next_dims) = downsample_bytes(src, dims, filter, tables);
	loop {
		let encoded: Vec<u8> = match space {
			Space::Stored => next.iter().map(|&v| unit_to_u8(v)).collect(),
			Space::Linear => next
				.chunks_exact(4)
				.flat_map(|p| {
					let encode = |v: f32| to_srgb[(v.clamp(0.0, 1.0) * 65535.0 + 0.5) as usize];
					[encode(p[0]), encode(p[1]), encode(p[2]), unit_to_u8(p[3])]
				})
				.collect(),
		};
		out.push(encoded);
		dims = next_dims;
		if out.len() >= count - 1 {
			return out;
		}
		(next, next_dims) = downsample(&next, dims, filter);
	}
}

/// Stored code → value in [0, 1], as `v as f32 * (1.0 / 255.0)` gives it.
fn unit_table() -> &'static [f32; 256] {
	static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
	TABLE.get_or_init(|| std::array::from_fn(|v| v as f32 * (1.0 / 255.0)))
}

#[inline]
fn unit_to_u8(v: f32) -> u8 {
	(v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn even_or_one(n: usize) -> bool {
	n == 1 || n.is_multiple_of(2)
}

/// Integer 2x2 (or 2x1 / 1x2) average with round-half-up, for even (or unit) dimensions.
fn halve_even_2d(src: &[u8], width: usize, height: usize) -> (Vec<u8>, [usize; 3]) {
	let (w2, h2) = ((width / 2).max(1), (height / 2).max(1));
	let mut out = vec![0u8; w2 * h2 * 4];
	let row = width * 4;
	match (width > 1, height > 1) {
		(true, true) => {
			for (y, dst) in out.chunks_exact_mut(w2 * 4).enumerate() {
				let r0 = &src[(2 * y) * row..(2 * y + 1) * row];
				let r1 = &src[(2 * y + 1) * row..(2 * y + 2) * row];
				for (x, d) in dst.chunks_exact_mut(4).enumerate() {
					let i = x * 8;
					for c in 0..4 {
						let sum = r0[i + c] as u16 + r0[i + 4 + c] as u16 + r1[i + c] as u16 + r1[i + 4 + c] as u16;
						d[c] = ((sum + 2) >> 2) as u8;
					}
				}
			}
		}
		(true, false) => {
			for (x, d) in out.chunks_exact_mut(4).enumerate() {
				for c in 0..4 {
					d[c] = ((src[x * 8 + c] as u16 + src[x * 8 + 4 + c] as u16 + 1) >> 1) as u8;
				}
			}
		}
		(false, true) => {
			for (y, d) in out.chunks_exact_mut(4).enumerate() {
				for c in 0..4 {
					d[c] = ((src[2 * y * 4 + c] as u16 + src[(2 * y + 1) * 4 + c] as u16 + 1) >> 1) as u8;
				}
			}
		}
		(false, false) => out.copy_from_slice(&src[..4]),
	}
	(out, [w2, h2, 1])
}

/// Source taps (index, weight) of each output index when halving an axis of length `n`.
fn taps(n: usize, filter: MipFilter) -> Vec<Vec<(usize, f32)>> {
	if n <= 1 {
		return vec![vec![(0, 1.0)]];
	}
	let m = n / 2;
	match filter {
		MipFilter::Box if n.is_multiple_of(2) => (0..m).map(|i| vec![(2 * i, 0.5), (2 * i + 1, 0.5)]).collect(),
		MipFilter::Box => {
			let nf = n as f32;
			(0..m).map(|i| vec![(2 * i, (m - i) as f32 / nf), (2 * i + 1, m as f32 / nf), (2 * i + 2, (i + 1) as f32 / nf)]).collect()
		}
		MipFilter::Triangle => {
			let scale = n as f64 / m as f64;
			(0..m)
				.map(|i| {
					let centre = (i as f64 + 0.5) * scale;
					let first = (centre - scale).floor() as isize;
					let last = (centre + scale).ceil() as isize;
					let mut taps: Vec<(usize, f32)> = Vec::new();
					let mut sum = 0.0f64;
					for j in first..=last {
						let w = 1.0 - ((j as f64 + 0.5) - centre).abs() / scale;
						if w <= 0.0 {
							continue;
						}
						let index = j.clamp(0, n as isize - 1) as usize;
						sum += w;
						match taps.iter_mut().find(|t| t.0 == index) {
							Some(t) => t.1 += w as f32,
							None => taps.push((index, w as f32)),
						}
					}
					for t in &mut taps {
						t.1 = (t.1 as f64 / sum) as f32;
					}
					taps
				})
				.collect()
		}
	}
}

/// [`downsample`] of the float values `tables` give each channel's bytes (channel c of a texel reads
/// `tables[c]`), the same sums in the same order without a float copy of `src`.
fn downsample_bytes(src: &[u8], dims: [usize; 3], filter: MipFilter, tables: [&[f32; 256]; 4]) -> (Vec<f32>, [usize; 3]) {
	let [w, h, d] = dims;
	let at = |index: usize, channel: usize| tables[channel][src[index] as usize];

	if filter == MipFilter::Box && d == 1 && w >= 2 && h >= 2 && w % 2 == 0 && h % 2 == 0 {
		let (w2, h2) = (w / 2, h / 2);
		let mut out = vec![0f32; w2 * h2 * 4];
		for (y, dst) in out.chunks_exact_mut(w2 * 4).enumerate() {
			let (r0, r1) = ((2 * y) * w * 4, (2 * y + 1) * w * 4);
			for (x, d4) in dst.chunks_exact_mut(4).enumerate() {
				let (a, b) = (r0 + x * 8, r1 + x * 8);
				for c in 0..4 {
					d4[c] = (at(a + c, c) + at(a + c + 4, c) + at(b + c, c) + at(b + c + 4, c)) * 0.25;
				}
			}
		}
		return (out, [w2, h2, 1]);
	}

	let mut cur = dims;
	let data = if w > 1 {
		let tx = taps(w, filter);
		let ow = tx.len();
		let mut data = vec![0f32; ow * h * d * 4];
		cur[0] = ow;
		for (row_in, row_out) in src.chunks_exact(w * 4).zip(data.chunks_exact_mut(ow * 4)) {
			for (x, out) in row_out.chunks_exact_mut(4).enumerate() {
				let mut acc = [0f32; 4];
				for &(s, weight) in &tx[x] {
					let p = &row_in[s * 4..s * 4 + 4];
					for c in 0..4 {
						acc[c] += tables[c][p[c] as usize] * weight;
					}
				}
				out.copy_from_slice(&acc);
			}
		}
		data
	} else {
		src.iter().enumerate().map(|(index, _)| at(index, index % 4)).collect()
	};
	downsample_lines(data, cur, filter)
}

/// Halves a `[depth][height][width][4]` float grid along every axis longer than 1.
fn downsample(src: &[f32], dims: [usize; 3], filter: MipFilter) -> (Vec<f32>, [usize; 3]) {
	let [w, h, d] = dims;

	// Common case in one pass: box filter, 2D, both sides even
	if filter == MipFilter::Box && d == 1 && w >= 2 && h >= 2 && w % 2 == 0 && h % 2 == 0 {
		let (w2, h2) = (w / 2, h / 2);
		let mut out = vec![0f32; w2 * h2 * 4];
		for (y, dst) in out.chunks_exact_mut(w2 * 4).enumerate() {
			let r0 = &src[(2 * y) * w * 4..(2 * y + 1) * w * 4];
			let r1 = &src[(2 * y + 1) * w * 4..(2 * y + 2) * w * 4];
			for ((d4, a), b) in dst.chunks_exact_mut(4).zip(r0.chunks_exact(8)).zip(r1.chunks_exact(8)) {
				for c in 0..4 {
					d4[c] = (a[c] + a[c + 4] + b[c] + b[c + 4]) * 0.25;
				}
			}
		}
		return (out, [w2, h2, 1]);
	}

	let mut data: Vec<f32>;
	let mut cur = dims;

	// X: per texel taps within each row
	if w > 1 {
		let tx = taps(w, filter);
		let ow = tx.len();
		data = vec![0f32; ow * h * d * 4];
		cur[0] = ow;
		for (row_in, row_out) in src.chunks_exact(w * 4).zip(data.chunks_exact_mut(ow * 4)) {
			for (x, out) in row_out.chunks_exact_mut(4).enumerate() {
				let mut acc = [0f32; 4];
				for &(s, weight) in &tx[x] {
					let p = &row_in[s * 4..s * 4 + 4];
					for c in 0..4 {
						acc[c] += p[c] * weight;
					}
				}
				out.copy_from_slice(&acc);
			}
		}
	} else {
		data = src.to_vec();
	}
	downsample_lines(data, cur, filter)
}

/// [`downsample`]'s Y and Z passes over the X pass's `data`: weighted sums of whole rows / slices
/// (vectorises).
fn downsample_lines(mut data: Vec<f32>, dims: [usize; 3], filter: MipFilter) -> (Vec<f32>, [usize; 3]) {
	let mut cur = dims;
	for axis in 1..3 {
		let n = cur[axis];
		if n <= 1 {
			continue;
		}
		let t = taps(n, filter);
		// A "line" is a row (axis 1) or a slice (axis 2); lines repeat `outer` times
		let line = if axis == 1 { cur[0] * 4 } else { cur[0] * cur[1] * 4 };
		let outer = if axis == 1 { cur[2] } else { 1 };
		let mut next = vec![0f32; line * t.len() * outer];
		for o in 0..outer {
			let src_block = &data[o * line * n..(o + 1) * line * n];
			let dst_block = &mut next[o * line * t.len()..(o + 1) * line * t.len()];
			for (i, dst) in dst_block.chunks_exact_mut(line).enumerate() {
				for &(s, weight) in &t[i] {
					let src_line = &src_block[s * line..(s + 1) * line];
					for (a, b) in dst.iter_mut().zip(src_line) {
						*a += b * weight;
					}
				}
			}
		}
		data = next;
		cur[axis] = t.len();
	}
	(data, cur)
}

/// sRGB code → linear light in [0, 1].
pub(crate) fn srgb_to_linear_table() -> &'static [f32; 256] {
	static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
	TABLE.get_or_init(|| {
		let mut t = [0f32; 256];
		for (i, v) in t.iter_mut().enumerate() {
			let c = i as f64 / 255.0;
			*v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } as f32;
		}
		t
	})
}

/// Linear light quantised to 16 bits → sRGB code.
pub(crate) fn linear_to_srgb_table() -> &'static [u8] {
	static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
	TABLE.get_or_init(|| {
		(0..65536u32)
			.map(|i| {
				let l = i as f64 / 65535.0;
				let s = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
				(s * 255.0 + 0.5).clamp(0.0, 255.0) as u8
			})
			.collect()
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Container, Image};

	fn rgba_image(width: u32, height: u32, data: Vec<u8>) -> Image {
		Image {
			width,
			height,
			format: PixelFormat::Rgba8,
			container: Container::Png,
			levels: vec![data],
			has_alpha: false,
			srgb: None,
			depth: 1,
			layers: 1,
			cube: false,
			premultiplied_alpha: false,
			dxt5nm: false,
			metadata: Vec::new(),
		}
	}

	#[test]
	fn even_chain() {
		// 4x2: left half black, right half white
		let mut data = Vec::new();
		for _ in 0..2 {
			for x in 0..4 {
				let v = if x < 2 { 0 } else { 255 };
				data.extend_from_slice(&[v, v, v, 255]);
			}
		}
		let mut image = rgba_image(4, 2, data);
		crate::generate_mips(&mut image);
		assert_eq!(image.levels.len(), 3);
		assert_eq!(image.levels[1], [0, 0, 0, 255, 255, 255, 255, 255]);
		// (0 + 255 + 1) >> 1 = 128
		assert_eq!(image.levels[2], [128, 128, 128, 255]);

		let mut srgb = rgba_image(4, 2, image.levels[0].clone());
		crate::generate_mips_srgb(&mut srgb);
		// Half black, half white in linear light is 0.5 → sRGB 188
		assert_eq!(srgb.levels[2], [188, 188, 188, 255]);
	}

	#[test]
	fn float_path_matches_integer_path() {
		// The generic box downsample agrees with the integer fast path up to rounding
		let data: Vec<u8> = (0..16 * 8 * 4).map(|i| ((i * 97) % 256) as u8).collect();
		let mut fast = rgba_image(16, 8, data.clone());
		crate::generate_mips(&mut fast);
		let floats: Vec<f32> = data.iter().map(|&v| v as f32 / 255.0).collect();
		let (level1, dims) = downsample(&floats, [16, 8, 1], MipFilter::Box);
		assert_eq!(dims, [8, 4, 1]);
		for (a, b) in fast.levels[1].iter().zip(level1.iter()) {
			assert!((*a as f32 - b * 255.0).abs() <= 0.5 + 1e-3);
		}
	}

	#[test]
	fn odd_sizes_keep_every_texel() {
		// 3x1: 0, 0, 255 → one texel with the exact average 85
		let data = vec![0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255];
		let mut image = rgba_image(3, 1, data);
		crate::generate_mips(&mut image);
		assert_eq!(image.levels.len(), 2);
		assert_eq!(image.levels[1], [85, 85, 85, 85]);

		// 13x7 chain: sizes 13x7, 6x3, 3x1, 1x1
		for filter in [MipFilter::Box, MipFilter::Triangle] {
			let mut image = rgba_image(13, 7, vec![200; 13 * 7 * 4]);
			crate::generate_mips_with(&mut image, filter, false);
			let sizes: Vec<usize> = image.levels.iter().map(|l| l.len() / 4).collect();
			assert_eq!(sizes, [91, 18, 3, 1]);
			assert!(image.levels.iter().all(|l| l.iter().all(|&v| v == 200)), "{filter:?}");
		}
	}

	/// The first float level read from the bytes is the one a float copy of them gave, bit for bit:
	/// every filter, both spaces, even, odd and unit sides, volumes.
	#[test]
	fn byte_source_matches_float_source() {
		let mut seed = 12345u32;
		let mut next = move || {
			seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
			(seed >> 24) as u8
		};
		let stored = unit_table();
		let linear = srgb_to_linear_table();
		for dims in [[13, 7, 1], [16, 8, 1], [1, 9, 1], [9, 1, 1], [403, 217, 1], [6, 5, 1], [5, 6, 1], [4, 4, 3], [7, 3, 5], [1, 1, 1], [2, 2, 1]] {
			let bytes: Vec<u8> = (0..dims[0] * dims[1] * dims[2] * 4).map(|_| next()).collect();
			for filter in [MipFilter::Box, MipFilter::Triangle] {
				for tables in [[stored; 4], [linear, linear, linear, stored]] {
					let floats: Vec<f32> = bytes.iter().enumerate().map(|(index, &v)| tables[index % 4][v as usize]).collect();
					let (expected, expected_dims) = downsample(&floats, dims, filter);
					let (got, got_dims) = downsample_bytes(&bytes, dims, filter, tables);
					assert_eq!(got_dims, expected_dims);
					assert!(got.iter().zip(&expected).all(|(a, b)| a.to_bits() == b.to_bits()), "{dims:?} {filter:?}");
				}
			}
		}
		// the stored table is the conversion the float path made
		assert!((0..256).all(|v| stored[v].to_bits() == (v as u8 as f32 * (1.0 / 255.0)).to_bits()));
	}

	#[test]
	fn triangle_taps() {
		// Even sizes: [1, 3, 3, 1] / 8, clamped at the edges
		let t = taps(8, MipFilter::Triangle);
		assert_eq!(t.len(), 4);
		let mid: Vec<(usize, f32)> = t[1].clone();
		assert_eq!(mid, vec![(1, 0.125), (2, 0.375), (3, 0.375), (4, 0.125)]);
		let edge = &t[0];
		let w0: f32 = edge.iter().filter(|x| x.0 == 0).map(|x| x.1).sum();
		assert!((w0 - 0.5).abs() < 1e-6);
		for row in taps(13, MipFilter::Triangle) {
			let sum: f32 = row.iter().map(|x| x.1).sum();
			assert!((sum - 1.0).abs() < 1e-5);
		}
	}

	#[test]
	fn layers_and_volumes() {
		// Two 2x2 layers with different constant colours
		let mut data = vec![10u8; 16];
		data.extend_from_slice(&[250u8; 16]);
		let mut image = rgba_image(2, 2, data);
		image.layers = 2;
		crate::generate_mips_srgb(&mut image);
		assert_eq!(image.levels[1], [10, 10, 10, 10, 250, 250, 250, 250]);

		// 2x2x2 volume: slices 0 and 255 average to 128 in stored space
		let mut data = vec![0u8; 16];
		data.extend_from_slice(&[255u8; 16]);
		let mut image = rgba_image(2, 2, data);
		image.depth = 2;
		crate::generate_mips(&mut image);
		assert_eq!(image.levels.len(), 2);
		assert_eq!(image.levels[1], [128, 128, 128, 128]);
	}

	#[test]
	fn srgb_tables_round_trip() {
		let to_linear = srgb_to_linear_table();
		let to_srgb = linear_to_srgb_table();
		for i in 0..256 {
			let back = to_srgb[(to_linear[i] * 65535.0 + 0.5) as usize];
			assert_eq!(back as usize, i);
		}
	}
}
