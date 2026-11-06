//! DDS: legacy `DDS_HEADER` and the DX10 extension; 2D, cube, array and volume textures with mip
//! chains; BC1-BC5/BC7 kept as blocks, uncompressed formats converted to RGBA8.

use crate::pixel::{self, Uncompressed};
use crate::{Container, Image, ImageError, PixelFormat, layout};

const DDSD_DEPTH: u32 = 0x0080_0000;
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_YUV: u32 = 0x200;
const DDPF_LUMINANCE: u32 = 0x2_0000;
const DDPF_BUMPDUDV: u32 = 0x8_0000;
/// NVTT extension: tangent-space normal map
const DDPF_NORMAL: u32 = 0x8000_0000;
const DDSCAPS2_CUBEMAP: u32 = 0x200;
const DDSCAPS2_CUBEMAP_FACES: u32 = 0xfc00;
const DDSCAPS2_VOLUME: u32 = 0x20_0000;
const DX10_MISC_TEXTURECUBE: u32 = 0x4;

const fn fourcc(code: &[u8; 4]) -> u32 {
	u32::from_le_bytes(*code)
}

fn corrupt(detail: impl Into<String>) -> ImageError {
	ImageError::Corrupt { container: Container::Dds, detail: detail.into() }
}

fn unsupported(detail: impl Into<String>) -> ImageError {
	ImageError::Unsupported { container: Container::Dds, detail: detail.into() }
}

/// Storage of the source texels.
#[derive(Clone, Copy, Debug)]
enum Source {
	Block(PixelFormat),
	Raw(Uncompressed),
}

struct Resolved {
	source: Source,
	srgb: Option<bool>,
	premultiplied: bool,
	/// The format has no alpha channel (alpha must read as opaque)
	opaque: bool,
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
	let mut b = [0u8; 4];
	b.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(b)
}

