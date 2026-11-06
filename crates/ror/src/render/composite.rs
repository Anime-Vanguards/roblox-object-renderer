//! Character compositing (notes/render.md "Characters"): a Humanoid's body colours, Pants, Shirt and
//! ShirtGraphic drawn into the texture its body parts sample, with Studio's own compositing meshes
//! (`content/avatar/compositing`). Each of those meshes is a sheet whose positions are pixels of the
//! target texture (y up) and whose uv points into the clothing template: the R6 atlas (1024 x 512)
//! holds every limb, R15 has one texture per limb group (the torso 388 x 272, an arm or leg 264 x
//! 284). Drawn on the CPU, once per distinct composite (the texture cache keeps it).

use anyhow::{Context, Result, anyhow};

use super::Renderer;
use super::resources::{GpuTexture, TextureUse};
use crate::scene::character::{Composite, Layer, Layout};

/// A composited texture's URI: the [`Composite`] it is made of, serialised (the material key names
/// it like any texture and [`Renderer::load_texture`] builds it).
pub const KEY_PREFIX: &str = "ror-composite:";

/// The texture URI standing for a composite.
pub fn key(composite: &Composite) -> String {
	format!("{KEY_PREFIX}{}", serde_json::to_string(composite).unwrap_or_default())
}

fn from_key(key: &str) -> Option<Composite> {
	serde_json::from_str(key.strip_prefix(KEY_PREFIX)?).ok()
}

/// A compositing mesh: positions in target pixels (y up), uv in its source image.
struct Sheet {
	positions: Vec<[f32; 2]>,
	uvs: Vec<[f32; 2]>,
	triangles: Vec<[u32; 3]>,
}

impl Sheet {
	fn from_mesh(mesh: &ror_mesh::Mesh) -> Sheet {
		let flip = mesh.uv_v_flipped();
		let uvs = mesh.uvs.iter().map(|&[u, v]| if flip { [u, 1.0 - v] } else { [u, v] }).collect();
		let lod = mesh.lod0();
		let triangles = lod.chunks_exact(3).map(|tri| [tri[0], tri[1], tri[2]]).collect();
		Sheet { positions: mesh.positions.iter().map(|p| [p[0], p[1]]).collect(), uvs, triangles }
	}

	/// A rectangle (x0, y0, x1, y1, y up) showing the whole source upright.
	fn quad([x0, y0, x1, y1]: [f32; 4]) -> Sheet {
		Sheet { positions: vec![[x0, y1], [x1, y1], [x1, y0], [x0, y0]], uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], triangles: vec![[0, 1, 2], [0, 2, 3]] }
	}
}

/// An RGBA image in stored (gamma) values 0..1, straight alpha.
pub struct Canvas {
	pub width: u32,
	pub height: u32,
	pub pixels: Vec<[f32; 4]>,
}

impl Canvas {
	fn new(width: u32, height: u32) -> Canvas {
		Canvas { width, height, pixels: vec![[0.0; 4]; (width * height) as usize] }
	}

