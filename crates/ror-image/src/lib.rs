//! Texture decoding for Roblox content.
//!
//! Roblox serves images in the format they were uploaded in (PNG, JPEG, ...) or transcoded (WebP,
//! KTX2 with zstd supercompression holding BC1 / BC3 blocks); the Studio install ships DDS and `.tex`
//! files. [`decode`] sniffs the container and returns an [`Image`]: either RGBA8 pixels or GPU block
//! data (BCn) with its mip chain, so the renderer can upload compressed data directly.
//!
//! - PNG, JPEG, WebP, GIF (first frame), BMP, TGA: decoded with the `image` crate to RGBA8, one level.
//! - DDS: legacy and DX10 headers; BC1/BC2/BC3/BC4/BC5/BC7 returned as blocks; uncompressed formats
//!   (RGBA/BGRA/BGRX/BGR 8-bit, masks such as 565/4444/1555/L8/A8L8, R8/RG8/A8, 16-bit and float
//!   formats, RGB10A2) converted to RGBA8; cube maps, arrays and volume textures.
//! - KTX2: BC1-BC5/BC7 and 8-bit uncompressed vkFormats; supercompression none, zstd, zlib; levels
//!   are located through the level index (Roblox writes both smallest-first and largest-first files).
//! - Roblox `.tex` (Studio's `PlatformContent/pc/textures/sky/*.tex`): these are plain DDS files
//!   (NVTT-written DXT1, one 2D cube face per file named `_bk/_dn/_ft/_lf/_rt/_up`, e.g.
//!   `sky512_*.tex` are 1024x1024 with 11 levels), so they decode with [`Container::Dds`].
//!
//! Roblox KTX2 files carry key/value metadata (see [`Image::metadata`]): `colorSpace`
//! (`Gamma2RGB`: colour with a gamma of 2, the texels the square root of the linear values;
//! `Linear`: data such as normal maps, or an image's texels as uploaded), `avgAlpha`, `avgRed` ...,
//! `constantColor`, `RobloxOriginalWidth/Height`, `transcodedWidth/Height`, `contentHash`,
//! `acrVersion`, and progressive "packs": `pack_N = "first_level,count"` with `packIndex` naming the
//! pack the file holds. A pack file's `pixelWidth` (this [`Image::width`]) is
//! `transcodedWidth >> first_level`, and it only holds that pack's levels; [`join_packs`] joins a
//! transcode's packs into the whole chain.

mod bc;
mod dds;
mod encode;
mod ktx2;
mod layout;
mod mips;
mod pixel;
mod raster;

pub use encode::{encode_bc4, encode_bc5};

/// Pixel storage of an [`Image`]'s levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
	/// 4 bytes per pixel, R G B A.
	Rgba8,
	/// BC1 (DXT1), 8 bytes per 4x4 block. When [`Image::has_alpha`] is false the alpha of decoded
	/// texels must be ignored: wgpu only has BC1 RGBA formats, while DDS DXT1 / KTX2 BC1 RGB data
	/// may still contain three-colour blocks whose index 3 an RGBA view decodes as transparent black.
	Bc1,
	/// BC2 (DXT3), 16 bytes per block.
	Bc2,
	/// BC3 (DXT5), 16 bytes per block.
	Bc3,
	/// BC4 (one channel), 8 bytes per block.
	Bc4,
	/// BC5 (two channels), 16 bytes per block.
	Bc5,
	/// BC7, 16 bytes per block.
	Bc7,
}

impl PixelFormat {
	/// Bytes per 4x4 block for block formats, `None` for [`PixelFormat::Rgba8`].
	pub fn block_bytes(self) -> Option<usize> {
		match self {
			PixelFormat::Rgba8 => None,
			PixelFormat::Bc1 | PixelFormat::Bc4 => Some(8),
			PixelFormat::Bc2 | PixelFormat::Bc3 | PixelFormat::Bc5 | PixelFormat::Bc7 => Some(16),
		}
	}

