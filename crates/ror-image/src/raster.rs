//! PNG, JPEG, WebP, GIF, BMP and TGA through the `image` crate, decoded to RGBA8.

use std::io::Cursor;

use crate::{Container, Image, ImageError, PixelFormat, layout, pixel};

fn image_format(container: Container) -> Option<image::ImageFormat> {
	Some(match container {
		Container::Png => image::ImageFormat::Png,
		Container::Jpeg => image::ImageFormat::Jpeg,
		Container::Webp => image::ImageFormat::WebP,
		Container::Gif => image::ImageFormat::Gif,
		Container::Bmp => image::ImageFormat::Bmp,
		Container::Tga => image::ImageFormat::Tga,
		_ => return None,
	})
}

pub(crate) fn decode(bytes: &[u8], container: Container) -> Result<Image, ImageError> {
	let Some(format) = image_format(container) else {
		return Err(ImageError::Unrecognised);
	};
	let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
	let mut limits = image::Limits::default();
	limits.max_image_width = Some(layout::MAX_DIMENSION);
	limits.max_image_height = Some(layout::MAX_DIMENSION);
	limits.max_alloc = Some(layout::MAX_TEXELS * 4);
	reader.limits(limits);

	let decoded = reader.decode().map_err(|e| map_error(container, e))?;
	let source_alpha = decoded.color().has_alpha();
	let (width, height) = (decoded.width(), decoded.height());
	layout::check_dimensions(container, width, height, 1, 1)?;
	// Moves the buffer when the image already is RGBA8
	let rgba = decoded.into_rgba8().into_raw();
	let has_alpha = source_alpha && pixel::any_transparent_rgba8(&rgba);

	Ok(Image {
		width,
		height,
		format: PixelFormat::Rgba8,
		container,
		levels: vec![rgba],
		has_alpha,
		srgb: None,
		depth: 1,
		layers: 1,
		cube: false,
		premultiplied_alpha: false,
		dxt5nm: false,
		metadata: Vec::new(),
	})
}

fn map_error(container: Container, error: image::ImageError) -> ImageError {
	match error {
		image::ImageError::Unsupported(e) => ImageError::Unsupported { container, detail: e.to_string() },
		image::ImageError::Limits(e) => ImageError::Unsupported { container, detail: e.to_string() },
		other => ImageError::Corrupt { container, detail: other.to_string() },
	}
}

/// Heuristic TGA check (TGA has no magic number): a v2 footer, or a plausible header.
pub(crate) fn looks_like_tga(bytes: &[u8]) -> bool {
	if bytes.len() < 18 {
		return false;
	}
	if bytes.len() >= 44 && bytes.ends_with(b"TRUEVISION-XFILE.\0") {
		return true;
	}
	let color_map_type = bytes[1];
	let image_type = bytes[2];
	let color_map_depth = bytes[7];
	let width = u16::from_le_bytes([bytes[12], bytes[13]]);
	let height = u16::from_le_bytes([bytes[14], bytes[15]]);
	let depth = bytes[16];
	let descriptor = bytes[17];

	let mapped = matches!(image_type, 1 | 9);
	let truecolor = matches!(image_type, 2 | 10);
	let gray = matches!(image_type, 3 | 11);
	if color_map_type > 1 || !(mapped || truecolor || gray) || width == 0 || height == 0 || descriptor & 0xc0 != 0 {
		return false;
	}
	if mapped && (color_map_type != 1 || !matches!(color_map_depth, 15 | 16 | 24 | 32) || !matches!(depth, 8 | 16)) {
		return false;
	}
	if truecolor && !matches!(depth, 15 | 16 | 24 | 32) {
		return false;
	}
	if gray && !matches!(depth, 8 | 16) {
		return false;
	}
	// Uncompressed images must contain all their pixels
	if matches!(image_type, 1..=3) {
		let id_len = bytes[0] as usize;
		let map_len = if color_map_type == 1 { u16::from_le_bytes([bytes[5], bytes[6]]) as usize * (color_map_depth as usize).div_ceil(8) } else { 0 };
		let pixels = width as usize * height as usize * (depth as usize).div_ceil(8);
		if bytes.len() < 18 + id_len + map_len + pixels {
			return false;
		}
	}
	true
}
