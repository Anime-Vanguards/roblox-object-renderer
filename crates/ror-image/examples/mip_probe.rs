//! Which mip filter does Roblox's texture transcoder use? For every cached Roblox KTX2 file that
//! holds two consecutive levels, rebuild level N+1 from the decoded level N with candidate filters
//! and compare with Roblox's own (decoded) level N+1. BC compression noise is in both levels, so
//! the right filter is the one with the clearly lowest error, not zero error.
//!
//! ```text
//! cargo run -p ror-image --release --example mip_probe -- [--storage DIR]
//! ```

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::PathBuf;

use ror_image::{Container, Image};

fn rbxh_content(bytes: &[u8]) -> Option<&[u8]> {
	let u32_at = |o: usize| -> Option<u32> { bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
	if bytes.get(0..4)? != b"RBXH" {
		return None;
	}
	let p = 12 + u32_at(8)? as usize + 1;
	let header_len = u32_at(p + 4)? as usize;
	let content_len = u32_at(p + 12)? as usize;
	let start = p + 24 + header_len;
	bytes.get(start..start + content_len)
}

fn srgb_to_linear(c: f32) -> f32 {
	let c = c / 255.0;
	if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(l: f32) -> f32 {
	let l = l.clamp(0.0, 1.0);
	let s = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
	s * 255.0
}

/// Taps (source offset d relative to 2*x, weight) of a 2:1 reduction with `kernel(t)`, t in
/// output texels from the output texel centre, support `radius` output texels.
fn taps(kernel: &dyn Fn(f64) -> f64, radius: f64) -> Vec<(isize, f32)> {
	let reach = (2.0 * radius).ceil() as isize + 1;
	let mut out = Vec::new();
	for d in -reach..=reach {
		let t = (d as f64 + 0.5 - 1.0) / 2.0;
		let w = if t.abs() < radius { kernel(t) } else { 0.0 };
		if w != 0.0 {
			out.push((d, w as f32));
		}
	}
	let sum: f32 = out.iter().map(|t| t.1).sum();
	out.iter().map(|&(d, w)| (d, w / sum)).collect()
}

/// Halves an even-sized RGBA image with separable taps (edge texels clamped), optionally in
/// linear light for RGB. Returns 8-bit-scale values.
fn downsample(src: &[u8], w: usize, h: usize, taps: &[(isize, f32)], linear: bool) -> Vec<f32> {
	let (w2, h2) = (w / 2, h / 2);
	let plane: Vec<f32> = src.iter().enumerate().map(|(i, &v)| if linear && i % 4 < 3 { srgb_to_linear(v as f32) } else { v as f32 / 255.0 }).collect();
	let mut tmp = vec![0f32; w2 * h * 4];
	for y in 0..h {
		for x in 0..w2 {
			let mut acc = [0f32; 4];
			for &(d, t) in taps {
				let sx = (2 * x as isize + d).clamp(0, w as isize - 1) as usize;
				let i = (y * w + sx) * 4;
				for c in 0..4 {
					acc[c] += plane[i + c] * t;
				}
			}
			tmp[(y * w2 + x) * 4..(y * w2 + x) * 4 + 4].copy_from_slice(&acc);
		}
	}
	let mut out = vec![0f32; w2 * h2 * 4];
	for y in 0..h2 {
		for x in 0..w2 {
			let mut acc = [0f32; 4];
			for &(d, t) in taps {
				let sy = (2 * y as isize + d).clamp(0, h as isize - 1) as usize;
				let i = (sy * w2 + x) * 4;
				for c in 0..4 {
					acc[c] += tmp[i + c] * t;
				}
			}
			out[(y * w2 + x) * 4..(y * w2 + x) * 4 + 4].copy_from_slice(&acc);
		}
	}
	out.iter().enumerate().map(|(i, &v)| if linear && i % 4 < 3 { linear_to_srgb(v) } else { v * 255.0 }).collect()
}

fn sinc(x: f64) -> f64 {
	if x == 0.0 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

fn cubic(b: f64, c: f64) -> impl Fn(f64) -> f64 {
	move |t: f64| {
		let x = t.abs();
		let v = if x < 1.0 {
			(12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b)
		} else if x < 2.0 {
			(-b - 6.0 * c) * x * x * x + (6.0 * b + 30.0 * c) * x * x + (-12.0 * b - 48.0 * c) * x + (8.0 * b + 24.0 * c)
		} else {
			0.0
		};
		v / 6.0
	}
}

fn kaiser(width: f64, alpha: f64) -> impl Fn(f64) -> f64 {
	fn bessel_i0(x: f64) -> f64 {
		let mut sum = 1.0;
		let mut term = 1.0;
		for k in 1..30 {
			term *= (x / 2.0) * (x / 2.0) / (k * k) as f64;
			sum += term;
		}
		sum
	}
	move |t: f64| {
		let r = t / (width / 2.0);
		sinc(t) * bessel_i0(alpha * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(alpha)
	}
}

fn main() {
	let mut args = std::env::args().skip(1);
	let mut storage = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Roblox").join("rbx-storage"));
	while let Some(arg) = args.next() {
		if arg == "--storage" {
			storage = args.next().map(PathBuf::from);
		}
	}
	let Some(storage) = storage else {
		eprintln!("no storage dir");
		return;
	};

	let kernels: Vec<(&str, Vec<(isize, f32)>)> = vec![
		("box", taps(&|_| 1.0, 0.5)),
		("triangle", taps(&|t: f64| 1.0 - t.abs(), 1.0)),
		("mitchell", taps(&cubic(1.0 / 3.0, 1.0 / 3.0), 2.0)),
		("catmull-rom", taps(&cubic(0.0, 0.5), 2.0)),
		("b-spline", taps(&cubic(1.0, 0.0), 2.0)),
		("lanczos2", taps(&|t: f64| sinc(t) * sinc(t / 2.0), 2.0)),
		("lanczos3", taps(&|t: f64| sinc(t) * sinc(t / 3.0), 3.0)),
		("kaiser3", taps(&kaiser(3.0, 4.0), 1.5)),
		("gauss0.5", taps(&|t: f64| (-2.0 * t * t / (0.5f64 * 0.5)).exp(), 1.5)),
	];
	// (name, taps, linear light)
	type Candidate = (String, Vec<(isize, f32)>, bool);
	let mut candidates: Vec<Candidate> = Vec::new();
	for (name, t) in &kernels {
		candidates.push((format!("{name} gamma"), t.clone(), false));
		candidates.push((format!("{name} linear"), t.clone(), true));
	}

	let mut totals: BTreeMap<String, Vec<(f64, u64, usize)>> = BTreeMap::new();
	let mut files = 0;
	for sub in std::fs::read_dir(&storage).into_iter().flatten().flatten() {
		for f in std::fs::read_dir(sub.path()).into_iter().flatten().flatten() {
			let Ok(bytes) = std::fs::read(f.path()) else { continue };
			let Some(content) = rbxh_content(&bytes) else { continue };
			let Ok(image) = ror_image::decode(content) else { continue };
			if image.container != Container::Ktx2 || image.levels.len() < 2 || image.dxt5nm {
				continue;
			}
			let rgba: Image = ror_image::to_rgba8(&image);
			let class = format!("{:?} {}", image.format, image.meta("colorSpace").unwrap_or("?"));
			for level in 0..rgba.levels.len() - 1 {
				let (w, h, _) = rgba.level_size(level);
				if w % 2 != 0 || h % 2 != 0 || w < 16 || h < 16 {
					continue;
				}
				let reference = &rgba.levels[level + 1];
				let mut errors = Vec::new();
				for (_, t, linear) in &candidates {
					let ours = downsample(&rgba.levels[level], w as usize, h as usize, t, *linear);
					let err: f64 = ours.iter().zip(reference.iter()).enumerate().filter(|(i, _)| i % 4 < 3).map(|(_, (&a, &b))| (a.round() as f64 - b as f64).abs()).sum();
					errors.push(err);
				}
				let texels = (w as u64 / 2) * (h as u64 / 2) * 3;
				let best = errors.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap_or(0);
				let entry = totals.entry(class.clone()).or_insert_with(|| vec![(0.0, 0, 0); candidates.len()]);
				for (i, e) in errors.iter().enumerate() {
					entry[i].0 += e;
					entry[i].1 += texels;
				}
				entry[best].2 += 1;
			}
			files += 1;
		}
	}

	println!("{files} KTX2 files with two or more levels (DXT5nm normal maps excluded)");
	for (class, stats) in &totals {
		println!("{class}:");
		let mut rows: Vec<(f64, &str, usize)> = candidates.iter().enumerate().map(|(i, (name, _, _))| (stats[i].0 / stats[i].1.max(1) as f64, name.as_str(), stats[i].2)).collect();
		rows.sort_by(|a, b| a.0.total_cmp(&b.0));
		for (err, name, wins) in rows {
			println!("  {name:20} mean abs error {err:.3} (8-bit steps), best for {wins} level pairs");
		}
	}
}
