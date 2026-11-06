// 64 x 64 thumbnails of images for matching Roblox's transcodes against their sources
// (tools/analysis/transcodes.py): reads one image path per line on stdin (RBXH-wrapped entries of
// Roblox's HTTP cache or plain files), appends each level 0's thumbnail to <out> (f32, little
// endian) and its path and size to <out>.txt.
//
//     cargo run --release -p ror --example tools -- transcode_thumbs <out> [srgb|square|raw4] < paths
//
// srgb / square: one linear-light gray thumbnail, texels decoded with the sRGB curve or a square
// (Roblox's Gamma2RGB transcodes); raw4: four thumbnails of the stored R, G, B, A (0..1).
use std::io::{BufRead, Write};

fn rbxh_content(data: &[u8]) -> Option<&[u8]> {
	if data.get(..4) != Some(b"RBXH") {
		return Some(data);
	}
	let u32_at = |pos: usize| data.get(pos..pos + 4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize);
	let url_len = u32_at(8)?;
	let pos = 12 + url_len + 1;
	let header_len = u32_at(pos + 4)?;
	let content_len = u32_at(pos + 12)?;
	let start = pos + 24 + header_len;
	data.get(start..start + content_len)
}

pub fn main() -> anyhow::Result<()> {
	let out = std::env::args().nth(1).expect("out file");
	let mode = std::env::args().nth(2).unwrap_or_else(|| "srgb".into());
	let mut file = std::fs::File::create(&out)?;
	let mut names = std::fs::File::create(format!("{out}.txt"))?;
	for line in std::io::stdin().lock().lines() {
		let path = line?;
		let Ok(bytes) = std::fs::read(path.trim()) else {
			continue;
		};
		let Some(image) = rbxh_content(&bytes).and_then(|body| ror_image::decode(body).ok()) else {
			continue;
		};
		let rgba = ror_image::to_rgba8(&image);
		let (width, height) = (rgba.width as usize, rgba.height as usize);
		if width < 64 || height < 64 {
			continue;
		}
		let level = &rgba.levels[0];
		let planes = if mode == "raw4" { 4 } else { 1 };
		let mut thumb = vec![0f32; 64 * 64 * planes];
		for y in 0..height {
			for x in 0..width {
				let texel = (y * width + x) * 4;
				let cell = (y * 64 / height) * 64 + x * 64 / width;
				if mode == "raw4" {
					for channel in 0..4 {
						thumb[channel * 4096 + cell] += level[texel + channel] as f32 / 255.0;
					}
					continue;
				}
				let decode = |value: f32| match mode.as_str() {
					"square" => value * value,
					_ if value <= 0.04045 => value / 12.92,
					_ => ((value + 0.055) / 1.055).powf(2.4),
				};
				thumb[cell] += (0..3).map(|channel| decode(level[texel + channel] as f32 / 255.0)).sum::<f32>() / 3.0;
			}
		}
		let scale = 4096.0 / (width * height) as f32;
		for value in &mut thumb {
			*value *= scale;
		}
		file.write_all(bytemuck::cast_slice(&thumb))?;
		writeln!(names, "{} {width}x{height}", path.trim())?;
	}
	Ok(())
}