	pub fn is_block_compressed(self) -> bool {
		self != PixelFormat::Rgba8
	}
}

/// The container an image was decoded from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
	Png,
	Jpeg,
	Webp,
	Gif,
	Bmp,
	Tga,
	Dds,
	Ktx2,
	/// Roblox `.tex` (Studio sky textures). Every `.tex` file in current and past installs is a DDS
	/// file and decodes as [`Container::Dds`]; [`decode_with_extension`] reports this variant for
	/// DDS data read from a `.tex` file.
	RobloxTex,
}

/// A decoded image: level 0 first, each following level half the size (rounded down, at least 1).
#[derive(Clone, Debug)]
pub struct Image {
	pub width: u32,
	pub height: u32,
	pub format: PixelFormat,
	pub container: Container,
	/// Mip levels, `levels[0]` is full size. Block formats store whole 4x4 blocks per level.
	///
	/// A level holds `layers` layers one after another; each layer holds `max(1, depth >> level)`
	/// 2D slices of `(width >> level) x (height >> level)` texels (each at least 1). Plain 2D images
	/// have one layer and one slice. The chain may be partial (Roblox KTX2 packs, DDS files with
	/// fewer mips); [`generate_mips`] builds a full one for RGBA8.
	pub levels: Vec<Vec<u8>>,
	/// True when any pixel is less than fully opaque. Exact: decoded RGBA8 pixels and BC alpha data
	/// (BC1 punch-through, BC2/BC3/BC7 alpha; texels outside the image in edge blocks ignored) are
	/// scanned, so a BC3 texture whose alpha is 255 everywhere reports false; check `format` for
	/// the format-level answer. Always false for formats without alpha (KTX2 BC1 RGB, RGB8,
	/// BC4/BC5) and for [`Image::dxt5nm`] normal maps (their alpha is the X component).
	pub has_alpha: bool,
	/// Whether the texels are gamma-encoded colour: `Some(true)` for an `_SRGB` DXGI / Vulkan format
	/// (sRGB) or Roblox's KTX2 `colorSpace = Gamma2RGB` (gamma 2.0: the texels hold the square root of
	/// the linear colour, so square them, not the sRGB curve), `Some(false)` linear or stored as
	/// uploaded (`_UNORM` / float DXGI formats, KTX2 `colorSpace = Linear` or a linear DFD transfer
	/// function), `None` when the container does not say (PNG, JPEG, WebP, GIF, BMP, TGA, legacy DDS
	/// headers).
	pub srgb: Option<bool>,
	/// Depth of a volume texture (1 for 2D textures).
	pub depth: u32,
	/// Number of layers per level: array layers times faces (6 for a cube map, faces ordered
	/// +X, -X, +Y, -Y, +Z, -Z); 1 for plain textures.
	pub layers: u32,
	/// True for cube maps (and cube map arrays).
	pub cube: bool,
	/// The colour channels are premultiplied by alpha (DDS DXT2/DXT4 or DX10 alpha mode, KTX2 DFD).
	pub premultiplied_alpha: bool,
	/// A BC3 normal map with X stored in alpha and Y in green ("DXT5nm"); reconstruct
	/// `z = sqrt(1 - x² - y²)`. DDS: NVTT's `DDPF_NORMAL` flag, `A2D5` swizzle code or the `RXGB`
	/// FourCC (Studio's `textures/plastic/normal*.dds`). Roblox KTX2: BC3 with `colorSpace =
	/// Linear` whose colour endpoints are all pure red (Roblox transcodes normal maps to
	/// (1, Y, 0, X); every such file in the local cache matches, no colour texture does).
	pub dxt5nm: bool,
	/// KTX2 key/value data in file order (values as text, trailing NULs removed).
	pub metadata: Vec<(String, String)>,
}

