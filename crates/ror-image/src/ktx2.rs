//! KTX2: BC1-BC5/BC7 and 8-bit uncompressed vkFormats; supercompression none, zstd and zlib; the
//! level index decides where each level is (Roblox writes both smallest-first and largest-first
//! files); the DFD and key/value data give sRGB / premultiplied information.

use std::cell::RefCell;

use crate::pixel::{self, Uncompressed};
use crate::{Container, Image, ImageError, PixelFormat, layout};

pub(crate) const IDENTIFIER: [u8; 12] = [0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb, b'\r', b'\n', 0x1a, b'\n'];

/// Largest accepted uncompressed level (all layers).
const MAX_LEVEL_BYTES: u64 = 1 << 30;

fn corrupt(detail: impl Into<String>) -> ImageError {
	ImageError::Corrupt { container: Container::Ktx2, detail: detail.into() }
}

fn unsupported(detail: impl Into<String>) -> ImageError {
	ImageError::Unsupported { container: Container::Ktx2, detail: detail.into() }
}

#[derive(Clone, Copy)]
enum Source {
	Block(PixelFormat),
	Raw(Uncompressed),
}

struct VkFormat {
	source: Source,
	srgb: bool,
	/// The format has no alpha channel (BC1 RGB, RGB8): alpha reads as opaque
	opaque: bool,
}

fn vk_format(vk: u32) -> Result<VkFormat, ImageError> {
	let b = |format, srgb, opaque| VkFormat { source: Source::Block(format), srgb, opaque };
	let r = |format: Uncompressed, srgb| VkFormat { source: Source::Raw(format), srgb, opaque: !format.has_alpha() };
	Ok(match vk {
		131 => b(PixelFormat::Bc1, false, true),
		132 => b(PixelFormat::Bc1, true, true),
		133 => b(PixelFormat::Bc1, false, false),
		134 => b(PixelFormat::Bc1, true, false),
		135 => b(PixelFormat::Bc2, false, false),
		136 => b(PixelFormat::Bc2, true, false),
		137 => b(PixelFormat::Bc3, false, false),
		138 => b(PixelFormat::Bc3, true, false),
		139 => b(PixelFormat::Bc4, false, true),
		141 => b(PixelFormat::Bc5, false, true),
		145 => b(PixelFormat::Bc7, false, false),
		146 => b(PixelFormat::Bc7, true, false),
		37 => r(Uncompressed::Rgba8, false),
		43 => r(Uncompressed::Rgba8, true),
		44 => r(Uncompressed::Bgra8, false),
		50 => r(Uncompressed::Bgra8, true),
		23 => r(Uncompressed::Rgb8, false),
		29 => r(Uncompressed::Rgb8, true),
		30 => r(Uncompressed::Bgr8, false),
		36 => r(Uncompressed::Bgr8, true),
		9 => r(Uncompressed::R8, false),
		15 => r(Uncompressed::R8, true),
		16 => r(Uncompressed::Rg8, false),
		22 => r(Uncompressed::Rg8, true),
		140 | 142 => return Err(unsupported(format!("signed BC4/BC5 (vkFormat {vk})"))),
		143 | 144 => return Err(unsupported(format!("BC6H (vkFormat {vk})"))),
		0 => return Err(unsupported("vkFormat 0 (Basis Universal / undefined)")),
		other => return Err(unsupported(format!("vkFormat {other}"))),
	})
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
	let mut b = [0u8; 4];
	b.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(b)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
	let mut b = [0u8; 8];
	b.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(b)
}

/// `bytes[offset..offset + len]` with overflow and bounds checks.
fn range(bytes: &[u8], offset: u64, len: u64, what: &str) -> Result<std::ops::Range<usize>, ImageError> {
	let end = offset.checked_add(len).filter(|&end| end <= bytes.len() as u64).ok_or_else(|| corrupt(format!("{what} outside the file")))?;
	Ok(offset as usize..end as usize)
}

/// Basic data format descriptor fields we use.
#[derive(Default)]
struct Dfd {
	/// KHR_DF_TRANSFER: 1 linear, 2 sRGB
	transfer: Option<u8>,
	premultiplied: bool,
}

