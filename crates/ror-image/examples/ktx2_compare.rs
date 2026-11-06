//! Compares Roblox's KTX2 transcodes of images with ror's own pipeline for the uploaded source
//! (PNG decode, then `generate_mips_with` Triangle in linear light for colour, box mips plus BC5 /
//! BC4 for normal / scalar maps), level by level, texel by texel.
//!
//! ```text
//! cargo run -p ror-image --release --example ktx2_compare -- <manifest> [--dump DIR]
//! ```
//!
//! Each manifest line: `name|usage|channels|source|pack,pack,...` with usage `color`, `normal` or
//! `scalar`; `channels` the KTX2 channels compared with the source's (`rgb`, `ag` for a DXT5nm
//! normal map's X and Y, `rg`, or one of `r`, `g`, `b`, `a`); `source` the PNG (or any decodable
//! file) and the packs the transcode's files (any order). Prints per level: sizes, mean absolute,
//! RMS and largest difference in 8-bit steps, the mean signed difference (KTX2 - ror) and the share
//! of texels off by more than 2, per compared channel. `--dump` writes both sides' levels as PNG.

use ror_image::{Image, MipFilter, PixelFormat};

struct Stats {
	n: u64,
	abs: f64,
	sq: f64,
	signed: f64,
	max: u32,
	over2: u64,
}

impl Stats {
	fn new() -> Self {
		Stats { n: 0, abs: 0.0, sq: 0.0, signed: 0.0, max: 0, over2: 0 }
	}

	fn add(&mut self, a: u8, b: u8) {
		let d = a as i32 - b as i32;
		self.n += 1;
		self.abs += d.unsigned_abs() as f64;
		self.sq += (d * d) as f64;
		self.signed += d as f64;
		self.max = self.max.max(d.unsigned_abs());
		if d.unsigned_abs() > 2 {
			self.over2 += 1;
		}
	}

	fn line(&self) -> String {
		let n = self.n.max(1) as f64;
		format!("mae {:5.2} rms {:5.2} max {:3} signed {:+6.2} >2 {:5.1} %", self.abs / n, (self.sq / n).sqrt(), self.max, self.signed / n, self.over2 as f64 / n * 100.0)
	}
}

fn channel_index(c: char) -> usize {
	match c {
		'r' => 0,
		'g' => 1,
		'b' => 2,
		'a' => 3,
		_ => panic!("channel {c}"),
	}
}