impl Image {
	/// A single-level 2D RGBA8 image from tightly packed pixels (`width * height * 4` bytes), e.g.
	/// a solid colour texture. Reports [`Container::Png`]; `has_alpha` is computed, `srgb` is `None`.
	pub fn from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Image {
		let has_alpha = pixel::any_transparent_rgba8(&pixels);
		Image {
			width,
			height,
			format: PixelFormat::Rgba8,
			container: Container::Png,
			levels: vec![pixels],
			has_alpha,
			srgb: None,
			depth: 1,
			layers: 1,
			cube: false,
			premultiplied_alpha: false,
			dxt5nm: false,
			metadata: Vec::new(),
		}
	}

	/// Size of `level` as (width, height, depth).
	pub fn level_size(&self, level: usize) -> (u32, u32, u32) {
		layout::level_dims(self.width, self.height, self.depth.max(1), level.min(31) as u32)
	}

	/// Value of a KTX2 key/value entry.
	pub fn meta(&self, key: &str) -> Option<&str> {
		self.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
	}
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
	#[error("unrecognised image data")]
	Unrecognised,
	#[error("unsupported {container:?} image: {detail}")]
	Unsupported { container: Container, detail: String },
	#[error("corrupt {container:?} image: {detail}")]
	Corrupt { container: Container, detail: String },
}

/// Identifies the container from the first bytes. TGA has no signature: it is recognised from a
/// plausible header (or its v2 footer) only when nothing else matches.
pub fn sniff(bytes: &[u8]) -> Option<Container> {
	if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
		Some(Container::Png)
	} else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
		Some(Container::Jpeg)
	} else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
		Some(Container::Webp)
	} else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
		Some(Container::Gif)
	} else if bytes.starts_with(b"DDS ") {
		Some(Container::Dds)
	} else if bytes.starts_with(&ktx2::IDENTIFIER) {
		Some(Container::Ktx2)
	} else if is_bmp(bytes) {
		Some(Container::Bmp)
	} else if raster::looks_like_tga(bytes) {
		Some(Container::Tga)
	} else {
		None
	}
}

fn is_bmp(bytes: &[u8]) -> bool {
	if bytes.len() < 18 || !bytes.starts_with(b"BM") {
		return false;
	}
	let dib = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
	matches!(dib, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124)
}

/// Decodes any supported image container.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
	match sniff(bytes) {
		Some(Container::Dds) => dds::decode(bytes, Container::Dds),
		Some(Container::Ktx2) => ktx2::decode(bytes),
		Some(container) => raster::decode(bytes, container),
		None => Err(ImageError::Unrecognised),
	}
}

/// [`decode`] with the file extension (without the dot, any case) as a hint: DDS data from a
/// `.tex` file reports [`Container::RobloxTex`], and `.tga` files are decoded as TGA even when the
/// header heuristic of [`sniff`] does not recognise them.
pub fn decode_with_extension(bytes: &[u8], extension: &str) -> Result<Image, ImageError> {
	let sniffed = sniff(bytes);
	if extension.eq_ignore_ascii_case("tex") && sniffed == Some(Container::Dds) {
		return dds::decode(bytes, Container::RobloxTex);
	}
	if extension.eq_ignore_ascii_case("tga") && sniffed.is_none() {
		return raster::decode(bytes, Container::Tga);
	}
	decode(bytes)
}

/// The progressive pack table of a Roblox KTX2 file, (first level, level count) per pack index from
/// its `pack_N` keys; empty for a file holding the whole chain. A transcode of a 1024² image is
/// typically four packs, `pack_0 = 4,7` (levels 4..10, 64² down), `pack_1 = 2,2`, `pack_2 = 1,1`,
/// `pack_3 = 0,1`, each fetched as its own file (notes/assets.md).
pub fn ktx2_pack_table(image: &Image) -> Vec<(u32, u32)> {
	ktx2::pack_table(image)
}

