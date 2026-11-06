//! Texture dimension helpers shared by the containers.

use crate::{Container, ImageError, PixelFormat};

/// Largest accepted width / height / depth / layer count.
pub(crate) const MAX_DIMENSION: u32 = 1 << 16;

/// Largest accepted texel count of level 0 over all layers and slices (4 GiB as RGBA8).
pub(crate) const MAX_TEXELS: u64 = 1 << 30;

pub(crate) fn check_dimensions(container: Container, width: u32, height: u32, depth: u32, layers: u32) -> Result<(), ImageError> {
	let too_large = |detail: String| Err(ImageError::Unsupported { container, detail });
	if width == 0 || height == 0 || depth == 0 || layers == 0 {
		return Err(ImageError::Corrupt { container, detail: format!("size {width}x{height}x{depth}, {layers} layers") });
	}
	if width > MAX_DIMENSION || height > MAX_DIMENSION || depth > MAX_DIMENSION || layers > MAX_DIMENSION {
		return too_large(format!("too large: {width}x{height}x{depth}, {layers} layers"));
	}
	let texels = width as u64 * height as u64 * depth as u64 * layers as u64;
	if texels > MAX_TEXELS {
		return too_large(format!("too large: {texels} texels"));
	}
	Ok(())
}

/// Number of levels in a full mip chain down to 1x1x1.
pub(crate) fn full_chain_len(width: u32, height: u32, depth: u32) -> u32 {
	let largest = width.max(height).max(depth).max(1);
	32 - largest.leading_zeros()
}

/// Size of `level` (each dimension halved per level, rounded down, at least 1).
pub(crate) fn level_dims(width: u32, height: u32, depth: u32, level: u32) -> (u32, u32, u32) {
	let shrink = |v: u32| v.checked_shr(level).unwrap_or(0).max(1);
	(shrink(width), shrink(height), shrink(depth))
}

/// Bytes of one 2D surface of block data (whole 4x4 blocks).
pub(crate) fn block_surface_bytes(format: PixelFormat, width: u32, height: u32) -> Option<usize> {
	let block = format.block_bytes()?;
	let blocks = (width as usize).div_ceil(4).max(1).checked_mul((height as usize).div_ceil(4).max(1))?;
	blocks.checked_mul(block)
}

/// Bytes of one 2D surface in `format` (blocks or RGBA8).
pub(crate) fn surface_bytes(format: PixelFormat, width: u32, height: u32) -> Option<usize> {
	match format {
		PixelFormat::Rgba8 => (width as usize).checked_mul(height as usize)?.checked_mul(4),
		_ => block_surface_bytes(format, width, height),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn chains() {
		assert_eq!(full_chain_len(1, 1, 1), 1);
		assert_eq!(full_chain_len(1024, 1024, 1), 11);
		assert_eq!(full_chain_len(416, 416, 1), 9);
		assert_eq!(full_chain_len(128, 2048, 1), 12);
		assert_eq!(level_dims(13, 7, 1, 2), (3, 1, 1));
		assert_eq!(level_dims(13, 7, 1, 40), (1, 1, 1));
		assert_eq!(block_surface_bytes(PixelFormat::Bc1, 13, 1), Some(4 * 8));
		assert_eq!(block_surface_bytes(PixelFormat::Bc3, 1, 1), Some(16));
	}
}
