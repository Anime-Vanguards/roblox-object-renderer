//! Studio's built-in bloom and BloomEffect: everything the bloom chain (shaders/bloom.wgsl, run by
//! `Renderer::render_views`) reads, decided in one place, [`bloom_parameters`] (notes/render.md
//! "Bloom"; tools/analysis/bloom_fit.py, bloom_refit.py).
//!
//! Measured on Neon lines and dots at every phase of the pixel grid, squares, ladders over the threshold
//! and coloured sources at a 1116 px tall viewport: the exposed scene (each sample already clamped at
//! EXPOSED_CAP, shaders/common.wgsl) clamped at the cap; a chain of half-resolution levels whose
//! texels weigh the 4 x 4 finer texels around their centre and each by its brightness
//! (BLOOM_WEIGHTING), the first keeping its brightest channel's excess over the threshold, then back up
//! the chain, every level stored as R11G11B10 floats keep it; the first level is added to x before the
//! tone curve. Threshold and cap in tone-curve input units. The chain runs on a working grid of a fixed
//! height, so the halo keeps its size relative to the image.

use crate::quality::Quality;
use crate::scene::Environment;
use crate::scene::environment::PostEffect;

/// Wide sources start glowing between x 3.87 (none) and 3.90 (bloom-threshold: the halo outside each
/// ladder tile; inside neon-slabs-bloom's red 3.99 panel; in a Roblox client, 3-stud squares at x 3.869
/// show no halo and at 3.900 one of 3.7 of 255 4-10 px out), and with the brightness weighing thin
/// ones fit the same threshold: a linear first level made it depend on the source's width (4.03 for
/// lines up to 3.5 px, 3.89 for 10-16 px, 4.16 for pairs of 2 px lines). Refitted through the tone
/// curve's refitted toe on ror's own images (tools/analysis/bloom_refit.py): at 3.904 the ladders' tiles
/// just over it glowed 10-30 % under Studio. The excess starts at the threshold, without a knee: a
/// soft knee from 0.06 under it (0.06 before) left a halo at x 3.87 (0.0025 4-10 px out, a decal at
/// x 3.84 glowing 1-2 levels) where Studio and the client show none, and moved the halo metric of the
/// whole bloom set by under 0.1 point.
pub(super) const BLOOM_THRESHOLD: f32 = 3.885;
/// The working grid's cap, exactly a half float: the grid is an Rgba16Float texture, and a capped
/// source's excess is then stored as R11G11B10 keeps it (6 mantissa bits, 3.84375 here). With 7.78
/// that excess (3.873 where the GPU rounds 7.78 down to 7.777, 3.877 where it rounds to 7.781) fell
/// on either side of a storage step at 3.875: every capped halo 0.8 % apart by GPU.
const BLOOM_CAP: f32 = 7.75;
/// Each downsample weighs its texels by 1 / (1 + this x their brightest channel) and normalises (a
/// weak Karis average), the first level's and the others': a texel partly covered by a source reads
/// under its linear mean, a uniform area keeps its value (bloom-phase, bloom-lines, bloom-pairs: a
/// 3 px line at EV -2 whose best texel is 4.44 glows less than a 4 px one whose texel is 4.55 and
/// fully covered, as a threshold of 4.10 against 3.92 would).
const BLOOM_WEIGHTING: [f32; 2] = [0.016, 0.010];
/// The working grid's height in pixels: the capture viewport's, the only one Studio was measured at.
const BLOOM_REFERENCE_HEIGHT: f32 = 1116.0;
/// Each level's full weight, first to last: on the way up a level adds the coarser one (upsampled
/// bilinearly, then blurred by a tent of BLOOM_TENTS texels) to its own texels times its weight. The
/// built-in bloom keeps log2(24) - 0.9 = 3.68 levels, the fourth at 0.685 of its weight (bloom_levels):
/// inside large sources they add up to 3.76 x the excess (neon-slabs-bloom: the weaker channels
/// inside saturated panels, 3.4-3.85; at 3.88 they read 3 of 255 over Studio's); the first fades
/// within ~3 px of a source's edge, the fourth ends the halo ~52 px out (bloom-wide, bloom-kernel).
/// Refitted with the deeper levels' taps (BLOOM_TAPS) on the halo metric of the whole bloom set
/// (bloom_refit.py; 0.940, 1.140, 0.943, 0.978 and 1.060 before).
const BLOOM_LEVEL_WEIGHTS: [f32; 5] = [0.986, 1.010, 1.037, 1.056, 1.073];
/// Each level's downsample weighs the 4 x 4 finer texels around its texel's centre by (a, 0.5 - a,
/// 0.5 - a, a) per axis; a for the first level, the second, ... The first two's outer-heavy taps (as
/// 4 bilinear taps 1.12 px out would give) set where thin sources glow (bloom-phase: a 4 px line
/// centred on a half-resolution texel glows 3x as much as one centred between two; bloom-dots: 4 x 4
/// squares on even pixel phases do not glow at all, on odd phases in both axes 1.4x as much as in
/// one). The deeper ones are (1, 3, 3, 1) / 8 (4 bilinear taps 0.75 texels out): they set the halo's
/// tail, which past 40 px ends sooner in Studio than with the first's taps and shifts with where a
/// source sits on the coarse grids (bloom-shape under Retro, whose curve shows the faint end: per-level
/// taps fit its squares' profiles to 0.64 display levels rms, the tents refitted 1.12, Gaussian blurs
/// 1.19, a fifth level 1.14; levels 3-4 fit 0.116 there, 0.116-0.146 over the whole bloom set).
const BLOOM_TAPS: [f32; 5] = [0.3103, 0.3103, 0.125, 0.125, 0.125];
/// The tents' radii in texels of the level they blur, up to the first level, the second, ... (refitted
/// with the deeper taps from 4.10, 3.93 and 3.45: the compact deeper levels take wider tents up)
const BLOOM_TENTS: [f32; 4] = [3.44, 4.00, 4.11, 4.11];

