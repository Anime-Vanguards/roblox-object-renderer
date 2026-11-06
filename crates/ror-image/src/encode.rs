//! BC4 / BC5 encoding of RGBA8 images, for single- and two-channel data maps (roughness, metalness,
//! normal maps) kept compressed on the GPU: 8x and 4x smaller than RGBA8 at an error of at most
//! (max - min) / 14 per block, which smooth data maps barely show.
//!
//! Sequential on purpose: callers encode many textures in parallel already, often inside a cache's
//! one-time initialiser, where nested work stealing could pick up a task waiting on that same
//! initialiser.

use crate::{Image, PixelFormat};

/// One BC4 block of a channel's 16 values (row-major 4x4): the 8-value mode with the block's
/// extremes as endpoints, every value at its nearest palette entry.
fn bc4_block(values: &[u8; 16]) -> [u8; 8] {
	let (low, high) = values.iter().fold((u8::MAX, u8::MIN), |(low, high), &value| (low.min(value), high.max(value)));
	let mut block = [0u8; 8];
	block[0] = high;
	block[1] = low;
	if high == low {
		return block;
	}
	// palette index for each step from high (0) to low (1), the 8-value mode's order
	const ORDER: [u64; 8] = [0, 2, 3, 4, 5, 6, 7, 1];
	let range = (high - low) as f32;
	let mut indices = 0u64;
	for (texel, &value) in values.iter().enumerate() {
		let step = (((high - value) as f32 / range) * 7.0).round() as usize;
		indices |= ORDER[step.min(7)] << (3 * texel);
	}
	block[2..8].copy_from_slice(&indices.to_le_bytes()[..6]);
	block
}

/// One level of RGBA8 texels as BC4 (`channels` = [c]) or BC5 (`channels` = [c0, c1]) blocks.
fn encode_level(rgba: &[u8], width: u32, height: u32, channels: &[usize]) -> Vec<u8> {
	let (w, h) = (width as usize, height as usize);
	let (bw, bh) = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
	let block_bytes = 8 * channels.len();
	let mut out = vec![0u8; bw * bh * block_bytes];
	out.chunks_mut(bw * block_bytes).enumerate().for_each(|(by, row)| {
		for bx in 0..bw {
			for (slot, &channel) in channels.iter().enumerate() {
				let mut values = [0u8; 16];
				for (texel, value) in values.iter_mut().enumerate() {
					// edge blocks repeat the last row / column
					let x = (bx * 4 + texel % 4).min(w - 1);
					let y = (by * 4 + texel / 4).min(h - 1);
					*value = rgba[(y * w + x) * 4 + channel];
				}
				let offset = bx * block_bytes + slot * 8;
				row[offset..offset + 8].copy_from_slice(&bc4_block(&values));
			}
		}
	});
	out
}

/// An RGBA8 image (all its levels) as BC4 of its red channel, or `None` when the image is not
/// RGBA8 or its size is not a multiple of 4 (GPUs need whole blocks at level 0).
pub fn encode_bc4(image: &Image) -> Option<Image> {
	encode(image, &[0], PixelFormat::Bc4)
}

/// An RGBA8 image (all its levels) as BC5 of its red and green channels (a normal map's X and Y;
/// Z is reconstructed when sampling), or `None` as for [`encode_bc4`].
pub fn encode_bc5(image: &Image) -> Option<Image> {
	encode(image, &[0, 1], PixelFormat::Bc5)
}

fn encode(image: &Image, channels: &[usize], format: PixelFormat) -> Option<Image> {
	if image.format != PixelFormat::Rgba8 || image.width % 4 != 0 || image.height % 4 != 0 || image.layers != 1 || image.depth > 1 {
		return None;
	}
	let levels = image
		.levels
		.iter()
		.enumerate()
		.map(|(level, data)| {
			let (width, height) = ((image.width >> level).max(1), (image.height >> level).max(1));
			encode_level(data, width, height, channels)
		})
		.collect();
	Some(Image { format, levels, has_alpha: false, srgb: Some(false), dxt5nm: false, ..image.clone() })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn bc4_round_trips_within_a_step() {
		let width = 16;
		let height = 8;
		let pixels: Vec<u8> = (0..width * height).flat_map(|index| [(index * 7 % 251) as u8, (index * 3 % 200) as u8, 0, 255]).collect();
		let image = Image::from_rgba8(width, height, pixels.clone());
		for (encoded, channels) in [(encode_bc4(&image).unwrap(), vec![0]), (encode_bc5(&image).unwrap(), vec![0, 1])] {
			let decoded = crate::to_rgba8(&encoded);
			for (index, texel) in decoded.levels[0].chunks_exact(4).enumerate() {
				for &channel in &channels {
					let x = (index as u32 % width) / 4 * 4;
					let y = (index as u32 / width) / 4 * 4;
					// the block's range bounds the error: (max - min) / 14
					let block: Vec<u8> = (0..16).map(|t| pixels[(((y + t / 4) * width + x + t % 4) * 4) as usize + channel]).collect();
					let range = (*block.iter().max().unwrap() - *block.iter().min().unwrap()) as i32;
					let error = (texel[channel] as i32 - pixels[index * 4 + channel] as i32).abs();
					assert!(error <= range / 14 + 1, "texel {index} channel {channel}: error {error}, range {range}");
				}
			}
		}
		assert!(encode_bc4(&Image::from_rgba8(6, 4, vec![0; 6 * 4 * 4])).is_none());
	}
}