fn parse_dfd(bytes: &[u8]) -> Dfd {
	// u32 total size, then the basic descriptor block: u32 vendor/type, u32 version/size,
	// u8 model, u8 primaries, u8 transfer, u8 flags, ...
	if bytes.len() < 4 + 12 {
		return Dfd::default();
	}
	let vendor_type = u32_at(bytes, 4);
	let (vendor, kind) = (vendor_type & 0x1ffff, vendor_type >> 17);
	if vendor != 0 || kind != 0 {
		return Dfd::default();
	}
	Dfd { transfer: Some(bytes[14]), premultiplied: bytes[15] & 1 != 0 }
}

/// Key/value pairs; values are stored as text (trailing NULs removed, invalid UTF-8 replaced).
fn parse_kvd(bytes: &[u8]) -> Vec<(String, String)> {
	let mut out = Vec::new();
	let mut offset = 0usize;
	while offset + 4 <= bytes.len() {
		let len = u32_at(bytes, offset) as usize;
		let start = offset + 4;
		let Some(end) = start.checked_add(len).filter(|&e| e <= bytes.len()) else {
			break;
		};
		let entry = &bytes[start..end];
		if let Some(nul) = entry.iter().position(|&b| b == 0) {
			let key = String::from_utf8_lossy(&entry[..nul]).into_owned();
			let mut value = &entry[nul + 1..];
			while let [rest @ .., 0] = value {
				value = rest;
			}
			out.push((key, String::from_utf8_lossy(value).into_owned()));
		}
		// Entries are padded to 4 bytes
		offset = end.div_ceil(4) * 4;
	}
	out
}

thread_local! {
	static ZSTD: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
}

fn zstd_level(src: &[u8], expected: usize) -> Result<Vec<u8>, ImageError> {
	let mut out = Vec::with_capacity(expected);
	ZSTD.with(|cell| {
		let mut slot = cell.borrow_mut();
		if slot.is_none() {
			*slot = Some(zstd::bulk::Decompressor::new().map_err(|e| corrupt(format!("zstd: {e}")))?);
		}
		let Some(dctx) = slot.as_mut() else {
			return Err(corrupt("zstd context"));
		};
		dctx.decompress_to_buffer(src, &mut out).map_err(|e| corrupt(format!("zstd: {e}")))
	})?;
	Ok(out)
}

