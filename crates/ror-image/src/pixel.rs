//! Uncompressed pixel formats (from DDS and KTX2) converted to RGBA8.
//!
//! Channels a format lacks read like a GPU samples them: missing colour channels 0, missing alpha
//! 255 (`R8` → (r, 0, 0, 255), `A8` → (0, 0, 0, a)); luminance formats replicate into RGB. Float
//! formats are clamped to [0, 1] and rounded, losing precision (e.g. a 16-bit float BRDF table).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Uncompressed {
	Rgba8,
	Bgra8,
	/// BGRA with an unused alpha byte
	Bgrx8,
	Rgb8,
	Bgr8,
	R8,
	Rg8,
	A8,
	Rgb10A2,
	Rgba16,
	Rg16,
	R16,
	Rgba16Float,
	Rg16Float,
	R16Float,
	Rgba32Float,
	R32Float,
	/// Little-endian `bits`-bit pixels with channel masks (R, G, B, A); luminance replicates R
	Masked { bits: u32, masks: [u32; 4], luminance: bool },
}

impl Uncompressed {
	pub(crate) fn bytes_per_pixel(self) -> usize {
		match self {
			Uncompressed::R8 | Uncompressed::A8 => 1,
			Uncompressed::Rg8 | Uncompressed::R16 | Uncompressed::R16Float => 2,
			Uncompressed::Rgb8 | Uncompressed::Bgr8 => 3,
			Uncompressed::Rgba8 | Uncompressed::Bgra8 | Uncompressed::Bgrx8 | Uncompressed::Rgb10A2 | Uncompressed::Rg16 | Uncompressed::Rg16Float | Uncompressed::R32Float => 4,
			Uncompressed::Rgba16 | Uncompressed::Rgba16Float => 8,
			Uncompressed::Rgba32Float => 16,
			Uncompressed::Masked { bits, .. } => (bits as usize).div_ceil(8),
		}
	}

	/// Whether the format stores alpha.
	pub(crate) fn has_alpha(self) -> bool {
		match self {
			Uncompressed::Rgba8 | Uncompressed::Bgra8 | Uncompressed::A8 | Uncompressed::Rgb10A2 | Uncompressed::Rgba16 | Uncompressed::Rgba16Float | Uncompressed::Rgba32Float => true,
			Uncompressed::Masked { masks, .. } => masks[3] != 0,
			_ => false,
		}
	}
}

/// Appends `src` (whole pixels of `format`) to `out` as RGBA8.
pub(crate) fn convert_into(format: Uncompressed, src: &[u8], out: &mut Vec<u8>) {
	let bpp = format.bytes_per_pixel();
	let pixels = src.len() / bpp;
	out.reserve(pixels * 4);
	let src = &src[..pixels * bpp];
	match format {
		Uncompressed::Rgba8 => out.extend_from_slice(src),
		Uncompressed::Bgra8 => out.extend(src.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]])),
		Uncompressed::Bgrx8 => out.extend(src.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255])),
		Uncompressed::Rgb8 => out.extend(src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255])),
		Uncompressed::Bgr8 => out.extend(src.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0], 255])),
		Uncompressed::R8 => out.extend(src.iter().flat_map(|&r| [r, 0, 0, 255])),
		Uncompressed::Rg8 => out.extend(src.chunks_exact(2).flat_map(|p| [p[0], p[1], 0, 255])),
		Uncompressed::A8 => out.extend(src.iter().flat_map(|&a| [0, 0, 0, a])),
		Uncompressed::Rgb10A2 => out.extend(src.chunks_exact(4).flat_map(|p| {
			let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
			let c = |shift: u32| scale_bits((v >> shift) & 0x3ff, 10);
			[c(0), c(10), c(20), ((v >> 30) * 85) as u8]
		})),
		Uncompressed::Rgba16 => out.extend(src.chunks_exact(8).flat_map(|p| [u16n(p, 0), u16n(p, 2), u16n(p, 4), u16n(p, 6)])),
		Uncompressed::Rg16 => out.extend(src.chunks_exact(4).flat_map(|p| [u16n(p, 0), u16n(p, 2), 0, 255])),
		Uncompressed::R16 => out.extend(src.chunks_exact(2).flat_map(|p| [u16n(p, 0), 0, 0, 255])),
		Uncompressed::Rgba16Float => out.extend(src.chunks_exact(8).flat_map(|p| [f16n(p, 0), f16n(p, 2), f16n(p, 4), f16n(p, 6)])),
		Uncompressed::Rg16Float => out.extend(src.chunks_exact(4).flat_map(|p| [f16n(p, 0), f16n(p, 2), 0, 255])),
		Uncompressed::R16Float => out.extend(src.chunks_exact(2).flat_map(|p| [f16n(p, 0), 0, 0, 255])),
		Uncompressed::Rgba32Float => out.extend(src.chunks_exact(16).flat_map(|p| [f32n(p, 0), f32n(p, 4), f32n(p, 8), f32n(p, 12)])),
		Uncompressed::R32Float => out.extend(src.chunks_exact(4).flat_map(|p| [f32n(p, 0), 0, 0, 255])),
		Uncompressed::Masked { bits, masks, luminance } => convert_masked(src, bits, masks, luminance, out),
	}
}