fn srgb_to_linear(c: f64) -> f64 {
	if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn ror_pipeline(source: &Image, usage: &str) -> Image {
	if usage == "asis" {
		// the source's own levels (e.g. a whole-chain transcode against its packs)
		return ror_image::to_rgba8(source);
	}
	let mut image = ror_image::to_rgba8(source);
	image.levels.truncate(1);
	if usage.starts_with("gamma2") {
		// sRGB texels re-encoded with a gamma of 2: c' = sqrt(linear(c))
		let table: Vec<u8> = (0..256).map(|c| (srgb_to_linear(c as f64 / 255.0).sqrt() * 255.0).round() as u8).collect();
		for texel in image.levels[0].chunks_exact_mut(4) {
			for c in &mut texel[..3] {
				*c = table[*c as usize];
			}
		}
	}
	match usage {
		"color" => ror_image::generate_mips_with(&mut image, MipFilter::Triangle, true),
		"color-box" => ror_image::generate_mips(&mut image),
		"gamma2-triangle" => ror_image::generate_mips_with(&mut image, MipFilter::Triangle, false),
		_ => ror_image::generate_mips(&mut image),
	}
	let compressed = match usage {
		"normal" => ror_image::encode_bc5(&image),
		"scalar" => ror_image::encode_bc4(&image),
		_ => None,
	};
	ror_image::to_rgba8(&compressed.unwrap_or(image))
}

fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
	use image::ImageEncoder;
	let file = std::fs::File::create(path).expect("create png");
	image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file)).write_image(rgba, width, height, image::ExtendedColorType::Rgba8).expect("write png");
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let manifest = args.first().expect("usage: ktx2_compare <manifest> [--dump DIR]");
	let dump = args.iter().position(|a| a == "--dump").map(|i| std::path::PathBuf::from(&args[i + 1]));
	let text = std::fs::read_to_string(manifest).expect("manifest");
	for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
		let fields: Vec<&str> = line.split('|').collect();
		let [name, usage, channels, source, packs] = fields[..] else {
			panic!("bad manifest line {line}");
		};
		let source_image = match std::fs::read(source).map_err(|e| e.to_string()).and_then(|b| ror_image::decode(&b).map_err(|e| e.to_string())) {
			Ok(image) => image,
			Err(error) => {
				println!("{name}: source {source}: {error}");
				continue;
			}
		};
		let pack_images: Result<Vec<Image>, String> = packs.split(',').map(|p| std::fs::read(p).map_err(|e| e.to_string()).and_then(|b| ror_image::decode(&b).map_err(|e| e.to_string()))).collect();
		let joined = match pack_images.and_then(|p| ror_image::join_packs(p).map_err(|e| e.to_string())) {
			Ok(image) => image,
			Err(error) => {
				println!("{name}: packs: {error}");
				continue;
			}
		};
		let ktx = ror_image::to_rgba8(&joined);
		let ror = ror_pipeline(&source_image, usage);
		let (ktx_channels, ror_channels): (Vec<usize>, Vec<usize>) = match channels {
			"rgb" => (vec![0, 1, 2], vec![0, 1, 2]),
			"rgba" => (vec![0, 1, 2, 3], vec![0, 1, 2, 3]),
			"ag" => (vec![3, 1], vec![0, 1]),
			"rg" => (vec![0, 1], vec![0, 1]),
			one => (vec![channel_index(one.chars().next().unwrap_or('r'))], vec![0]),
		};
		println!(
			"{name} [{usage}] source {}x{} -> ktx2 {:?} {}x{} {} levels, colorSpace {}, dxt5nm {}, alpha {}",
			source_image.width,
			source_image.height,
			joined.format,
			joined.width,
			joined.height,
			joined.levels.len(),
			joined.meta("colorSpace").unwrap_or("-"),
			joined.dxt5nm,
			joined.has_alpha
		);
		// Pair levels of equal size
		for (k, ktx_level) in ktx.levels.iter().enumerate() {
			let (kw, kh, _) = ktx.level_size(k);
			let Some(j) = (0..ror.levels.len()).find(|&j| ror.level_size(j).0 == kw && ror.level_size(j).1 == kh) else {
				continue;
			};
			let ror_level = &ror.levels[j];
			let mut per = Vec::new();
			for (&kc, &rc) in ktx_channels.iter().zip(&ror_channels) {
				let mut s = Stats::new();
				for (a, b) in ktx_level.chunks_exact(4).zip(ror_level.chunks_exact(4)) {
					s.add(a[kc], b[rc]);
				}
				per.push(s);
			}
			let mut all = Stats::new();
			for s in &per {
				all.n += s.n;
				all.abs += s.abs;
				all.sq += s.sq;
				all.signed += s.signed;
				all.max = all.max.max(s.max);
				all.over2 += s.over2;
			}
			println!("  level {k:2} {kw:5}x{kh:<5} (ror level {j:2}) all: {}", all.line());
			if per.len() > 1 {
				for (s, c) in per.iter().zip(channels.chars()) {
					println!("        {c}: {}", s.line());
				}
			}
			if let Some(dir) = &dump {
				std::fs::create_dir_all(dir).expect("dump dir");
				write_png(&dir.join(format!("{name}_ktx2_{k}.png")), kw, kh, ktx_level);
				write_png(&dir.join(format!("{name}_ror_{k}.png")), kw, kh, ror_level);
			}
		}
		if joined.format == PixelFormat::Rgba8 {
			println!("  (transcode decoded to RGBA8)");
		}
	}
}