fn zlib_level(src: &[u8], expected: usize) -> Result<Vec<u8>, ImageError> {
	use std::io::Read;
	let mut out = Vec::with_capacity(expected);
	flate2::read::ZlibDecoder::new(src)
		.take(expected as u64 + 1)
		.read_to_end(&mut out)
		.map_err(|e| corrupt(format!("zlib: {e}")))?;
	Ok(out)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
	if bytes.len() < 80 || bytes[0..12] != IDENTIFIER {
		return Err(corrupt("truncated header"));
	}
	let vk = u32_at(bytes, 12);
	let width = u32_at(bytes, 20);
	let height = u32_at(bytes, 24).max(1);
	let pixel_depth = u32_at(bytes, 28);
	let layer_count = u32_at(bytes, 32);
	let face_count = u32_at(bytes, 36);
	let level_count = u32_at(bytes, 40);
	let scheme = u32_at(bytes, 44);
	let dfd_offset = u32_at(bytes, 48) as u64;
	let dfd_len = u32_at(bytes, 52) as u64;
	let kvd_offset = u32_at(bytes, 56) as u64;
	let kvd_len = u32_at(bytes, 60) as u64;

	let format = vk_format(vk)?;
	if !matches!(face_count, 1 | 6) {
		return Err(corrupt(format!("{face_count} faces")));
	}
	let depth = pixel_depth.max(1);
	let layers = layer_count.max(1).checked_mul(face_count).ok_or_else(|| corrupt("layer count"))?;
	layout::check_dimensions(Container::Ktx2, width, height, depth, layers)?;

	// levelCount 0 means "level 0 only, generate the rest"
	let levels_in_file = level_count.max(1);
	if levels_in_file > layout::full_chain_len(width, height, depth) {
		return Err(corrupt(format!("{level_count} levels for {width}x{height}x{depth}")));
	}
	let index_end = 80 + levels_in_file as usize * 24;
	if bytes.len() < index_end {
		return Err(corrupt("truncated level index"));
	}

	let dfd = if dfd_len > 0 { parse_dfd(&bytes[range(bytes, dfd_offset, dfd_len, "DFD")?]) } else { Dfd::default() };
	let metadata = if kvd_len > 0 { parse_kvd(&bytes[range(bytes, kvd_offset, kvd_len, "key/value data")?]) } else { Vec::new() };

	let mut levels = Vec::with_capacity(levels_in_file as usize);
	for level in 0..levels_in_file {
		let entry = 80 + level as usize * 24;
		let offset = u64_at(bytes, entry);
		let length = u64_at(bytes, entry + 8);
		let uncompressed = u64_at(bytes, entry + 16);
		let src = &bytes[range(bytes, offset, length, "level data")?];

		let (w, h, d) = layout::level_dims(width, height, depth, level);
		let slice_bytes = match format.source {
			Source::Block(f) => layout::block_surface_bytes(f, w, h),
			Source::Raw(raw) => (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(raw.bytes_per_pixel())),
		};
		let expected = slice_bytes
			.and_then(|s| s.checked_mul(d as usize))
			.and_then(|s| s.checked_mul(layers as usize))
			.filter(|&s| s as u64 <= MAX_LEVEL_BYTES)
			.ok_or_else(|| corrupt("level too large"))?;

		let data = match scheme {
			0 => {
				if src.len() < expected {
					return Err(corrupt(format!("level {level}: {} bytes, {expected} expected", src.len())));
				}
				std::borrow::Cow::Borrowed(&src[..expected])
			}
			2 | 3 => {
				if uncompressed < expected as u64 {
					return Err(corrupt(format!("level {level}: {uncompressed} uncompressed bytes, {expected} expected")));
				}
				let out = if scheme == 2 { zstd_level(src, expected)? } else { zlib_level(src, expected)? };
				if out.len() < expected {
					return Err(corrupt(format!("level {level}: decompressed {} bytes, {expected} expected", out.len())));
				}
				let mut out = out;
				out.truncate(expected);
				std::borrow::Cow::Owned(out)
			}
			1 => return Err(unsupported("BasisLZ supercompression")),
			other => return Err(unsupported(format!("supercompression scheme {other}"))),
		};

		let level_data = match format.source {
			Source::Block(_) => data.into_owned(),
			Source::Raw(raw) => {
				let mut out = Vec::with_capacity(expected / raw.bytes_per_pixel() * 4);
				pixel::convert_into(raw, &data, &mut out);
				out
			}
		};
		levels.push(level_data);
	}

	// sRGB: the vkFormat, then Roblox's colorSpace key (its DFDs always say linear), then the DFD
	let srgb = if format.srgb {
		Some(true)
	} else {
		let color_space = metadata.iter().find(|(k, _)| k == "colorSpace").map(|(_, v)| v.as_str());
		match color_space {
			Some("Gamma2RGB") | Some("sRGB") | Some("srgb") => Some(true),
			Some("Linear") | Some("linear") => Some(false),
			_ => match dfd.transfer {
				Some(2) => Some(true),
				Some(1) => Some(false),
				_ => None,
			},
		}
	};

	let pixel_format = match format.source {
		Source::Block(f) => f,
		Source::Raw(_) => PixelFormat::Rgba8,
	};
	let dxt5nm = pixel_format == PixelFormat::Bc3 && srgb == Some(false) && levels.first().is_some_and(|l| is_dxt5nm(l));
	// A DXT5nm normal map keeps X in alpha: not opacity
	let has_alpha = !format.opaque
		&& !dxt5nm
		&& match format.source {
			Source::Block(f) => crate::bc::levels_transparent(f, &levels, (width, height, depth), true),
			Source::Raw(_) => levels.iter().any(|l| pixel::any_transparent_rgba8(l)),
		};

	Ok(Image {
		width,
		height,
		format: pixel_format,
		container: Container::Ktx2,
		levels,
		has_alpha,
		srgb,
		depth,
		layers,
		cube: face_count == 6,
		premultiplied_alpha: dfd.premultiplied,
		dxt5nm,
		metadata,
	})
}

/// The progressive pack table of a Roblox KTX2 file: (first level, level count) per pack index, from
/// its `pack_N = "first,count"` keys. Empty for a file holding the whole chain.
pub(crate) fn pack_table(image: &Image) -> Vec<(u32, u32)> {
	let mut table = Vec::new();
	for index in 0.. {
		let Some(value) = image.meta(&format!("pack_{index}")) else {
			break;
		};
		let mut parts = value.split(',').map(|part| part.trim().parse::<u32>().ok());
		match (parts.next().flatten(), parts.next().flatten()) {
			(Some(first), Some(count)) if count > 0 => table.push((first, count)),
			_ => return Vec::new(),
		}
	}
	table
}

