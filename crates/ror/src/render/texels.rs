//! What a kept texture file (render::texture_files) holds, byte for byte: which image a texture key
//! draws (its transcode, else its upload with ror's own mips and compression), how it goes up (as it
//! is, or expanded to RGBA8 where the device takes no block compression or its size is not whole
//! blocks) and the `.tex` layout it is written in.
//!
//! The code deciding a kept file's bytes is this file, render/transcode.rs and ror-image, with the
//! locked versions of ror-image's dependencies: build.rs hashes exactly those into
//! `ROR_TEXTURE_CODE`, the folder the files are kept under, so a build whose texels could differ
//! never reads another's files, and a rebuild that changed none of them keeps every one (the hash took
//! all of resources.rs, texture_files.rs and Cargo.lock before: each change to the GPU caches, the
//! residency's reads or any dependency made every texture again, a map's first frame 10 s instead of
//! 0.1). Keep anything that changes a texel or the layout here; code elsewhere only finds, reads and
//! uploads the files.

use anyhow::Result;

use super::resources::TextureUse;

/// A `.tex` file: this, then format, width, height, levels and flags (u32, little-endian), the bytes
/// its levels hold unpadded and the bytes that follow (u64), then the levels.
pub(super) const MAGIC: &[u8; 8] = b"RORTEX1\0";
pub(super) const HEADER: usize = 48;
/// Rows padded to this for buffer-to-texture copies.
const ROW_ALIGNMENT: u64 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64;

pub(super) const FLAG_ALPHA: u32 = 1;
pub(super) const FLAG_DXT5NM: u32 = 2;
pub(super) const FLAG_NORMAL_RG: u32 = 4;

fn format_code(format: ror_image::PixelFormat) -> u32 {
	use ror_image::PixelFormat;
	match format {
		PixelFormat::Rgba8 => 1,
		PixelFormat::Bc1 => 2,
		PixelFormat::Bc2 => 3,
		PixelFormat::Bc3 => 4,
		PixelFormat::Bc4 => 5,
		PixelFormat::Bc5 => 6,
		PixelFormat::Bc7 => 7,
	}
}

fn format_of(code: u32) -> Option<ror_image::PixelFormat> {
	use ror_image::PixelFormat;
	Some(match code {
		1 => PixelFormat::Rgba8,
		2 => PixelFormat::Bc1,
		3 => PixelFormat::Bc2,
		4 => PixelFormat::Bc3,
		5 => PixelFormat::Bc4,
		6 => PixelFormat::Bc5,
		7 => PixelFormat::Bc7,
		_ => return None,
	})
}

/// A level as it is copied: its copy extent, its rows' bytes unpadded and padded, its rows and its
/// offset in the file's levels.
pub(super) struct Level {
	pub extent: wgpu::Extent3d,
	pub row: u64,
	pub padded: u64,
	pub rows: u64,
	pub offset: u64,
}

/// Every level's layout and their total size, as [`super::resources::GpuTexture::upload`] copies them.
pub(super) fn layout(format: ror_image::PixelFormat, width: u32, height: u32, levels: u32) -> (Vec<Level>, u64) {
	let mut offset = 0;
	let mut out = Vec::with_capacity(levels as usize);
	for level in 0..levels {
		let (w, h) = ((width >> level).max(1), (height >> level).max(1));
		let (row, rows, extent) = match format.block_bytes() {
			Some(block) => (w.div_ceil(4) as u64 * block as u64, h.div_ceil(4) as u64, wgpu::Extent3d { width: w.div_ceil(4) * 4, height: h.div_ceil(4) * 4, depth_or_array_layers: 1 }),
			None => (w as u64 * 4, h as u64, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }),
		};
		let padded = row.next_multiple_of(ROW_ALIGNMENT);
		out.push(Level { extent, row, padded, rows, offset });
		offset += padded * rows;
	}
	(out, offset)
}

/// The flags a texture made from `image` (as uploaded, [`uploadable`]) for `usage` takes:
/// `[has_alpha, dxt5nm, normal_rg]`, as [`super::resources::GpuTexture::upload`] sets them.
fn image_flags(image: &ror_image::Image, usage: TextureUse) -> [bool; 3] {
	[image.has_alpha, image.dxt5nm, usage.is_normal() && image.format == ror_image::PixelFormat::Bc5]
}