	pub fn to_rgba8(&self) -> Vec<u8> {
		self.pixels.iter().flat_map(|pixel| pixel.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
	}
}

/// A source image for a layer, RGBA8.
struct Source {
	width: u32,
	height: u32,
	pixels: Vec<u8>,
}

impl Source {
	/// Bilinear, clamped to the edges, at uv (0..1, v down).
	fn sample(&self, u: f32, v: f32) -> [f32; 4] {
		let x = (u * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
		let y = (v * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
		let (x0, y0) = (x.floor() as u32, y.floor() as u32);
		let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
		let (fx, fy) = (x - x0 as f32, y - y0 as f32);
		let texel = |x: u32, y: u32| -> [f32; 4] {
			let index = ((y * self.width + x) * 4) as usize;
			[0, 1, 2, 3].map(|channel| self.pixels[index + channel] as f32 / 255.0)
		};
		let (a, b, c, d) = (texel(x0, y0), texel(x1, y0), texel(x0, y1), texel(x1, y1));
		[0, 1, 2, 3].map(|channel| {
			let top = a[channel] + (b[channel] - a[channel]) * fx;
			let bottom = c[channel] + (d[channel] - c[channel]) * fx;
			top + (bottom - top) * fy
		})
	}
}

enum Fill<'a> {
	Solid([f32; 3]),
	Image(&'a Source, [f32; 3]),
}

/// Draws a sheet into the canvas (positions scaled by `scale`), blending its fill over what is there.
/// Pixel centres inside a triangle are covered, edges by the top-left rule (shared edges once).
fn draw(canvas: &mut Canvas, sheet: &Sheet, units_high: f32, scale: f32, fill: &Fill) {
	let (width, height) = (canvas.width as i32, canvas.height as i32);
	for triangle in &sheet.triangles {
		let corner = |index: u32| {
			let [x, y] = sheet.positions[index as usize];
			// y up in the sheet, rows down in the canvas
			(x * scale, (units_high - y) * scale)
		};
		let (mut a, mut b, c) = (corner(triangle[0]), corner(triangle[1]), corner(triangle[2]));
		let (mut uv_a, mut uv_b, uv_c) = (sheet.uvs[triangle[0] as usize], sheet.uvs[triangle[1] as usize], sheet.uvs[triangle[2] as usize]);
		let mut area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
		if area.abs() < 1e-9 {
			continue;
		}
		if area < 0.0 {
			std::mem::swap(&mut a, &mut b);
			std::mem::swap(&mut uv_a, &mut uv_b);
			area = -area;
		}
		let x0 = (a.0.min(b.0).min(c.0).floor() as i32).max(0);
		let x1 = (a.0.max(b.0).max(c.0).ceil() as i32).min(width - 1);
		let y0 = (a.1.min(b.1).min(c.1).floor() as i32).max(0);
		let y1 = (a.1.max(b.1).max(c.1).ceil() as i32).min(height - 1);
		// the edge function of p against the edge p0 -> p1 (positive inside, the corners ordered so the
		// area is positive), and which edges own the pixel centres lying on them: of an edge shared by
		// two triangles (run in opposite directions) exactly one counts
		let edge = |p0: (f32, f32), p1: (f32, f32), p: (f32, f32)| (p1.0 - p0.0) * (p.1 - p0.1) - (p1.1 - p0.1) * (p.0 - p0.0);
		let top_left = |p0: (f32, f32), p1: (f32, f32)| (p0.1 == p1.1 && p1.0 < p0.0) || p1.1 < p0.1;
		let edges = [(b, c, top_left(b, c)), (c, a, top_left(c, a)), (a, b, top_left(a, b))];
		for y in y0..=y1 {
			for x in x0..=x1 {
				let p = (x as f32 + 0.5, y as f32 + 0.5);
				let weights = edges.map(|(p0, p1, top_left)| {
					let w = edge(p0, p1, p);
					if w > 0.0 || (w == 0.0 && top_left) { Some(w) } else { None }
				});
				let [Some(wa), Some(wb), Some(wc)] = weights else {
					continue;
				};
				let (wa, wb, wc) = (wa / area, wb / area, wc / area);
				let source = match fill {
					Fill::Solid(color) => [color[0], color[1], color[2], 1.0],
					Fill::Image(image, tint) => {
						let u = uv_a[0] * wa + uv_b[0] * wb + uv_c[0] * wc;
						let v = uv_a[1] * wa + uv_b[1] * wb + uv_c[1] * wc;
						let texel = image.sample(u, v);
						[texel[0] * tint[0], texel[1] * tint[1], texel[2] * tint[2], texel[3]]
					}
				};
				let pixel = &mut canvas.pixels[(y * width + x) as usize];
				let alpha = source[3];
				for channel in 0..3 {
					pixel[channel] = source[channel] * alpha + pixel[channel] * (1.0 - alpha);
				}
				pixel[3] = alpha + pixel[3] * (1.0 - alpha);
			}
		}
	}
}

/// Where a layout's T-shirt graphic goes: R6's compositing mesh, or (R15's torso, which has none) a
/// rectangle of the texture (x0, y0, x1, y1 in mesh units, y up), the graphic upright across it.
enum GraphicSheet {
	None,
	Mesh(&'static str),
	Quad([f32; 4]),
}

/// The compositing meshes of a layout: per body region its base (body colour) sheet, the sheets that
/// place the pants and shirt templates, and the T-shirt graphic's place.
struct Sheets {
	size: (f32, f32),
	bases: Vec<&'static str>,
	pants: &'static str,
	shirt: &'static str,
	graphic: GraphicSheet,
}

fn sheets(layout: Layout) -> Sheets {
	match layout {
		Layout::R6 => Sheets {
			size: (1024.0, 512.0),
			bases: vec![
				"rbxasset://avatar/compositing/CompositTorsoBase.mesh",
				"rbxasset://avatar/compositing/CompositLeftArmBase.mesh",
				"rbxasset://avatar/compositing/CompositRightArmBase.mesh",
				"rbxasset://avatar/compositing/CompositLeftLegBase.mesh",
				"rbxasset://avatar/compositing/CompositRightLegBase.mesh",
			],
			pants: "rbxasset://avatar/compositing/CompositPantsTemplate.mesh",
			shirt: "rbxasset://avatar/compositing/CompositShirtTemplate.mesh",
			graphic: GraphicSheet::Mesh("rbxasset://avatar/compositing/CompositTShirt.mesh"),
		},
		Layout::R15Torso => Sheets {
			size: (388.0, 272.0),
			bases: vec!["rbxasset://avatar/compositing/R15CompositTorsoBase.mesh"],
			pants: "rbxasset://avatar/compositing/R15CompositTorsoBase.mesh",
			shirt: "rbxasset://avatar/compositing/R15CompositTorsoBase.mesh",
			// the template's torso front (231..359 x 74..202), where R15CompositTorsoBase puts it
			// (characters "r15-graphic-grid")
			graphic: GraphicSheet::Quad([2.0, 70.0, 130.0, 198.0]),
		},
		Layout::R15LeftArm | Layout::R15LeftLeg => Sheets {
			size: (264.0, 284.0),
			bases: vec!["rbxasset://avatar/compositing/R15CompositLeftArmBase.mesh"],
			pants: "rbxasset://avatar/compositing/R15CompositLeftArmBase.mesh",
			shirt: "rbxasset://avatar/compositing/R15CompositLeftArmBase.mesh",
			graphic: GraphicSheet::None,
		},
		Layout::R15RightArm | Layout::R15RightLeg => Sheets {
			size: (264.0, 284.0),
			bases: vec!["rbxasset://avatar/compositing/R15CompositRightArmBase.mesh"],
			pants: "rbxasset://avatar/compositing/R15CompositRightArmBase.mesh",
			shirt: "rbxasset://avatar/compositing/R15CompositRightArmBase.mesh",
			graphic: GraphicSheet::None,
		},
	}
}

/// Pixels of the composited texture per unit of the compositing meshes: one, as Studio's (characters
/// "r6-grid-close": 80 % within 2 at 1, 61 % at 0.5, 72 % at 2; `ROR_TUNE_COMPOSITE_SCALE` while
/// measuring).
fn composite_scale() -> f32 {
	static SCALE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
	*SCALE.get_or_init(|| std::env::var("ROR_TUNE_COMPOSITE_SCALE").ok().and_then(|value| value.parse().ok()).unwrap_or(1.0))
}

fn load_sheet(assets: &ror_assets::Assets, uri: &str) -> Result<Sheet> {
	let fetched = assets.fetch(uri).map_err(|error| anyhow!("{error}"))?;
	let mesh = ror_mesh::decode_mesh(&fetched.bytes).map_err(|error| anyhow!("{uri}: {error}"))?;
	Ok(Sheet::from_mesh(&mesh))
}

fn load_source(assets: &ror_assets::Assets, uri: &str) -> Result<Source> {
	let fetched = assets.fetch(uri).map_err(|error| anyhow!("{error}"))?;
	let image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow!("{uri}: {error}"))?;
	let image = ror_image::to_rgba8(&image);
	let pixels = image.levels.into_iter().next().context("image without pixels")?;
	Ok(Source { width: image.width, height: image.height, pixels })
}

/// The composited texture's pixels (before mips).
pub fn canvas(assets: &ror_assets::Assets, composite: &Composite) -> Result<Canvas> {
	let sheets = sheets(composite.layout);
	let scale = composite_scale();
	let (units_wide, units_high) = sheets.size;
	let mut canvas = Canvas::new((units_wide * scale).round() as u32, (units_high * scale).round() as u32);
	for (uri, color) in sheets.bases.iter().zip(&composite.colors) {
		draw(&mut canvas, &load_sheet(assets, uri)?, units_high, scale, &Fill::Solid(*color));
	}
	// in the compositing meshes' order (their z): pants, shirt, the T-shirt
	let graphic = match (&composite.graphic, &sheets.graphic) {
		(Some(layer), GraphicSheet::Mesh(uri)) => Some((layer, load_sheet(assets, uri)?)),
		(Some(layer), GraphicSheet::Quad(rectangle)) => Some((layer, Sheet::quad(*rectangle))),
		_ => None,
	};
	let mut layers: Vec<(&Layer, Sheet)> = Vec::new();
	if let Some(layer) = &composite.pants {
		layers.push((layer, load_sheet(assets, sheets.pants)?));
	}
	if let Some(layer) = &composite.shirt {
		layers.push((layer, load_sheet(assets, sheets.shirt)?));
	}
	layers.extend(graphic);
	for (layer, sheet) in layers {
		let source = load_source(assets, &layer.image)?;
		draw(&mut canvas, &sheet, units_high, scale, &Fill::Image(&source, layer.tint));
	}
	Ok(canvas)
}

impl Renderer {
	/// Whether a composited texture is available (built now if needed); a failure (a template that
	/// does not load) is reported in `missing` for the part.
	pub(super) fn composite_loads(&self, key: &str, path: &str, missing: &mut Vec<String>) -> bool {
		match self.load_texture(key, TextureUse::Color) {
			Ok(_) => true,
			Err(error) => {
				missing.push(format!("{path}: clothing: {error:#}"));
				false
			}
		}
	}

	/// The composited texture for a [`key`], with mips.
	pub(super) fn composite_texture(&self, key: &str) -> Result<GpuTexture> {
		let composite = from_key(key).context("malformed composite key")?;
		let canvas = canvas(&self.assets, &composite)?;
		let mut image = ror_image::Image::from_rgba8(canvas.width, canvas.height, canvas.to_rgba8());
		ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true);
		GpuTexture::upload(&self.gpu, "composite", &image, TextureUse::Color)
	}
}

#[cfg(test)]
mod tests {
	use super::{Canvas, Fill, Sheet, Source, draw};