pub(crate) fn join_packs(packs: Vec<Image>) -> Result<Image, ImageError> {
	let Some(first) = packs.first() else {
		return Err(corrupt("no packs to join"));
	};
	let table = pack_table(first);
	let format = first.format;
	if table.is_empty() {
		// a whole chain in one file
		if packs.len() == 1 {
			return Ok(packs.into_iter().next().unwrap_or_else(|| unreachable!()));
		}
		return Err(corrupt("packs without a pack table"));
	}
	let total = table.iter().map(|&(start, count)| start + count).max().unwrap_or(0);
	let mut slots: Vec<Option<Image>> = (0..table.len()).map(|_| None).collect();
	for pack in packs {
		let index = pack.meta("packIndex").and_then(|value| value.trim().parse::<usize>().ok()).ok_or_else(|| corrupt("pack without packIndex"))?;
		if index >= table.len() || pack_table(&pack) != table {
			return Err(corrupt(format!("pack {index} does not belong to this pack table")));
		}
		let (start, count) = table[index];
		if pack.format != format || pack.levels.len() < count as usize || pack.layers != 1 || pack.depth != 1 {
			return Err(corrupt(format!("pack {index}: {:?} with {} levels, expected {format:?} with {count}", pack.format, pack.levels.len())));
		}
		if start > 31 {
			return Err(corrupt(format!("pack {index} starts at level {start}")));
		}
		slots[index] = Some(pack);
	}
	let missing: Vec<usize> = slots.iter().enumerate().filter(|(_, slot)| slot.is_none()).map(|(index, _)| index).collect();
	if !missing.is_empty() {
		return Err(corrupt(format!("missing packs {missing:?}")));
	}
	let slots: Vec<Image> = slots.into_iter().flatten().collect();

	// The pack holding level 0 gives the size (transcodedWidth / Height) and the image's properties
	let top = table.iter().position(|&(start, _)| start == 0).ok_or_else(|| corrupt("no pack holds level 0"))?;
	let width = slots[top].width;
	let height = slots[top].height;
	// constant textures are a single 4x4 level (`pack_0 = 0,1`)
	if total > layout::full_chain_len(width, height, 1) {
		return Err(corrupt(format!("packs hold {total} levels of a {width}x{height} chain")));
	}
	let mut levels = Vec::with_capacity(total as usize);
	for level in 0..total {
		let index = table.iter().position(|&(start, count)| (start..start + count).contains(&level)).ok_or_else(|| corrupt(format!("no pack holds level {level}")))?;
		let pack = &slots[index];
		let (w, h, _) = layout::level_dims(width, height, 1, level);
		let local = (level - table[index].0) as usize;
		if pack.level_size(local).0 != w || pack.level_size(local).1 != h {
			return Err(corrupt(format!("pack {index} level {local} is not {w}x{h}")));
		}
		levels.push(pack.levels[local].clone());
	}
	let has_alpha = slots.iter().any(|pack| pack.has_alpha);
	let mut metadata = slots[top].metadata.clone();
	metadata.retain(|(key, _)| key != "packIndex");
	Ok(Image { levels, has_alpha, metadata, ..slots[top].clone_without_levels() })
}