fn convert_masked(src: &[u8], bits: u32, masks: [u32; 4], luminance: bool, out: &mut Vec<u8>) {
	struct Channel {
		mask: u32,
		shift: u32,
		width: u32,
	}

	let channel = |mask: u32| {
		let shift = if mask == 0 { 0 } else { mask.trailing_zeros() };
		let width = (mask >> shift).count_ones().min(32);
		Channel { mask, shift, width }
	};
	let channels = [channel(masks[0]), channel(masks[1]), channel(masks[2]), channel(masks[3])];
	let extract = |c: &Channel, v: u32, default: u8| -> u8 {
		if c.mask == 0 || c.width == 0 {
			default
		} else {
			scale_bits((v & c.mask) >> c.shift, c.width)
		}
	};

	let bpp = (bits as usize).div_ceil(8);
	for p in src.chunks_exact(bpp) {
		let mut raw = [0u8; 4];
		raw[..bpp].copy_from_slice(p);
		let v = u32::from_le_bytes(raw);
		let a = extract(&channels[3], v, 255);
		if luminance {
			let l = extract(&channels[0], v, 0);
			out.extend_from_slice(&[l, l, l, a]);
		} else {
			out.extend_from_slice(&[extract(&channels[0], v, 0), extract(&channels[1], v, 0), extract(&channels[2], v, 0), a]);
		}
	}
}

/// Rescales an unsigned `width`-bit value to 8 bits with rounding.
#[inline]
fn scale_bits(value: u32, width: u32) -> u8 {
	match width {
		0 => 0,
		8 => value as u8,
		1..=31 => {
			let max = (1u64 << width) - 1;
			((value as u64 * 255 + max / 2) / max) as u8
		}
		_ => (value >> 24) as u8,
	}
}

#[inline]
fn u16n(p: &[u8], offset: usize) -> u8 {
	let v = u16::from_le_bytes([p[offset], p[offset + 1]]) as u32;
	((v * 255 + 32767) / 65535) as u8
}

#[inline]
fn f16n(p: &[u8], offset: usize) -> u8 {
	unit_to_u8(f16_to_f32(u16::from_le_bytes([p[offset], p[offset + 1]])))
}

#[inline]
fn f32n(p: &[u8], offset: usize) -> u8 {
	unit_to_u8(f32::from_le_bytes([p[offset], p[offset + 1], p[offset + 2], p[offset + 3]]))
}

#[inline]
fn unit_to_u8(v: f32) -> u8 {
	// NaN maps to 0
	if v >= 0.0 { (v.min(1.0) * 255.0 + 0.5) as u8 } else { 0 }
}

/// IEEE 754 binary16 to binary32.
pub(crate) fn f16_to_f32(h: u16) -> f32 {
	let sign = ((h >> 15) as u32) << 31;
	let exponent = ((h >> 10) & 0x1f) as u32;
	let mantissa = (h & 0x3ff) as u32;
	let bits = match (exponent, mantissa) {
		(0, 0) => sign,
		(0, m) => {
			// Subnormal: normalise
			let shift = m.leading_zeros() - 21;
			let m = (m << shift) & 0x3ff;
			let e = 127 - 15 + 1 - shift;
			sign | (e << 23) | (m << 13)
		}
		(0x1f, m) => sign | 0x7f80_0000 | (m << 13),
		(e, m) => sign | ((e + 127 - 15) << 23) | (m << 13),
	};
	f32::from_bits(bits)
}

/// True when any RGBA8 pixel has alpha below 255.
pub(crate) fn any_transparent_rgba8(data: &[u8]) -> bool {
	// Branch-free over 16-pixel chunks so the loop vectorises
	let mut chunks = data.chunks_exact(64);
	for chunk in &mut chunks {
		let mut all = 0xffu8;
		for p in chunk.chunks_exact(4) {
			all &= p[3];
		}
		if all != 0xff {
			return true;
		}
	}
	chunks.remainder().chunks_exact(4).any(|p| p[3] != 0xff)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn half_floats() {
		assert_eq!(f16_to_f32(0x0000), 0.0);
		assert_eq!(f16_to_f32(0x3c00), 1.0);
		assert_eq!(f16_to_f32(0xc000), -2.0);
		assert_eq!(f16_to_f32(0x3800), 0.5);
		assert_eq!(f16_to_f32(0x7bff), 65504.0);
		assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
		assert_eq!(f16_to_f32(0x03ff), 1023.0 * 2f32.powi(-24));
		assert!(f16_to_f32(0x7c00).is_infinite());
		assert!(f16_to_f32(0x7e00).is_nan());
	}

	#[test]
	fn conversions() {
		let mut out = Vec::new();
		convert_into(Uncompressed::Rg16Float, &[0x00, 0x3c, 0x00, 0x38], &mut out);
		assert_eq!(out, [255, 128, 0, 255]);

		out.clear();
		convert_into(Uncompressed::Masked { bits: 16, masks: [0x0f00, 0x00f0, 0x000f, 0xf000], luminance: false }, &0x8f40u16.to_le_bytes(), &mut out);
		assert_eq!(out, [255, 68, 0, 136]);

		out.clear();
		convert_into(Uncompressed::Rgb10A2, &(0x3ff | (0x200 << 10) | (3 << 30) as u32).to_le_bytes(), &mut out);
		assert_eq!(out, [255, 128, 0, 255]);

		assert!(any_transparent_rgba8(&[0, 0, 0, 255, 0, 0, 0, 254]));
		let opaque = vec![255u8; 4 * 100];
		assert!(!any_transparent_rgba8(&opaque));
		let mut late = opaque.clone();
		late[4 * 99 + 3] = 0;
		assert!(any_transparent_rgba8(&late));
	}
}