/// A BloomEffect's threshold in tone-curve input units is 1.951 Threshold plus this: at Threshold 0 a
/// uniform area of x glows as 3.42 (x - 0.001) in a Roblox client (0.747; fixtures/tone-bloom.luau's
/// gray tiles under a BloomEffect of Intensity 1, Size 24, Threshold 0 at EV 0 and -2: 1.65 x at x
/// 0.002, 2.9 x at 0.008, 3.35 x at 0.03, 3.4 x from 0.07, where ror's threshold of 0 gave 3.55 x).
/// Below x 0.001 the client still glows 0.6-1.1 x, which this leaves out (bloom-shape retro-threshold0:
/// fog-lit black at EV -2 reads 1 of 255 in Studio and the client, 2 in ror before, 0 now).
const BLOOM_EFFECT_THRESHOLD_OFFSET: f32 = 0.001;

/// The built-in glow's three strengths by graphics quality level (crate::quality::Quality::glow: levels 1-5, 6-11,
/// 12 and up), each [threshold, amplitude, the downsamples' brightness weighting, how much of it reads the
/// luminance instead of the brightest channel, how much of the threshold's peak does]; 12 and up is the fit above.
/// Measured in a Roblox client (0.747, fixtures/quality-levels.luau `bloom`: white, red and cyan Neon bars and a
/// 0.4-stud speck in the dark, 1600 x 900, two sessions a level alike), each halo in linear light as a share of
/// level 12's 0.8 / 1.5 studs out: at 6-11 white 0.67 / 0.67, red 0.94 / 0.84, cyan 0.70 / 0.60, the speck 0.58 (the
/// red bar's least dimmed: the luminance weighs it down least); at 1-5 white 0.64 / 0.58, red 0.37 / 0.28, cyan 0.56
/// / 0.46, the speck 0.24 (a red source's peak read partly as luminance glows least; a small one is weighed down
/// most). Fitted on ror's chain against those shares times ror's own level-21 halo (rms of the log ratio over the
/// seven: 6-11 0.38 drawn as 12 → 0.11, 1-5 0.92 → 0.21, its white bar 0.79 / 0.68 and the speck 1.33 of the
/// client's share). The threshold stays 12's: a lower one fitted these Neon bars a little better (0.29 at 0.8) but
/// made a daylight scene's sky and sunlit surfaces glow, 20 of 255 over the client at level 4.
const GLOW_TIERS: [[f32; 5]; 3] = [
	[BLOOM_THRESHOLD, 0.8, 0.6, 0.0, 0.4],
	[BLOOM_THRESHOLD, 0.8, 0.2, 1.0, 0.0],
	[BLOOM_THRESHOLD, 1.0, BLOOM_WEIGHTING[0], 0.0, 0.0],
];

/// `ROR_TUNE_GLOW="threshold,amplitude,weighting,luminance weighting,luminance peak"` replaces the glow tier
/// below 12 a view draws, for fits.
fn tuned_glow() -> Option<[f32; 5]> {
	let values: Vec<f32> = std::env::var("ROR_TUNE_GLOW").ok()?.split(',').filter_map(|part| part.trim().parse().ok()).collect();
	values.try_into().ok()
}