/// The `.tex` file of `image` (as uploaded, [`uploadable`]) drawn as `usage`; `None` when its levels
/// are not whole (it is then made every time).
pub(super) fn file_bytes(image: &ror_image::Image, usage: TextureUse) -> Option<Vec<u8>> {
	if image.depth > 1 || image.layers > 1 || image.levels.is_empty() || image.width == 0 || image.height == 0 || image.format.block_bytes().is_none() && image.format != ror_image::PixelFormat::Rgba8 {
		return None;
	}
	let (levels, size) = layout(image.format, image.width, image.height, image.levels.len() as u32);
	if levels.iter().zip(&image.levels).any(|(level, data)| (data.len() as u64) < level.row * level.rows) {
		return None;
	}
	let [has_alpha, dxt5nm, normal_rg] = image_flags(image, usage);
	let flags = if has_alpha { FLAG_ALPHA } else { 0 } | if dxt5nm { FLAG_DXT5NM } else { 0 } | if normal_rg { FLAG_NORMAL_RG } else { 0 };
	// what GpuTexture::upload counts: every level's bytes as decoded
	let unpadded: u64 = image.levels.iter().map(|level| level.len() as u64).sum();
	let mut bytes = Vec::with_capacity(HEADER + size as usize);
	bytes.extend_from_slice(MAGIC);
	for word in [format_code(image.format), image.width, image.height, image.levels.len() as u32, flags] {
		bytes.extend_from_slice(&word.to_le_bytes());
	}
	bytes.extend_from_slice(&unpadded.to_le_bytes());
	bytes.extend_from_slice(&size.to_le_bytes());
	bytes.resize(HEADER, 0);
	for (level, data) in levels.iter().zip(&image.levels) {
		for row in 0..level.rows {
			let start = (row * level.row) as usize;
			bytes.extend_from_slice(&data[start..start + level.row as usize]);
			bytes.resize(bytes.len() + (level.padded - level.row) as usize, 0);
		}
	}
	Some(bytes)
}

/// What a `.tex` file's header says: format, width, height, levels, flags, unpadded bytes, size.
pub(super) struct Header {
	pub format: ror_image::PixelFormat,
	pub width: u32,
	pub height: u32,
	pub levels: u32,
	pub flags: u32,
	pub bytes: u64,
	pub size: u64,
}

pub(super) fn header(bytes: &[u8; HEADER]) -> Option<Header> {
	if &bytes[..8] != MAGIC {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let long = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
	let header = Header { format: format_of(word(8))?, width: word(12), height: word(16), levels: word(20), flags: word(24), bytes: long(28), size: long(36) };
	(header.width > 0 && header.height > 0 && header.levels > 0 && header.levels <= 32 && layout(header.format, header.width, header.height, header.levels).1 == header.size).then_some(header)
}

/// `image` as [`super::resources::GpuTexture::upload`] puts it on the GPU (`bc`: the device takes
/// block-compressed formats): block-compressed as it is where the device takes BC and its size is
/// whole blocks, else expanded to RGBA8.
pub(super) fn uploadable(image: &ror_image::Image, bc: bool) -> std::borrow::Cow<'_, ror_image::Image> {
	use ror_image::PixelFormat;
	let block_aligned = image.width % 4 == 0 && image.height % 4 == 0;
	if image.format != PixelFormat::Rgba8 && (!bc || !block_aligned) { std::borrow::Cow::Owned(ror_image::to_rgba8(image)) } else { std::borrow::Cow::Borrowed(image) }
}