/// Joins the files of one Roblox KTX2 transcode's progressive packs (in any order) into one image
/// with the whole mip chain: level `i` comes from the pack whose `pack_N` range holds it, at the
/// size of the pack holding level 0 (`transcodedWidth` x `transcodedHeight`). The result keeps
/// that pack's metadata without `packIndex`. A single file without pack keys is returned as is.
/// Errors when a pack is missing, repeated with another table, or of another format.
pub fn join_packs(packs: Vec<Image>) -> Result<Image, ImageError> {
	ktx2::join_packs(packs)
}

/// Expands a block-compressed image to RGBA8 (all levels, layers and slices); RGBA8 images are
/// returned unchanged. BC1 without [`Image::has_alpha`] decodes as BC1 RGB (punch-through texels
/// opaque black); BC4 gives (r, 0, 0, 255) and BC5 (r, g, 0, 255), as a GPU samples them.
pub fn to_rgba8(image: &Image) -> Image {
	if image.format == PixelFormat::Rgba8 {
		return image.clone();
	}
	let mut levels = Vec::with_capacity(image.levels.len());
	for (level, data) in image.levels.iter().enumerate() {
		let (w, h, d) = image.level_size(level);
		let slices = d as usize * image.layers.max(1) as usize;
		let src_slice = layout::block_surface_bytes(image.format, w, h).unwrap_or(0);
		let dst_slice = w as usize * h as usize * 4;
		let mut out = vec![0u8; dst_slice * slices];
		for s in 0..slices {
			let src_range = s * src_slice..(s + 1) * src_slice;
			if src_slice == 0 || src_range.end > data.len() {
				break;
			}
			bc::decode_surface(image.format, &data[src_range], w, h, &mut out[s * dst_slice..(s + 1) * dst_slice], image.has_alpha);
		}
		levels.push(out);
	}
	Image { format: PixelFormat::Rgba8, levels, ..image.clone_without_levels() }
}

impl Image {
	fn clone_without_levels(&self) -> Image {
		Image {
			width: self.width,
			height: self.height,
			format: self.format,
			container: self.container,
			levels: Vec::new(),
			has_alpha: self.has_alpha,
			srgb: self.srgb,
			depth: self.depth,
			layers: self.layers,
			cube: self.cube,
			premultiplied_alpha: self.premultiplied_alpha,
			dxt5nm: self.dxt5nm,
			metadata: self.metadata.clone(),
		}
	}
}

/// Mip filter kernel for [`generate_mips_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MipFilter {
	/// Box: the average of the 2x2 (2x2x2) source texels; exact area weights for odd sizes.
	Box,
	/// Triangle (tent): [1, 3, 3, 1] / 8 per axis for even sizes, edges clamped.
	Triangle,
}

/// Replaces the mip chain of an RGBA8 image with a full chain built from level 0, box-filtering
/// the stored values (gamma space for sRGB colour). Block-compressed images are left unchanged
/// ([`to_rgba8`] first). Roblox's transcoder filters its `Linear` textures this way.
pub fn generate_mips(image: &mut Image) {
	mips::generate(image, MipFilter::Box, mips::Space::Stored);
}

/// [`generate_mips`] in linear light: RGB is decoded from sRGB, box-filtered and re-encoded (alpha
/// is averaged as stored). The physically correct box filter for sRGB colour textures.
pub fn generate_mips_srgb(image: &mut Image) {
	mips::generate(image, MipFilter::Box, mips::Space::Linear);
}