/// The bloom a view draws: every value of the chain that a scene's effects or a graphics quality level
/// change. [`bloom_parameters`] is the one place that sets them.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Bloom {
	/// The brightest channel's excess over this (tone-curve input units) glows
	pub threshold: f32,
	/// Each level's weight, first to last (0 past the last level used, all 0 for no bloom)
	pub levels: [f32; 5],
	/// The working grid's cap on the exposed radiance
	pub cap: f32,
	/// How much the first level's downsample, and the deeper ones', weigh a texel down by its brightness
	pub weighting: [f32; 2],
	/// Each level's outer tap weight (BLOOM_TAPS)
	pub taps: [f32; 5],
	/// Each level's tent radius on the way up, in texels of the level it blurs (BLOOM_TENTS)
	pub tents: [f32; 4],
	/// The working grid's height in pixels
	pub grid_height: f32,
	/// How much the downsamples' weighting, and the first level's peak against the threshold, read the
	/// luminance instead of the brightest channel (0 for the brightest channel)
	pub luminance: [f32; 2],
}

impl Bloom {
	/// The levels the chain runs: up to the last with a weight
	pub fn level_count(&self) -> usize {
		self.levels.iter().rposition(|weight| *weight > 0.0).map_or(0, |last| last + 1)
	}
}

/// The levels a bloom of BloomEffect.Size `size` keeps and their weights: log2(size) - 0.9 of them,
/// the last one partly, Size held to 6-56. Chain fits per run (bloom-effect, bloom-effect-size) keep
/// 2.1 levels at Size 8 (2.1 predicted), 2.67 at 12 (2.68), 3.05 at 16 (3.10), 3.69 at 24 (3.68),
/// 4.1 at 32 (4.10), 4.46 at 40 (4.42) and 4.9 at 56 (4.91); Size 2 and 4 draw alike (~1.7) and 80
/// as 56.
fn bloom_levels(size: f32, amplitude: f32) -> [f32; 5] {
	if size <= 0.0 {
		return [0.0; 5];
	}
	let count = size.clamp(6.0, 56.0).log2() - 0.9;
	std::array::from_fn(|index| BLOOM_LEVEL_WEIGHTS[index] * (count - index as f32).clamp(0.0, 1.0) * amplitude)
}

