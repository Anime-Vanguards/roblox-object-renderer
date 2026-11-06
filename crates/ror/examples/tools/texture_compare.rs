// An image asset's upload against its KTX2 transcode (what ror draws for particles, beams and decals):
// size, alpha, and the mean RGB where alpha is high, for each URI; writes both level-0 images as PNG
// into <out dir>:
//   cargo run -p ror --release --example tools -- texture_compare <out dir> <uri>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let stats = |rgba: &[u8]| -> (f32, [f32; 3], [f32; 3]) {
		let (mut alpha, mut all, mut solid, mut count) = (0.0f64, [0.0f64; 3], [0.0f64; 3], 0.0f64);
		let pixels = (rgba.len() / 4).max(1) as f64;
		for texel in rgba.chunks_exact(4) {
			alpha += texel[3] as f64;
			for channel in 0..3 {
				all[channel] += texel[channel] as f64;
			}
			if texel[3] >= 128 {
				count += 1.0;
				for channel in 0..3 {
					solid[channel] += texel[channel] as f64;
				}
			}
		}
		((alpha / pixels / 255.0) as f32, all.map(|value| (value / pixels) as f32), solid.map(|value| (value / count.max(1.0)) as f32))
	};
	for uri in args {
		let name: String = uri.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
		match assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{error}")).and_then(|fetched| ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{error}"))) {
			Ok(image) => {
				let rgba = ror_image::to_rgba8(&image);
				let (alpha, all, solid) = stats(&rgba.levels[0]);
				ror::output::write_png(&out.join(format!("{name}-upload.png")), rgba.width, rgba.height, &rgba.levels[0], true)?;
				println!("{uri} upload {}x{} alpha {alpha:.3} rgb {all:.0?} where opaque {solid:.0?}", image.width, image.height);
			}
			Err(error) => println!("{uri} upload: {error}"),
		}
		let decode = |pack: u32| -> anyhow::Result<ror_image::Image> {
			let fetched = assets.fetch_transcode(&uri, ror_assets::Transcode::Image, pack).map_err(|error| anyhow::anyhow!("{error}"))?;
			ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{error}"))
		};
		let transcode = decode(0).and_then(|first| {
			let packs = ror_image::ktx2_pack_table(&first).len() as u32;
			if packs == 0 {
				return Ok(first);
			}
			let mut images = vec![first];
			for pack in 1..packs {
				images.push(decode(pack)?);
			}
			ror_image::join_packs(images).map_err(|error| anyhow::anyhow!("{error}"))
		});
		match transcode {
			Ok(image) => {
				let rgba = ror_image::to_rgba8(&image);
				let (alpha, all, solid) = stats(&rgba.levels[0]);
				ror::output::write_png(&out.join(format!("{name}-transcode.png")), rgba.width, rgba.height, &rgba.levels[0], true)?;
				println!("{uri} transcode {}x{} alpha {alpha:.3} rgb {all:.0?} where opaque {solid:.0?}", image.width, image.height);
			}
			Err(error) => println!("{uri} transcode: {error}"),
		}
	}
	Ok(())
}