pub(crate) fn decode(bytes: &[u8], container: Container) -> Result<Image, ImageError> {
	if bytes.len() < 128 || &bytes[0..4] != b"DDS " {
		return Err(corrupt("truncated header"));
	}
	let header = |offset: usize| u32_at(bytes, 4 + offset);
	let flags = header(4);
	let height = header(8);
	let width = header(12);
	let depth_field = header(20);
	let mip_field = header(24);
	let pf_flags = header(76);
	let pf_fourcc = header(80);
	let pf_bits = header(84);
	let masks = [header(88), header(92), header(96), header(100)];
	let caps2 = header(108);

	let mut data_offset = 128usize;
	let mut layers = 1u32;
	let mut cube = false;
	let mut depth = 1u32;

	let resolved = if pf_flags & DDPF_FOURCC != 0 && pf_fourcc == fourcc(b"DX10") {
		if bytes.len() < 148 {
			return Err(corrupt("truncated DX10 header"));
		}
		let dxgi = u32_at(bytes, 128);
		let dimension = u32_at(bytes, 132);
		let misc = u32_at(bytes, 136);
		let array_size = u32_at(bytes, 140).max(1);
		let alpha_mode = u32_at(bytes, 144) & 0x7;
		data_offset = 148;

		match dimension {
			// Texture1D / Texture2D
			2 | 3 => {
				if misc & DX10_MISC_TEXTURECUBE != 0 {
					cube = true;
					layers = array_size.checked_mul(6).ok_or_else(|| corrupt("array size"))?;
				} else {
					layers = array_size;
				}
			}
			// Texture3D
			4 => {
				depth = depth_field.max(1);
			}
			other => return Err(corrupt(format!("resource dimension {other}"))),
		}

		let mut r = resolve_dxgi(dxgi)?;
		if alpha_mode == 2 {
			r.premultiplied = true;
		}
		r
	} else {
		if caps2 & DDSCAPS2_CUBEMAP != 0 {
			cube = true;
			// Partial cube maps store only the flagged faces
			let faces = (caps2 & DDSCAPS2_CUBEMAP_FACES).count_ones();
			layers = if faces == 0 { 6 } else { faces };
		} else if caps2 & DDSCAPS2_VOLUME != 0 || (flags & DDSD_DEPTH != 0 && depth_field > 1) {
			depth = depth_field.max(1);
		}
		resolve_legacy(pf_flags, pf_fourcc, pf_bits, masks)?
	};

	if width == 0 || height == 0 {
		return Err(corrupt("zero size"));
	}
	layout::check_dimensions(Container::Dds, width, height, depth, layers)?;
	let full_chain = layout::full_chain_len(width, height, depth);
	let level_count = mip_field.clamp(1, full_chain);

	let block = match resolved.source {
		Source::Block(format) => Some(format),
		Source::Raw(_) => None,
	};

	// Source surface sizes (bytes) per level
	let surface_bytes = |level: u32| -> Result<usize, ImageError> {
		let (w, h, d) = layout::level_dims(width, height, depth, level);
		let per_slice = match resolved.source {
			Source::Block(format) => layout::block_surface_bytes(format, w, h),
			Source::Raw(raw) => Some((w as usize) * (h as usize) * raw.bytes_per_pixel()),
		};
		per_slice.and_then(|s| s.checked_mul(d as usize)).ok_or_else(|| corrupt("size overflow"))
	};

	// DDS stores every layer's full mip chain before the next layer
	let mut layer_chain = 0usize;
	for level in 0..level_count {
		layer_chain = layer_chain.checked_add(surface_bytes(level)?).ok_or_else(|| corrupt("size overflow"))?;
	}
	let total = layer_chain.checked_mul(layers as usize).ok_or_else(|| corrupt("size overflow"))?;
	let available = bytes.len() - data_offset;
	if available < total {
		return Err(corrupt(format!("{available} data bytes, {total} expected")));
	}
	let data = &bytes[data_offset..data_offset + total];

	let mut levels = Vec::with_capacity(level_count as usize);
	let mut level_offset = 0usize;
	for level in 0..level_count {
		let size = surface_bytes(level)?;
		let (w, h, d) = layout::level_dims(width, height, depth, level);
		let out_size = match block {
			Some(_) => size * layers as usize,
			None => (w as usize) * (h as usize) * (d as usize) * 4 * layers as usize,
		};
		let mut out = Vec::with_capacity(out_size);
		for layer in 0..layers as usize {
			let start = layer * layer_chain + level_offset;
			let src = &data[start..start + size];
			match resolved.source {
				Source::Block(_) => out.extend_from_slice(src),
				Source::Raw(raw) => pixel::convert_into(raw, src, &mut out),
			}
		}
		levels.push(out);
		level_offset += size;
	}

	let (format, has_alpha) = match resolved.source {
		Source::Block(format) => {
			// DDS cannot say whether BC1 data uses punch-through alpha: scan for it
			let has_alpha = !resolved.opaque && crate::bc::levels_transparent(format, &levels, (width, height, depth), true);
			(format, has_alpha)
		}
		Source::Raw(raw) => {
			let has_alpha = raw.has_alpha() && levels.iter().any(|l| pixel::any_transparent_rgba8(l));
			(PixelFormat::Rgba8, has_alpha)
		}
	};

	let dxt5nm = format == PixelFormat::Bc3
		&& (pf_flags & DDPF_NORMAL != 0 || pf_bits == fourcc(b"A2D5") || pf_fourcc == fourcc(b"RXGB"));
	// A DXT5nm normal map keeps X in alpha: not opacity
	let has_alpha = has_alpha && !dxt5nm;

	Ok(Image {
		width,
		height,
		format,
		container,
		levels,
		has_alpha,
		srgb: resolved.srgb,
		depth,
		layers,
		cube,
		premultiplied_alpha: resolved.premultiplied,
		dxt5nm,
		metadata: Vec::new(),
	})
}

fn block(format: PixelFormat, srgb: Option<bool>) -> Resolved {
	Resolved { source: Source::Block(format), srgb, premultiplied: false, opaque: false }
}

fn raw(format: Uncompressed, srgb: Option<bool>) -> Resolved {
	Resolved { source: Source::Raw(format), srgb, premultiplied: false, opaque: !format.has_alpha() }
}