/// The bloom a scene draws. A BloomEffect replaces the built-in bloom (bloom-effect: Intensity 0
/// removes every halo). Fitted per run with the built-in's chain: its threshold is 1.951 Threshold
/// (plus BLOOM_EFFECT_THRESHOLD_OFFSET)
/// (0.5, 0.95 and 1.5 fit 1.04, 1.87-1.88 and 2.92; at 3 only saturated sources glow and the
/// threshold trades with the weights, 5.9 with the built-in's; 1.966 before the toe's refit), its
/// levels weigh the built-in's times Intensity (levels 2-4 per unit of Intensity 1.15, 0.94, 0.64 at
/// Intensity 0.4, 1.20, 0.90, 0.65 at 1 and 1.24, 0.89, 0.66 at 2; the built-in's 1.16, 0.92, 0.64
/// then) and Size picks how many levels it keeps (bloom_levels). Several BloomEffects (in the Lighting
/// or the Camera alike) act as one of their Intensities added, their largest Size and their lowest
/// Threshold, whatever their order (bloom-combine in the capture Studio, 2026-10-17: 1 + 8 at Sizes 24
/// and 60 matched 9 at 60 exactly, 4 + 4 matched 8, Thresholds 1 and 2 matched 1, 3 and 2 matched 2,
/// Sizes 30 and 40 matched 40; a Camera's BloomEffect and ColorCorrectionEffect drew exactly as the
/// Lighting's).
///
/// At a graphics quality level (crate::quality::Quality) below 12 BloomEffects add nothing (a white
/// Neon bar's halo in the dark 0.8 studs out reads 78 of 255 at level 11 with a BloomEffect of Threshold
/// 0.8 and without, 145 with it at 12) and the built-in glow takes the level's strength (GLOW_TIERS).
pub(super) fn bloom_parameters(environment: &Environment, quality: &Quality) -> Bloom {
	let built_in = Bloom { threshold: BLOOM_THRESHOLD, levels: bloom_levels(24.0, 1.0), cap: BLOOM_CAP, weighting: BLOOM_WEIGHTING, taps: BLOOM_TAPS, tents: BLOOM_TENTS, grid_height: BLOOM_REFERENCE_HEIGHT, luminance: [0.0; 2] };
	if !quality.bloom_effect || quality.glow < 2 {
		let [threshold, amplitude, weighting, luminance_weighting, luminance_peak] = match quality.glow {
			tier @ 0..=1 => tuned_glow().unwrap_or(GLOW_TIERS[tier as usize]),
			_ => GLOW_TIERS[2],
		};
		return Bloom { threshold, levels: bloom_levels(24.0, amplitude), weighting: [weighting, BLOOM_WEIGHTING[1] * weighting / BLOOM_WEIGHTING[0]], luminance: [luminance_weighting, luminance_peak], ..built_in };
	}
	let mut combined: Option<(f32, f32, f32)> = None;
	for effect in &environment.effects {
		if let PostEffect::Bloom { intensity, size, threshold } = effect {
			let (intensity, size, threshold) = (intensity.max(0.0), *size, threshold.max(0.0));
			combined = Some(match combined {
				Some((sum, largest, lowest)) => (sum + intensity, largest.max(size), lowest.min(threshold)),
				None => (intensity, size, threshold),
			});
		}
	}
	match combined {
		Some((intensity, size, threshold)) => Bloom { threshold: 1.951 * threshold + BLOOM_EFFECT_THRESHOLD_OFFSET, levels: bloom_levels(size, intensity), ..built_in },
		None => built_in,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn bloom_of(effects: Vec<PostEffect>) -> Bloom {
		bloom_parameters(&Environment { effects, ..Default::default() }, &Quality::top())
	}

	/// A cutscene lights its impact with a Size-60 Intensity-8 BloomEffect in the Camera over
	/// the cutscene's own Intensity-1 bloom in the Lighting: Studio draws the two as one of Intensity
	/// 9 at Size 60, in either order (bloom-combine).
	#[test]
	fn several_blooms_act_as_one() {
		let bloom = |intensity, size, threshold| PostEffect::Bloom { intensity, size, threshold };
		let one = bloom_of(vec![bloom(9.0, 60.0, 1.0)]);
		for pair in [vec![bloom(1.0, 24.0, 2.0), bloom(8.0, 60.0, 1.0)], vec![bloom(8.0, 60.0, 1.0), bloom(1.0, 24.0, 2.0)]] {
			assert_eq!(bloom_of(pair), one);
		}
		assert_eq!(bloom_of(vec![bloom(0.0, 24.0, 2.0)]).levels, [0.0; 5], "Intensity 0 removes every halo");
		assert_eq!(bloom_of(vec![bloom(0.0, 24.0, 2.0)]).level_count(), 0);
		assert_eq!(bloom_of(Vec::new()).level_count(), 4, "the built-in bloom keeps 3.68 levels");
	}

	/// Below graphics quality level 12 a BloomEffect adds nothing and the built-in glow takes the level's tier:
	/// weaker, its weighting reading the luminance (6-11) and its peak too (1-5); from 12 as without a level.
	#[test]
	fn a_quality_level_picks_its_glow() {
		let effect = vec![PostEffect::Bloom { intensity: 1.0, size: 24.0, threshold: 0.8 }];
		let at = |level: u32, effects: Vec<PostEffect>| bloom_parameters(&Environment { effects, ..Default::default() }, &Quality::new(level, false, true));
		assert_eq!(at(12, effect.clone()), bloom_of(effect.clone()), "12 draws the BloomEffect");
		assert_eq!(at(21, Vec::new()), bloom_of(Vec::new()));
		for level in [1, 5, 6, 11] {
			assert_eq!(at(level, effect.clone()), at(level, Vec::new()), "level {level}: the BloomEffect adds nothing");
		}
		let (low, middle, top) = (at(5, Vec::new()), at(6, Vec::new()), at(12, Vec::new()));
		assert_eq!(at(1, Vec::new()), low);
		assert_eq!(at(11, Vec::new()), middle);
		assert!(middle.levels[0] < top.levels[0] && low.levels[0] < top.levels[0], "weaker below 12");
		assert!(low.weighting[0] > middle.weighting[0] && middle.weighting[0] > top.weighting[0], "small sources weighed down most below 6");
		assert_eq!((low.threshold, middle.threshold), (top.threshold, top.threshold), "no lower threshold: daylight does not glow");
		assert_eq!((top.luminance, middle.luminance[1]), ([0.0; 2], 0.0));
		assert!(middle.luminance[0] > 0.0 && low.luminance[1] > 0.0);
	}
}