/// The mip chain of an RGBA8 tangent-space normal map (x in red, y in green, levels 1.. replaced) as
/// Roblox's transcoder builds it: each texel the renormalised average of the 2 x 2 unit normals
/// below it (z rebuilt from x and y), alpha averaged. The transcodes in Roblox's HTTP cache match it
/// level by level (DiamondPlate's levels 1-4: slope std 0.991-1.002 of it), while a box filter of
/// the stored values loses 2-11 % of the slopes by level 3-4 (Brick: 16-18 %). Sizes that do not
/// halve evenly fall back to that box filter.
pub fn normal_mips(image: &mut ror_image::Image) {
	use ror_image::PixelFormat;
	let (mut width, mut height) = (image.width as usize, image.height as usize);
	if image.format != PixelFormat::Rgba8 || image.levels.len() != 1 || image.depth > 1 || image.layers > 1 || image.levels[0].len() != width * height * 4 {
		ror_image::generate_mips(image);
		return;
	}
	let mut dims = (width, height);
	while dims != (1, 1) {
		if (dims.0 > 1 && dims.0 % 2 == 1) || (dims.1 > 1 && dims.1 % 2 == 1) {
			ror_image::generate_mips(image);
			return;
		}
		dims = ((dims.0 / 2).max(1), (dims.1 / 2).max(1));
	}
	// unit normals and alpha in floats, so levels do not drift through rounding
	let mut texels: Vec<[f32; 4]> = image.levels[0]
		.chunks_exact(4)
		.map(|p| {
			let x = p[0] as f32 / 127.5 - 1.0;
			let y = p[1] as f32 / 127.5 - 1.0;
			let z = (1.0 - x * x - y * y).max(0.0).sqrt();
			[x, y, z, p[3] as f32 / 255.0]
		})
		.collect();
	let encode = |value: f32| ((value * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
	while (width, height) != (1, 1) {
		let (next_width, next_height) = ((width / 2).max(1), (height / 2).max(1));
		let (step_x, step_y) = (width / next_width, height / next_height);
		let mut next = Vec::with_capacity(next_width * next_height);
		for y in 0..next_height {
			for x in 0..next_width {
				let mut sum = [0.0f32; 4];
				for dy in 0..step_y {
					for dx in 0..step_x {
						let texel = texels[(y * step_y + dy) * width + x * step_x + dx];
						for (total, value) in sum.iter_mut().zip(texel) {
							*total += value;
						}
					}
				}
				let length = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt().max(1e-6);
				let count = (step_x * step_y) as f32;
				next.push([sum[0] / length, sum[1] / length, sum[2] / length, sum[3] / count]);
			}
		}
		image.levels.push(next.iter().flat_map(|n| [encode(n[0]), encode(n[1]), encode(n[2]), (n[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]).collect());
		texels = next;
		(width, height) = (next_width, next_height);
	}
}

impl super::Renderer {
	/// The image a texture key (a transcode layer's, else an image's URI) draws, before
	/// [`uploadable`]: an image asset's transcode, else (none, or not an asset) its upload with ror's
	/// own mips.
	pub(super) fn texture_image(&self, uri: &str, usage: TextureUse) -> Result<ror_image::Image> {
		match super::transcode::parse_layer_key(uri) {
			Some((layer, asset)) => match self.transcode_image(asset, layer) {
				Ok(image) => Ok(image),
				// an image asset without a transcode (or none yet) draws its upload; TexturePack layers
				// are only keyed once they loaded (Renderer::pack_layers)
				Err(_) if layer == ror_assets::Transcode::Image => self.source_image(asset, usage),
				Err(error) => Err(error),
			},
			None => self.source_image(uri, usage),
		}
	}

	/// The uploaded image (PNG, JPEG, DDS, ...) with ror's own mips.
	fn source_image(&self, uri: &str, usage: TextureUse) -> Result<ror_image::Image> {
		let fetched = self.assets.fetch(uri).map_err(|error| anyhow::anyhow!("{error}"))?;
		let mut image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		if image.format == ror_image::PixelFormat::Rgba8 && image.levels.len() <= 1 {
			// Roblox builds colour mips in linear light with a tent filter, data mips with a box
			// (notes/image.md "Mips"), normal maps' as averaged and renormalised normals
			// (normal_mips)
			match usage {
				TextureUse::Color => ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true),
				TextureUse::Normal => normal_mips(&mut image),
				TextureUse::Data | TextureUse::Scalar => ror_image::generate_mips(&mut image),
			}
			let compressed = match usage {
				TextureUse::Normal if !image.dxt5nm => ror_image::encode_bc5(&image),
				TextureUse::Scalar => ror_image::encode_bc4(&image),
				_ => None,
			};
			if let Some(compressed) = compressed.filter(|_| self.gpu.supports_bc()) {
				image = compressed;
			}
		}
		Ok(image)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn layouts_pad_rows_and_round_blocks() {
		let (levels, size) = layout(ror_image::PixelFormat::Bc1, 13, 6, 4);
		assert_eq!(levels.len(), 4);
		// 4 blocks of 8 bytes a row, 2 rows of blocks, padded to 256
		assert_eq!((levels[0].row, levels[0].padded, levels[0].rows), (32, 256, 2));
		assert_eq!((levels[0].extent.width, levels[0].extent.height), (16, 8));
		// 1 x 1: a whole block
		assert_eq!((levels[3].row, levels[3].rows, levels[3].extent.width), (8, 1, 4));
		assert_eq!(size, levels.iter().map(|level| level.padded * level.rows).sum::<u64>());
		let (levels, _) = layout(ror_image::PixelFormat::Rgba8, 100, 3, 2);
		assert_eq!((levels[1].row, levels[1].padded, levels[1].rows, levels[1].offset), (200, 256, 1, 3 * 512));
	}

	#[test]
	fn normal_mips_renormalise_the_averaged_normals() {
		// unit normals (0, 0.707, 0.707) twice and (0, 0, 1) twice: their sum normalised has
		// y = 0.383, where a box filter of the stored values gives 0.354
		let tilt = (0.5f32.sqrt() * 127.5 + 127.5).round() as u8;
		let texels = [[128, tilt, 255, 255], [128, tilt, 255, 255], [128, 128, 255, 255], [128, 128, 255, 255]];
		let mut image = ror_image::Image::from_rgba8(2, 2, texels.iter().flatten().copied().collect());
		normal_mips(&mut image);
		assert_eq!(image.levels.len(), 2);
		let y = image.levels[1][1] as f32 / 127.5 - 1.0;
		assert!((y - 0.383).abs() < 0.01, "y {y}");
	}
}