fn resolve_dxgi(dxgi: u32) -> Result<Resolved, ImageError> {
	use Uncompressed as U;
	// sRGB: Some(true) for *_SRGB, Some(false) for *_UNORM / *_FLOAT, None for *_TYPELESS
	Ok(match dxgi {
		70 => block(PixelFormat::Bc1, None),
		71 => block(PixelFormat::Bc1, Some(false)),
		72 => block(PixelFormat::Bc1, Some(true)),
		73 => block(PixelFormat::Bc2, None),
		74 => block(PixelFormat::Bc2, Some(false)),
		75 => block(PixelFormat::Bc2, Some(true)),
		76 => block(PixelFormat::Bc3, None),
		77 => block(PixelFormat::Bc3, Some(false)),
		78 => block(PixelFormat::Bc3, Some(true)),
		79 => block(PixelFormat::Bc4, None),
		80 => block(PixelFormat::Bc4, Some(false)),
		82 => block(PixelFormat::Bc5, None),
		83 => block(PixelFormat::Bc5, Some(false)),
		97 => block(PixelFormat::Bc7, None),
		98 => block(PixelFormat::Bc7, Some(false)),
		99 => block(PixelFormat::Bc7, Some(true)),
		27 => raw(U::Rgba8, None),
		28 => raw(U::Rgba8, Some(false)),
		29 => raw(U::Rgba8, Some(true)),
		87 => raw(U::Bgra8, Some(false)),
		90 => raw(U::Bgra8, None),
		91 => raw(U::Bgra8, Some(true)),
		88 => raw(U::Bgrx8, Some(false)),
		92 => raw(U::Bgrx8, None),
		93 => raw(U::Bgrx8, Some(true)),
		24 => raw(U::Rgb10A2, Some(false)),
		10 => raw(U::Rgba16Float, Some(false)),
		11 => raw(U::Rgba16, Some(false)),
		2 => raw(U::Rgba32Float, Some(false)),
		34 => raw(U::Rg16Float, Some(false)),
		35 => raw(U::Rg16, Some(false)),
		41 => raw(U::R32Float, Some(false)),
		49 => raw(U::Rg8, Some(false)),
		54 => raw(U::R16Float, Some(false)),
		56 => raw(U::R16, Some(false)),
		61 => raw(U::R8, Some(false)),
		65 => raw(U::A8, Some(false)),
		85 => raw(U::Masked { bits: 16, masks: [0xf800, 0x07e0, 0x001f, 0], luminance: false }, Some(false)),
		86 => raw(U::Masked { bits: 16, masks: [0x7c00, 0x03e0, 0x001f, 0x8000], luminance: false }, Some(false)),
		115 => raw(U::Masked { bits: 16, masks: [0x0f00, 0x00f0, 0x000f, 0xf000], luminance: false }, Some(false)),
		81 | 84 => return Err(unsupported(format!("signed BC4/BC5 (DXGI format {dxgi})"))),
		94..=96 => return Err(unsupported(format!("BC6H (DXGI format {dxgi})"))),
		other => return Err(unsupported(format!("DXGI format {other}"))),
	})
}