	/// A quad of two triangles over a 6 x 4 canvas, y up, uv over the whole source.
	fn quad() -> Sheet {
		Sheet { positions: vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]], uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]], triangles: vec![[0, 1, 2], [0, 2, 3]] }
	}

	#[test]
	fn covers_each_pixel_once() {
		// a half-transparent layer: a pixel drawn twice (the shared diagonal) would reach alpha 0.75
		let source = Source { width: 1, height: 1, pixels: vec![255, 0, 0, 128] };
		let mut canvas = Canvas::new(6, 4);
		draw(&mut canvas, &quad(), 4.0, 1.0, &Fill::Image(&source, [1.0; 3]));
		for pixel in &canvas.pixels {
			assert!((pixel[3] - 128.0 / 255.0).abs() < 1e-5, "{pixel:?}");
		}
	}

	#[test]
	fn image_orientation() {
		// the source's top row lands at the canvas's top (positions y up, uv v down)
		let source = Source { width: 1, height: 2, pixels: vec![255, 0, 0, 255, 0, 0, 255, 255] };
		let mut canvas = Canvas::new(6, 4);
		draw(&mut canvas, &quad(), 4.0, 1.0, &Fill::Image(&source, [1.0; 3]));
		assert!(canvas.pixels[0][0] > 0.99 && canvas.pixels[0][2] < 0.01);
		assert!(canvas.pixels[23][2] > 0.99 && canvas.pixels[23][0] < 0.01);
		// a solid fill at twice the scale
		let mut canvas = Canvas::new(12, 8);
		draw(&mut canvas, &quad(), 4.0, 2.0, &Fill::Solid([0.0, 1.0, 0.0]));
		assert!(canvas.pixels.iter().all(|pixel| pixel[1] == 1.0 && pixel[3] == 1.0));
	}
}