/// Roblox transcodes normal maps (`colorSpace = Linear`) to BC3 as (1, Y, 0, X): every colour
/// endpoint is pure red 31 / blue 0 in RGB565. Measured on 4,000 cached Roblox KTX2 files: all 927
/// swizzled normal maps match, no colour texture does.
fn is_dxt5nm(level: &[u8]) -> bool {
	!level.is_empty()
		&& level.chunks_exact(16).all(|block| {
			let c0 = u16::from_le_bytes([block[8], block[9]]);
			let c1 = u16::from_le_bytes([block[10], block[11]]);
			c0 >> 11 == 31 && c0 & 31 == 0 && c1 >> 11 == 31 && c1 & 31 == 0
		})
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// Builds a KTX2 file. `levels` are uncompressed level payloads (level 0 first); `scheme` 0 or
	/// 2; `smallest_first` stores level data in reverse order (as the KTX2 spec recommends).
	pub(crate) fn build(vk: u32, width: u32, height: u32, levels: &[Vec<u8>], scheme: u32, smallest_first: bool, kv: &[(&str, &str)]) -> Vec<u8> {
		let mut kvd = Vec::new();
		for (k, v) in kv {
			let entry_len = k.len() + 1 + v.len() + 1;
			kvd.extend_from_slice(&(entry_len as u32).to_le_bytes());
			kvd.extend_from_slice(k.as_bytes());
			kvd.push(0);
			kvd.extend_from_slice(v.as_bytes());
			kvd.push(0);
			while kvd.len() % 4 != 0 {
				kvd.push(0);
			}
		}
		// Minimal basic DFD: total size, vendor/type 0, version 2 / size 24, model, primaries, transfer=1, flags
		let mut dfd = Vec::new();
		dfd.extend_from_slice(&28u32.to_le_bytes());
		dfd.extend_from_slice(&0u32.to_le_bytes());
		dfd.extend_from_slice(&((24u32 << 16) | 2).to_le_bytes());
		dfd.extend_from_slice(&[128, 1, 1, 0]);
		dfd.extend_from_slice(&[3, 3, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0]);

		let index_end = 80 + levels.len() * 24;
		let dfd_offset = index_end;
		let kvd_offset = dfd_offset + dfd.len();
		let mut data_offset = kvd_offset + kvd.len();

		let payloads: Vec<Vec<u8>> = levels
			.iter()
			.map(|l| if scheme == 2 { zstd::bulk::compress(l, 3).unwrap() } else { l.clone() })
			.collect();
		let mut offsets = vec![0usize; levels.len()];
		let order: Vec<usize> = if smallest_first { (0..levels.len()).rev().collect() } else { (0..levels.len()).collect() };
		for &i in &order {
			offsets[i] = data_offset;
			data_offset += payloads[i].len();
		}

		let mut file = Vec::new();
		file.extend_from_slice(&IDENTIFIER);
		for v in [vk, 1, width, height, 0, 0, 1, levels.len() as u32, scheme] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		for v in [dfd_offset as u32, dfd.len() as u32, kvd_offset as u32, kvd.len() as u32] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		file.extend_from_slice(&0u64.to_le_bytes());
		file.extend_from_slice(&0u64.to_le_bytes());
		for (i, level) in levels.iter().enumerate() {
			file.extend_from_slice(&(offsets[i] as u64).to_le_bytes());
			file.extend_from_slice(&(payloads[i].len() as u64).to_le_bytes());
			file.extend_from_slice(&(level.len() as u64).to_le_bytes());
		}
		file.extend_from_slice(&dfd);
		file.extend_from_slice(&kvd);
		for &i in &order {
			file.extend_from_slice(&payloads[i]);
		}
		file
	}

	fn bc1_levels(width: u32, height: u32, count: u32) -> Vec<Vec<u8>> {
		(0..count)
			.map(|level| {
				let (w, h, _) = layout::level_dims(width, height, 1, level);
				let size = layout::block_surface_bytes(PixelFormat::Bc1, w, h).unwrap();
				(0..size).map(|i| (i as u32 * 31 + level * 7) as u8).collect()
			})
			.collect()
	}

	#[test]
	fn zstd_levels_in_either_order() {
		let levels = bc1_levels(16, 8, 5);
		for smallest_first in [false, true] {
			let file = build(131, 16, 8, &levels, 2, smallest_first, &[("colorSpace", "Gamma2RGB"), ("avgAlpha", "255")]);
			let image = crate::decode(&file).unwrap();
			assert_eq!((image.width, image.height, image.format, image.container), (16, 8, PixelFormat::Bc1, Container::Ktx2));
			assert_eq!(image.levels, levels, "smallest_first={smallest_first}");
			// BC1 RGB: never alpha, even with punch-through blocks in the data
			assert!(!image.has_alpha);
			assert_eq!(image.srgb, Some(true));
			assert_eq!(image.meta("avgAlpha"), Some("255"));
		}
	}

	#[test]
	fn srgb_sources() {
		let levels = bc1_levels(4, 4, 1);
		let linear = crate::decode(&build(137, 4, 4, &[vec![0xff; 16]], 0, false, &[("colorSpace", "Linear")])).unwrap();
		assert_eq!(linear.srgb, Some(false));
		assert!(!linear.has_alpha);
		let srgb_format = crate::decode(&build(132, 4, 4, &levels, 0, false, &[])).unwrap();
		assert_eq!(srgb_format.srgb, Some(true));
		// No colorSpace key: the DFD transfer function (linear in `build`)
		let dfd_only = crate::decode(&build(131, 4, 4, &levels, 0, false, &[])).unwrap();
		assert_eq!(dfd_only.srgb, Some(false));
	}

	#[test]
	fn bc3_alpha_scan_and_rgba8() {
		// BC3 block with alpha endpoints 255/0 and index 1 everywhere: alpha 0
		let mut block = vec![255u8, 0, 0x49, 0x92, 0x24, 0x49, 0x92, 0x24];
		block.extend_from_slice(&[0; 8]);
		let image = crate::decode(&build(137, 4, 4, &[block], 0, false, &[])).unwrap();
		assert!(image.has_alpha);

		let rgba: Vec<u8> = (0..2 * 2 * 4).map(|i| if i % 4 == 3 { 255 } else { i as u8 }).collect();
		let image = crate::decode(&build(37, 2, 2, std::slice::from_ref(&rgba), 2, false, &[])).unwrap();
		assert_eq!(image.format, PixelFormat::Rgba8);
		assert_eq!(image.levels[0], rgba);
		assert!(!image.has_alpha);

		let rgb = vec![1u8, 2, 3, 4, 5, 6];
		let image = crate::decode(&build(23, 2, 1, &[rgb], 0, false, &[])).unwrap();
		assert_eq!(image.levels[0], [1, 2, 3, 255, 4, 5, 6, 255]);
	}

	#[test]
	fn packs_join_into_the_whole_chain() {
		// A 32 x 32 transcode as Roblox splits it: levels 3..5 (pack 0), 1..2 (pack 1), 0 (pack 2)
		let levels = bc1_levels(32, 32, 6);
		let table = [("pack_0", "3,3"), ("pack_1", "1,2"), ("pack_2", "0,1")];
		let pack = |index: usize, first: usize, count: usize| {
			let kv: Vec<(&str, &str)> = table.iter().copied().chain([("packIndex", ["0", "1", "2"][index]), ("colorSpace", "Gamma2RGB"), ("transcodedWidth", "32")]).collect();
			crate::decode(&build(131, 32 >> first, 32 >> first, &levels[first..first + count], 2, index == 1, &kv)).unwrap()
		};
		let (p0, p1, p2) = (pack(0, 3, 3), pack(1, 1, 2), pack(2, 0, 1));
		assert_eq!(crate::ktx2_pack_table(&p0), vec![(3, 3), (1, 2), (0, 1)]);
		let joined = crate::join_packs(vec![p1.clone(), p2.clone(), p0.clone()]).unwrap();
		assert_eq!((joined.width, joined.height, joined.format), (32, 32, PixelFormat::Bc1));
		assert_eq!(joined.levels, levels);
		assert_eq!(joined.srgb, Some(true));
		assert_eq!(joined.meta("packIndex"), None);
		assert_eq!(joined.meta("pack_0"), Some("3,3"));

		// A pack missing, or one pack twice
		assert!(crate::join_packs(vec![p0.clone(), p2.clone()]).is_err());
		assert!(crate::join_packs(vec![p0.clone(), p1.clone(), p1.clone()]).is_err());
		// A file holding the whole chain comes back as it is
		let whole = crate::decode(&build(131, 32, 32, &levels, 2, false, &[("colorSpace", "Linear")])).unwrap();
		assert!(crate::ktx2_pack_table(&whole).is_empty());
		assert_eq!(crate::join_packs(vec![whole]).unwrap().levels, levels);
		// A constant texture: one 4 x 4 level in one pack
		let constant = crate::decode(&build(131, 4, 4, &bc1_levels(4, 4, 1), 2, false, &[("pack_0", "0,1"), ("packIndex", "0")])).unwrap();
		assert_eq!(crate::join_packs(vec![constant]).unwrap().levels.len(), 1);
	}

	#[test]
	fn corrupt_files_error() {
		let levels = bc1_levels(16, 16, 3);
		let file = build(131, 16, 16, &levels, 2, true, &[]);
		for cut in [0, 12, 79, 100, file.len() / 2, file.len() - 1] {
			assert!(crate::decode(&file[..cut]).is_err(), "cut at {cut}");
		}
		// Garbage zstd payload
		let mut bad = file.clone();
		let last = bad.len() - 3;
		bad[last] ^= 0xff;
		bad[last - 5] ^= 0xff;
		let _ = crate::decode(&bad);

		// Absurd sizes are rejected before allocating
		let mut huge = build(131, 4, 4, &bc1_levels(4, 4, 1), 0, false, &[]);
		huge[20..24].copy_from_slice(&60000u32.to_le_bytes());
		huge[24..28].copy_from_slice(&60000u32.to_le_bytes());
		assert!(crate::decode(&huge).is_err());
	}
}