fn resolve_legacy(pf_flags: u32, code: u32, bits: u32, masks: [u32; 4]) -> Result<Resolved, ImageError> {
	use Uncompressed as U;
	if pf_flags & DDPF_FOURCC != 0 {
		let mut r = match code {
			c if c == fourcc(b"DXT1") => block(PixelFormat::Bc1, None),
			c if c == fourcc(b"DXT2") || c == fourcc(b"DXT3") => block(PixelFormat::Bc2, None),
			c if c == fourcc(b"DXT4") || c == fourcc(b"DXT5") || c == fourcc(b"RXGB") => block(PixelFormat::Bc3, None),
			c if c == fourcc(b"ATI1") || c == fourcc(b"BC4U") => block(PixelFormat::Bc4, None),
			c if c == fourcc(b"ATI2") || c == fourcc(b"BC5U") => block(PixelFormat::Bc5, None),
			// D3DFORMAT codes stored in the FourCC field
			36 => raw(U::Rgba16, None),
			111 => raw(U::R16Float, None),
			112 => raw(U::Rg16Float, None),
			113 => raw(U::Rgba16Float, None),
			114 => raw(U::R32Float, None),
			116 => raw(U::Rgba32Float, None),
			other => {
				let text: String = other.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect();
				return Err(unsupported(format!("FourCC {text:?} ({other:#x})")));
			}
		};
		r.premultiplied = code == fourcc(b"DXT2") || code == fourcc(b"DXT4");
		return Ok(r);
	}

	if pf_flags & (DDPF_YUV | DDPF_BUMPDUDV) != 0 {
		return Err(unsupported("YUV / bump-map pixel format"));
	}
	if !matches!(bits, 8 | 16 | 24 | 32) {
		return Err(unsupported(format!("{bits}-bit pixel format")));
	}

	let alpha_mask = if pf_flags & (DDPF_ALPHAPIXELS | DDPF_ALPHA) != 0 { masks[3] } else { 0 };
	let luminance = pf_flags & DDPF_LUMINANCE != 0;
	let rgb_masks = if pf_flags & (DDPF_RGB | DDPF_LUMINANCE) != 0 { [masks[0], masks[1], masks[2]] } else { [0, 0, 0] };
	if rgb_masks == [0, 0, 0] && alpha_mask == 0 {
		return Err(unsupported(format!("pixel format flags {pf_flags:#x}")));
	}

	// Fast paths for the common byte layouts
	let format = match (bits, rgb_masks, alpha_mask, luminance) {
		(32, [0x00ff_0000, 0x0000_ff00, 0x0000_00ff], 0xff00_0000, false) => U::Bgra8,
		(32, [0x00ff_0000, 0x0000_ff00, 0x0000_00ff], 0, false) => U::Bgrx8,
		(32, [0x0000_00ff, 0x0000_ff00, 0x00ff_0000], 0xff00_0000, false) => U::Rgba8,
		(24, [0x00ff_0000, 0x0000_ff00, 0x0000_00ff], 0, false) => U::Bgr8,
		_ => U::Masked { bits, masks: [rgb_masks[0], rgb_masks[1], rgb_masks[2], alpha_mask], luminance },
	};
	Ok(raw(format, None))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Builds a legacy DDS header for tests.
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn legacy_header(width: u32, height: u32, mips: u32, pf_flags: u32, code: &[u8; 4], bits: u32, masks: [u32; 4], caps2: u32) -> Vec<u8> {
		let mut h = vec![0u8; 128];
		h[0..4].copy_from_slice(b"DDS ");
		let mut put = |offset: usize, v: u32| h[4 + offset..8 + offset].copy_from_slice(&v.to_le_bytes());
		put(0, 124);
		put(4, 0x1 | 0x2 | 0x4 | 0x1000 | 0x2_0000);
		put(8, height);
		put(12, width);
		put(24, mips);
		put(72, 32);
		put(76, pf_flags);
		put(80, u32::from_le_bytes(*code));
		put(84, bits);
		put(88, masks[0]);
		put(92, masks[1]);
		put(96, masks[2]);
		put(100, masks[3]);
		put(104, 0x1000);
		put(108, caps2);
		h
	}

	#[test]
	fn dxt1_with_mips() {
		// 8x4, 4 levels: 8x4 (2 blocks), 4x2 (1), 2x1 (1), 1x1 (1)
		let mut file = legacy_header(8, 4, 4, DDPF_FOURCC, b"DXT1", 0, [0; 4], 0);
		let blocks: Vec<u8> = (0..5 * 8).map(|i| (i * 37 % 251) as u8).collect();
		file.extend_from_slice(&blocks);
		let image = crate::decode(&file).unwrap();
		assert_eq!((image.width, image.height, image.format, image.container), (8, 4, PixelFormat::Bc1, Container::Dds));
		assert_eq!(image.levels.len(), 4);
		assert_eq!(image.levels[0], &blocks[0..16]);
		assert_eq!(image.levels[3], &blocks[32..40]);
		assert_eq!(image.srgb, None);

		// Truncated data is an error, never a panic
		let short = &file[..file.len() - 1];
		assert!(matches!(crate::decode(short), Err(ImageError::Corrupt { .. })));
	}

	#[test]
	fn bgra8_converts_and_scans_alpha() {
		let masks = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000];
		let mut file = legacy_header(2, 1, 1, DDPF_RGB | DDPF_ALPHAPIXELS, b"\0\0\0\0", 32, masks, 0);
		// B G R A
		file.extend_from_slice(&[1, 2, 3, 255, 4, 5, 6, 128]);
		let image = crate::decode(&file).unwrap();
		assert_eq!(image.format, PixelFormat::Rgba8);
		assert_eq!(image.levels[0], [3, 2, 1, 255, 6, 5, 4, 128]);
		assert!(image.has_alpha);
	}

	#[test]
	fn luminance_alpha_and_565() {
		// L8A8 (16 bit): L in the low byte, A in the high byte
		let mut file = legacy_header(1, 1, 1, DDPF_LUMINANCE | DDPF_ALPHAPIXELS, b"\0\0\0\0", 16, [0xff, 0, 0, 0xff00], 0);
		file.extend_from_slice(&[200, 100]);
		let image = crate::decode(&file).unwrap();
		assert_eq!(image.levels[0], [200, 200, 200, 100]);

		// R5G6B5: pure red
		let mut file = legacy_header(1, 1, 1, DDPF_RGB, b"\0\0\0\0", 16, [0xf800, 0x07e0, 0x001f, 0], 0);
		file.extend_from_slice(&0xf800u16.to_le_bytes());
		let image = crate::decode(&file).unwrap();
		assert_eq!(image.levels[0], [255, 0, 0, 255]);
		assert!(!image.has_alpha);
	}

	#[test]
	fn cube_map_layers_are_gathered_per_level() {
		// 4x4 DXT5 cube with 2 levels: each face stores [level0 (16 bytes), level1 (16 bytes)]
		let mut file = legacy_header(4, 4, 2, DDPF_FOURCC, b"DXT5", 0, [0; 4], DDSCAPS2_CUBEMAP | DDSCAPS2_CUBEMAP_FACES);
		for face in 0..6u8 {
			file.extend_from_slice(&[face * 2; 16]);
			file.extend_from_slice(&[face * 2 + 1; 16]);
		}
		let image = crate::decode(&file).unwrap();
		assert!(image.cube);
		assert_eq!(image.layers, 6);
		assert_eq!(image.levels[0].len(), 6 * 16);
		for face in 0..6usize {
			assert!(image.levels[0][face * 16..face * 16 + 16].iter().all(|&b| b == face as u8 * 2));
			assert!(image.levels[1][face * 16..face * 16 + 16].iter().all(|&b| b == face as u8 * 2 + 1));
		}
	}

	#[test]
	fn dx10_volume_r8() {
		// 4x4x2 R8 volume, 1 level
		let mut file = legacy_header(4, 4, 1, DDPF_FOURCC, b"DX10", 0, [0; 4], DDSCAPS2_VOLUME);
		file[4 + 20..4 + 24].copy_from_slice(&2u32.to_le_bytes());
		for v in [61u32, 4, 0, 1, 0] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		file.extend((0..32).map(|i| i as u8));
		let image = crate::decode(&file).unwrap();
		assert_eq!((image.depth, image.layers, image.format), (2, 1, PixelFormat::Rgba8));
		assert_eq!(image.levels[0].len(), 4 * 4 * 2 * 4);
		assert_eq!(&image.levels[0][4..8], &[1, 0, 0, 255]);
		assert_eq!(image.srgb, Some(false));
	}

	#[test]
	fn dx10_bc7_srgb() {
		let mut file = legacy_header(4, 4, 1, DDPF_FOURCC, b"DX10", 0, [0; 4], 0);
		for v in [99u32, 3, 0, 1, 0] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		// Mode 6 block, all zero payload: opaque? alpha endpoints are 0 -> transparent
		let mut block = [0u8; 16];
		block[0] = 0x40;
		file.extend_from_slice(&block);
		let image = crate::decode(&file).unwrap();
		assert_eq!((image.format, image.srgb), (PixelFormat::Bc7, Some(true)));
		assert!(image.has_alpha);
	}

	#[test]
	fn unsupported_formats_are_reported() {
		let file = {
			let mut f = legacy_header(4, 4, 1, DDPF_FOURCC, b"DX10", 0, [0; 4], 0);
			for v in [95u32, 3, 0, 1, 0] {
				f.extend_from_slice(&v.to_le_bytes());
			}
			f.extend_from_slice(&[0u8; 16]);
			f
		};
		assert!(matches!(crate::decode(&file), Err(ImageError::Unsupported { .. })));
	}
}