/// Mip chain with a chosen kernel, in linear light (`linear_light`: RGB decoded from sRGB) or on
/// the stored values.
///
/// To reproduce Roblox's mips (measured on 1,245 cached Roblox KTX2 files, see `notes/image.md`):
/// colour textures (`colorSpace = Gamma2RGB`) match a triangle kernel in linear light best
/// (`generate_mips_with(image, MipFilter::Triangle, true)`), data textures (`Linear`) a box filter
/// on the stored values ([`generate_mips`]).
pub fn generate_mips_with(image: &mut Image, filter: MipFilter, linear_light: bool) {
	let space = if linear_light { mips::Space::Linear } else { mips::Space::Stored };
	mips::generate(image, filter, space);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sniffs_signatures() {
		assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some(Container::Png));
		assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Container::Jpeg));
		assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Container::Webp));
		assert_eq!(sniff(b"GIF89a.."), Some(Container::Gif));
		assert_eq!(sniff(b"DDS |\0\0\0"), Some(Container::Dds));
		assert_eq!(sniff(&ktx2::IDENTIFIER), Some(Container::Ktx2));
		assert_eq!(sniff(b"version 2.00\n"), None);
		assert_eq!(sniff(b""), None);
		assert!(matches!(decode(b"not an image at all"), Err(ImageError::Unrecognised)));
	}

	#[test]
	fn png_round_trip() {
		use image::ImageEncoder;

		let mut png = Vec::new();
		let pixels: Vec<u8> = (0..3 * 2 * 3).map(|i| i as u8 * 10).collect();
		image::codecs::png::PngEncoder::new(&mut png).write_image(&pixels, 3, 2, image::ExtendedColorType::Rgb8).unwrap();
		let image = decode(&png).unwrap();
		assert_eq!((image.width, image.height, image.format, image.container), (3, 2, PixelFormat::Rgba8, Container::Png));
		assert_eq!(&image.levels[0][0..8], &[0, 10, 20, 255, 30, 40, 50, 255]);
		assert!(!image.has_alpha);
		assert_eq!(image.srgb, None);

		// Truncated data errors
		assert!(decode(&png[..png.len() / 2]).is_err());
	}

	#[test]
	fn to_rgba8_expands_every_level() {
		let levels: Vec<Vec<u8>> = vec![vec![0xff; 4 * 8], vec![0xff; 8], vec![0xff; 8]];
		let image = Image {
			width: 8,
			height: 4,
			format: PixelFormat::Bc1,
			container: Container::Dds,
			levels,
			has_alpha: false,
			srgb: None,
			depth: 1,
			layers: 1,
			cube: false,
			premultiplied_alpha: false,
			dxt5nm: false,
			metadata: Vec::new(),
		};
		let rgba = to_rgba8(&image);
		assert_eq!(rgba.format, PixelFormat::Rgba8);
		let sizes: Vec<usize> = rgba.levels.iter().map(Vec::len).collect();
		assert_eq!(sizes, [8 * 4 * 4, 4 * 2 * 4, 2 * 4]);
		// c0 == c1 == 0xffff: three-colour mode, indices 3 → black, opaque under BC1 RGB semantics
		assert!(rgba.levels[0].chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
	}

	#[test]
	fn from_rgba8() {
		let image = Image::from_rgba8(1, 1, vec![1, 2, 3, 200]);
		assert!(image.has_alpha);
		assert_eq!((image.width, image.format, image.layers, image.depth), (1, PixelFormat::Rgba8, 1, 1));
		assert!(!Image::from_rgba8(1, 1, vec![1, 2, 3, 255]).has_alpha);
	}

	#[test]
	fn tex_extension_hint() {
		let mut file = vec![0u8; 128];
		file[0..4].copy_from_slice(b"DDS ");
		let put = |f: &mut Vec<u8>, offset: usize, v: u32| f[4 + offset..8 + offset].copy_from_slice(&v.to_le_bytes());
		put(&mut file, 0, 124);
		put(&mut file, 8, 4);
		put(&mut file, 12, 4);
		put(&mut file, 24, 1);
		put(&mut file, 76, 0x4);
		file[4 + 80..4 + 84].copy_from_slice(b"DXT1");
		file.extend_from_slice(&[0u8; 8]);
		assert_eq!(decode(&file).unwrap().container, Container::Dds);
		assert_eq!(decode_with_extension(&file, "TEX").unwrap().container, Container::RobloxTex);
	}
}
