//! Roblox materials → shading parameters and textures.
//!
//! The values are first estimates unless notes/render.md or notes/materials.md record how they were
//! measured against Studio captures.

/// Shading models (mirrors `SHADING_*` in shaders/common.wgsl).
pub const SHADING_STANDARD: u32 = 0;
pub const SHADING_NEON: u32 = 1;
pub const SHADING_GLASS: u32 = 2;
pub const SHADING_FORCEFIELD: u32 = 3;

/// `Enum.Material` values.
pub const PLASTIC: u32 = 256;
pub const NEON: u32 = 288;

/// ForceField's radiance as a multiple of c² (c the part colour in 0..1): where its rim saturates
/// the alpha at 1 (Transparency -1 and -2), white reads x = 1.92-1.94 and a ship model's green
/// 1.91-1.93 · c² per channel at -2 and 0 EV (forcefield-basics; notes/render.md "ForceField").
pub const FORCEFIELD_RADIANCE: f32 = 1.93;

/// Studs per tile of the built-in materials' textures (material-swatches: every pattern repeats
/// every 8 studs).
pub const BUILT_IN_STUDS_PER_TILE: f32 = 8.0;

#[derive(Clone, Copy, Debug)]
pub struct MaterialParams {
	pub shading: u32,
	/// Used where the material's textures have no roughness map.
	pub roughness: f32,
	/// Used where the material's textures have no metalness map.
	pub metalness: f32,
	/// Emitted radiance as a multiple of the base colour.
	pub emissive: f32,
	/// Dielectric specular reflectance at normal incidence.
	pub f0: f32,
	/// How much of Schlick's Fresnel rise applies on top of [`DIRECT_REFLECTANCE`]'s (0 for every
	/// material; `ROR_TUNE_SPEC` sets it for fits).
	pub fresnel: f32,
	/// Specular visibility term: 0 correlated Smith, 1 Schlick-GGX (k = α / 2), 2 implicit, 3 Kelemen,
	/// 4 Schlick-GGX with k = k0 + k1 roughness ([`DIRECT_SPECULAR`]).
	pub visibility: u32,
}

/// The direct specular's lobe (the sun's and local lights'; shaders/mesh.wgsl `direct_light`),
/// [w, width, k0, k1]: GGX of width α = width (w + (1 - w) roughness²), seen through Schlick-GGX's
/// visibility with k = k0 + k1 roughness. Fitted with [`DIRECT_REFLECTANCE`] to the sun's term of
/// metal-sun's metals, sa-sun's and ibl-scale's spheres, sun-grazing's tiles (n·v down to 0.08) and
/// a character's close-up, every sphere or tile, view and band of the half angle one bin
/// (tools/analysis/direct_specular.py; notes/render.md "Direct specular"): log ratio rms 0.054 over
/// 1656 bins, against 0.116 for the law before (k = 1.17 (roughness + 1)² / 8, F0 alone, the metals'
/// own albedo compensated).
pub const DIRECT_SPECULAR: [f32; 4] = [0.038, 1.026, 0.377, 0.159];

/// The direct specular's reflectance, [g, c, fresnel, compensation]: the dielectric's F0 rises by
/// Schlick's (1 - v·h)⁵ of weight `fresnel` and is then times g (1 + c α²); the metal's albedo rises
/// alike; multiple scattering's energy is kept per share, the dielectric's times 1 + F0 (1 / E - 1)
/// with E the split-sum table's directional albedo at the prefiltered level's roughness, the metal's
/// times 1 + `compensation` (1 / E_m - 1) whatever its albedo (metal-sun: albedo 0.24, 0.48 and 0.97
/// alike), E_m read as [`DIRECT_METAL`] says.
pub const DIRECT_REFLECTANCE: [f32; 4] = [1.089, 0.945, 0.894, 0.5607];

/// The direct specular's metallic share, [a, b, width, separate]: its multiple scattering reads the
/// split-sum table at roughness a + b r (flatter than the dielectric's 0.075 + 0.9 r: metal-sun's full
/// metals had read 5-11 % over the law at roughness 0.45 and 5-6 % under it at 0.79), its lobe is
/// `width` times the dielectric's, and with `separate` 1 each share is compensated by its own
/// reflectance (0, the law before: the mixed reflectance times one compensation of the mixed R,
/// which left metal-sun's partly metallic spheres, metalness 0.66, 2-7 % under Studio at every angle).
/// Fitted with metal-sun's metals, the ship close-up's sun term at s = 1 per part
/// (`fresh-decompose`) and the rest of tools/analysis/direct_specular.py's bins (notes/render.md
/// "Direct specular").
pub const DIRECT_METAL: [f32; 4] = [0.2414, 0.8079, 1.0181, 1.0];

/// The sky specular and the metalness fade out with the view depth (shaders/mesh.wgsl
/// `specular_fade`), [level depth, metalness depth, level, metalness]: each linearly to none at its
/// depth in studs, from `level` (the sky specular's) and `metalness` (times the map's) at depth 0.
/// Fitted to ibl-fade's dielectric spheres and checked on its metals (within 1 % from 10 to 450 studs)
/// and on the close-ups, the survey and a large place; ibl-panels' walls seen head-on under a white sky
/// fade slower, as 1600 and 1000 (notes/render.md "Specular fade").
pub const SPECULAR_FADE: [f32; 4] = [1214.0, 1214.0, 1.0, 1.0];

/// The sky specular's colour against the prefiltered sky's: Studio's reflections read less blue than
/// red and green at the same shown level, 1-1.2 % on gray metals whatever their roughness (lod 1.7 and
/// 3) and the exposure (`sky-tint` at -2, 0 and +2 EV) and on the ship's panels, ~0.7 % on walls
/// under the default sky (`ibl-walls`); not the chain's storage, an extra storage of the sky, its
/// prefilter's colour, a Fresnel or F0 tint or ColorShift (notes/render.md "Specular fade"). Half the
/// metals' 1.2 %: the ship close-up 94.14 → 94.29 % within 2 (the full 1.2 %: 94.33, but
/// `ibl-panels` -6.7 and `ibl-walls` -1.7 against -2.0 and -0.4).
pub const SKY_SPECULAR_COLOUR: [f32; 3] = [1.0, 1.0, 0.994];

/// The direct specular's visibility term (MaterialParams::visibility).
const VISIBILITY: u32 = 4;

/// A SurfaceAppearance's roughness where it has no roughness map: a TexturePack's 4 x 4 constant
/// (notes/render.md "SurfaceAppearance").
pub const SA_DEFAULT_ROUGHNESS: f32 = 127.0 / 255.0;

const fn standard(roughness: f32, metalness: f32) -> MaterialParams {
	MaterialParams { shading: SHADING_STANDARD, roughness, metalness, emissive: 0.0, f0: 0.039, fresnel: 0.0, visibility: VISIBILITY }
}

/// Parameters of an `Enum.Material` value; unknown values shade as SmoothPlastic.
pub fn params(material: u32) -> MaterialParams {
	match material {
		// Plastic (roughness swept on plastic spheres, specular-spheres)
		256 => standard(0.6, 0.0),
		// SmoothPlastic (fitted on black spheres and a grazing backdrop, specular-spheres / neon-rows;
		// under the direct specular law sa-sun's black balls and MeshPart spheres want 0.41: log ratio
		// rms 0.083 over 32 bins against 0.118 at 0.43, the peak tone-top sees within 3 %)
		272 => standard(0.41, 0.0),
		// Neon
		288 => MaterialParams { shading: SHADING_NEON, roughness: 1.0, metalness: 0.0, emissive: 1.0, f0: 0.039, fresnel: 0.0, visibility: VISIBILITY },
		// Wood, WoodPlanks
		512 | 528 => standard(0.7, 0.0),
		// Marble
		784 => standard(0.35, 0.0),
		// CorrodedMetal
		1040 => standard(0.7, 0.8),
		// DiamondPlate
		1056 => standard(0.4, 1.0),
		// Foil
		// Foil (its pack has no roughness map): 0.45 catches the low sun's glints off its crumples
		// (metal-diffuse, the slab at EnvironmentSpecularScale 1: Studio 0.750, ror 0.567 at 0.3,
		// 0.767 at 0.45, 0.993 at 0.6; at 0.5: 0.672 / 0.550 / 0.645) and the sky in its ball's
		// reflection (ibl-spheres: per-pixel rms 8.4-10.8 % at 0.3, 7.5-8.8 % at 0.45)
		1072 => standard(0.45, 1.0),
		// Metal
		1088 => standard(0.35, 1.0),
		// Ice, Glacier
		1536 | 1552 => standard(0.2, 0.0),
		// Glass
		1568 => MaterialParams { shading: SHADING_GLASS, roughness: 0.05, metalness: 0.0, emissive: 0.0, f0: 0.039, fresnel: 0.0, visibility: VISIBILITY },
		// ForceField: unlit, emissive is its radiance per c² (the shader's forcefield())
		1584 => MaterialParams { shading: SHADING_FORCEFIELD, roughness: 1.0, metalness: 0.0, emissive: FORCEFIELD_RADIANCE, f0: 0.039, fresnel: 0.0, visibility: VISIBILITY },
		_ => standard(0.75, 0.0),
	}
}

/// The Organic pattern's noise frequency on MaterialVariants: noise uv per tile (organic-variant;
/// notes/materials.md "Organic pattern").
pub const VARIANT_ORGANIC_NOISE: f32 = 0.03;

/// A MaterialVariant's emission per unit of part colour (decoded), colour map, mask (as stored, not
/// squared), EmissiveTint (as stored) and EmissiveStrength: a white part with a uniform mask of 255 at
/// strength 1 emits 1.94 in darkness, 0.97 at strength 0.5, 0.506 and 0.299 of it with masks of 130
/// and 76; a gray (128) part 0.25 of it, a purple one its colour squared; a tint (1, 0.5, 0.25) as
/// stored; a colour map of 130 0.223 (variant-emission). The variant's AlphaMode changes nothing.
pub const VARIANT_EMISSION: f32 = 1.94;

/// A pre-2022 colour map's second, larger tiling, blended in with view depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarTiling {
	/// Far tiles per near tile (0.25: four times larger).
	pub scale: f32,
	/// View depth (studs) where the far tiling has fully replaced the near one; the blend is linear
	/// from the camera.
	pub depth: f32,
}

/// A built-in material's textures: its texture pack (an asset id) and how it tiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuiltInTextures {
	pub pack: &'static str,
	pub studs_per_tile: f32,
	/// The Organic pattern's noise frequency (noise uv per tile), when it tiles with it.
	pub organic: Option<f32>,
	/// The texture is a 4 x 4 atlas of quarter-tile cells; Studio's wang index picks the atlas cell
	/// of every quarter-tile cell of the surface (the pre-2022 packs with `<tiling>2</tiling>`).
	pub wang: bool,
	pub far: Option<FarTiling>,
	/// Studio shifts the texture by a random offset per part (material-offsets: a new one each time
	/// the part is created), so no capture can be matched exactly; ror derives one from the part.
	/// The Organic materials too (organic-anchor, organic-reset), and every pre-2022 pack but the
	/// newer materials'.
	pub random_offset: bool,
	/// Studio draws the pack without its colour map (DiamondPlate: its flat-lit swatch is uniform,
	/// std 0.0, and its sun-lit one is a white albedo's, while the pack's normal map matches).
	pub skip_color: bool,
	/// How strongly the pack's normal map tilts the surface (1: as stored).
	pub normal_strength: f32,
}

/// Plastic's normal map draws at under half its relief: 0.44 of its pack's (9475362634; 0.38 of the
/// steeper map of the 4096 table's pack it was fitted on, 92358737762614, whose slopes are 1.15 x).
/// With Studio's mip blend and renormalised mips, sun-lit Plastic within 2 of 255 at 0.35 / 0.38 /
/// 0.40 / 0.45 of that map: surfaces' Plastic blocks 98.7 / 99.0 / 99.1 / 99.2 %, meshpart-relief 93.5
/// / 93.1 / 92.5 / 90.4, the swatch 99.5 / 98.8 / 98.1 / 95.4, meshpart-materials 94.9 / 94.7 / 94.5 /
/// 93.7, material-zoom's slab (6-96 studs) 97.6 / 98.5 / 98.4 / 96.0; the swatch 99.9 % with its own
/// pack's transcode. Slate and Brick match at 1.
const PLASTIC_NORMAL_STRENGTH: f32 = 0.44;

/// Foil's crumples tilt about twice as far as its normal map says (its transcode is the PNG's): the
/// high-passed sun-lit slab matches Studio's at 2.4 x from 6 studs, 2.2 at 12-17, 2.0-2.1 at 24-34,
/// 1.85 at 48-59 and 1.55 at 96 (material-zoom, material-swatches; surfaces from 40 studs 1.95), and
/// the mean radiance at 1.9-2.15. DiamondPlate's matches at 1 (0.99-1.07 from 6 to 48 studs).
const FOIL_NORMAL_STRENGTH: f32 = 2.0;

/// 2022 Glass barely shows its normal map: the sun-lit swatch's high-pass is 5.0 x Studio's at 1,
/// 1.10 at 0.1, 1.01 at 0.05 and 0.93 without (its colour map's own contrast); within 2 of 255 22 /
/// 82 / 87 / 89 % (material-swatches; surfaces' Glass block from 40 studs alike, 23 / 82 / 86 / 89 %).
const GLASS_NORMAL_STRENGTH: f32 = 0.05;

/// Materials whose texture Studio shifts by a random offset per part (material-offsets: Marble,
/// CrackedLava, Pavement, Pebble, Cobblestone, Salt, Glass, Fabric and Metal changed phase between
/// two captures of the same slabs, high-passed NCC -0.13-0.06 against 0.998-1.000 for Brick,
/// WoodPlanks and the 2023 materials, which keep the corner-origin phase; material-phase: Foil and
/// DiamondPlate on four 16- and 24-stud slabs, Foil, DiamondPlate and Fabric at a new offset on every
/// slab and load, Leather and Plaster at one). Granite shows one layer at an offset other than the
/// corner's (material-swatches), Organic materials one too (organic-reset).
const RANDOM_OFFSET: [u32; 12] = [784, 804, 832, 836, 864, 880, 1056, 1072, 1088, 1312, 1392, 1568];

/// 2022 Granite blends its colour map into 4 x larger tiles with view depth like the pre-2022 far
/// tilings (the 32-stud repeat of its 200-stud slab), at a quarter of the near tiling's offset (+
/// whole tiles). material-zoom (one slab, one offset, straight down from 12 to 96 studs): far weight
/// 0.11, 0.15, 0.20, 0.27, 0.38, 0.80 at 12, 17, 24, 34, 48, 96 studs, depth / weight 110-127, D = 122
/// through the origin; the swatch 0.43-0.46 at 59 (material-swatches), 0.98 at 236 (organic-builtin).
const GRANITE_FAR: FarTiling = FarTiling { scale: 0.25, depth: 122.0 };

/// CrackedLava's emissive mask glows like a MaterialVariant's (VARIANT_EMISSION): the part colour
/// times the colour map times the mask as stored, here 3.55 x. material-glass, CrackedLava-*-dark (in
/// darkness; a white slab 0.0248 radiance over the frame, a red one only in red, a black one nothing):
/// Studio / ror at the 90th, 95th, 97.5th, 99th, 99.5th and 99.9th percentiles of the white slab's
/// glow 1.00, 1.06, 1.00, 1.01, 0.94, 1.08 (with the mask squared and no colour map, fitted on the
/// mean: 2.87, 1.94, 1.25, 1.05, 1.50, 2.23); its pixels above 0.01 0.179 of the frame against
/// Studio's 0.172 (0.108). Under flat light the same amount adds on top.
pub const EMISSIVE_STRENGTH: f32 = 3.55;

/// Packs only the account can read (assetdelivery answers 401 without the cookie) and the public
/// maps they list, for renders without it: 2022 Plastic's pack holds a normal map only.
const PUBLIC_MAPS: [(&str, [Option<&str>; 5]); 1] = [("9475422736", [None, Some("9475362634"), None, None, None])];

/// The maps (colour, normal, metalness, roughness, emissive asset ids) of a pack only the account
/// can read.
pub fn public_maps(pack: &str) -> Option<[Option<&'static str>; 5]> {
	PUBLIC_MAPS.iter().find(|(id, _)| *id == pack).map(|&(_, maps)| maps)
}

/// Leather and Plaster tile with the Organic pattern at noise frequency 0 and no random offsets, in
/// both generations: every part reads the noise at its origin, k = 0.531 (noise.dds bilinear at
/// (0, 0)), so it shows layer 4 at (sin 12, sin 28) = (0.463, 0.271) tiles cross-faded 0.25 with
/// layer 5. material-swatches, material-offsets: Studio's pattern sits at (0.465, 0.270) tiles on
/// 36- and 16-stud slabs, turned or not, with 0.78 of the colour map's contrast at 2-16 texels (two
/// uncorrelated layers at 0.75 / 0.25: 0.79).
const ORGANIC_FIXED: [u32; 2] = [2309, 2310];

/// The built-in materials' texture packs with `MaterialService.Use2022Materials`: the set the
/// terrain materials share, 1024² (512² Glass) `<usage>` 3 packs. 4096² packs of the same images
/// also exist, which Studio 0.741 does not draw: their transcoded layers give Brick's and WoodPlanks' deterministic flat swatches 47.3 %
/// and 39.7 % of pixels within 2 of 255 against 100.0 % and 100.0 % with these (material-swatches;
/// notes/materials.md "Where the textures come from"); after its captures Roblox's HTTP cache holds
/// these packs' layers, not the 4096² ones. The newer materials (Cardboard ... Rubber) use the
/// pre-2022 table's packs. The third value is the Organic pattern's noise frequency (noise uv per
/// tile), measured on a 200-stud slab of each material seen from 236 studs (organic-builtin: the
/// noise registers at one frequency and shift, level rms 0.06-0.14 (Limestone 0.46) against the next
/// shift's 1.1-1.2; the swatches agree to 0.0006), 0 for the Regular pattern. SmoothPlastic, Neon and
/// ForceField have none.
const PACKS_2022: [(u32, &str, f32); 40] = [
	(256, "9475422736", 0.0),          // Plastic (a normal map only; the account's: PUBLIC_MAPS)
	(512, "9920625499", 0.03),         // Wood
	(528, "9920626896", 0.0),          // WoodPlanks
	(784, "9873292869", 0.0),          // Marble
	(788, "9920482224", 0.05),         // Basalt
	(800, "9920600052", 0.03),         // Slate
	(804, "9920485426", 0.0),          // CrackedLava
	(816, "9920484334", 0.03),         // Concrete
	(820, "9920561624", 0.04),         // Limestone
	(832, "9920550720", 0.0),          // Granite (no Organic levels; far tiling: GRANITE_FAR)
	(836, "9920580094", 0.0),          // Pavement
	(848, "9920482992", 0.0),          // Brick
	(864, "9920581197", 0.0),          // Pebble
	(880, "9919719550", 0.0),          // Cobblestone
	(896, "10129366149", 0.045),       // Rock
	(912, "9920596353", 0.03),         // Sandstone
	(1040, "9920589512", 0.03),        // CorrodedMetal
	(1056, "10237721036", 0.0),        // DiamondPlate
	(1072, "9438410239", 0.0),         // Foil (a normal map only)
	(1088, "9920574966", 0.0),         // Metal
	(1280, "9920552044", 0.02),        // Grass
	(1284, "9920558145", 0.05),        // LeafyGrass
	(1296, "9920591862", 0.03),        // Sand
	(1312, "9920517963", 0.0),         // Fabric
	(1328, "9920620451", 0.03),        // Snow
	(1344, "9920578676", 0.03),        // Mud
	(1360, "9920554695", 0.05),        // Ground (frequency from its swatch)
	(1376, "9930003180", 0.03),        // Asphalt (frequency from its swatch)
	(1392, "9920590478", 0.0),         // Salt
	(1536, "9920556429", 0.03),        // Ice
	(1552, "9920518995", 0.03),        // Glacier
	(1568, "9873284556", 0.0),         // Glass
	(2304, "14108654559", 0.0),        // Cardboard
	(2305, "14108663921", 0.0),        // Carpet
	(2306, "17429458668", 0.0),        // CeramicTiles
	(2307, "18147685875", 0.0),        // ClayRoofTiles
	(2308, "91579661064923", 0.0),     // RoofShingles
	(2309, "14108671010", 0.0),        // Leather
	(2310, "14108672569", 0.0),        // Plaster
	(2311, "14108675388", 0.0),        // Rubber
];

/// A 2022 pack (and the newer materials' in both generations): 8 studs per tile, Organic where it
/// has a noise frequency.
const fn pack_2022(material: u32, pack: &'static str, noise: f32) -> BuiltInTextures {
	let mut random_offset = noise > 0.0;
	let mut index = 0;
	while index < RANDOM_OFFSET.len() {
		random_offset |= RANDOM_OFFSET[index] == material;
		index += 1;
	}
	let mut organic = noise > 0.0;
	let mut index = 0;
	while index < ORGANIC_FIXED.len() {
		organic |= ORGANIC_FIXED[index] == material;
		index += 1;
	}
	BuiltInTextures {
		pack,
		studs_per_tile: BUILT_IN_STUDS_PER_TILE,
		organic: if organic { Some(noise) } else { None },
		wang: false,
		far: None,
		random_offset,
		skip_color: material == 1056,
		normal_strength: 1.0,
	}
}

/// A pre-2022 pack, tiling regularly from a random offset per part.
const fn pre(pack: &'static str, studs_per_tile: f32) -> BuiltInTextures {
	BuiltInTextures { pack, studs_per_tile, organic: None, wang: false, far: None, random_offset: true, skip_color: false, normal_strength: 1.0 }
}

/// A pre-2022 pack tiled in Wang cells.
const fn pre_wang(pack: &'static str, studs_per_tile: f32) -> BuiltInTextures {
	BuiltInTextures { wang: true, ..pre(pack, studs_per_tile) }
}

/// A pre-2022 pack with the Organic pattern, at 8 studs per tile.
const fn pre_organic(pack: &'static str, noise: f32) -> BuiltInTextures {
	BuiltInTextures { organic: Some(noise), ..pre(pack, BUILT_IN_STUDS_PER_TILE) }
}

/// A pre-2022 pack whose colour map blends to `1 / scale` times larger tiles by `depth` studs.
const fn pre_far(pack: &'static str, studs_per_tile: f32, scale: f32, depth: f32) -> BuiltInTextures {
	BuiltInTextures { far: Some(FarTiling { scale, depth }), ..pre(pack, studs_per_tile) }
}

/// The built-in materials' textures without `MaterialService.Use2022Materials` (notes/materials.md
/// "Pre-2022 materials"). material-swatches matched every capture against the pack tables: the
/// classic materials show 2021 packs at 4-15 studs per tile, the terrain-type ones 2022-era packs at
/// 8, the eight newer ones their 2022 packs. Tile sizes from autocorrelation peaks and whole-frame
/// fits; Wang and far tilings from material-swatches, material-far and material-depths; the Organic
/// noise frequencies are the 2022 materials' (not measured on these packs). Plastic's faint detail
/// relief is not mapped; SmoothPlastic, Neon and ForceField have no textures.
const PACKS_PRE2022: [(u32, BuiltInTextures); 39] = [
	(512, pre("7547303225", 5.0)),                          // Wood
	(528, pre_wang("7547332968", 8.0)),                     // WoodPlanks
	(784, pre_wang("7547177270", 10.0)),                    // Marble
	(788, pre_organic("9873270487", 0.05)),                 // Basalt
	(800, pre_far("7547298114", 10.0, 0.25, 72.0)),         // Slate
	(804, pre("9438582231", 8.0)),                          // CrackedLava
	(816, pre_far("7546653951", 20.0 / 3.0, 0.25, 64.0)),   // Concrete
	(820, pre_organic("9873289812", 0.04)),                 // Limestone
	(832, pre_far("7547164710", 10.0, 0.25, 115.0)),        // Granite
	(836, pre("9873322398", 8.0)),                          // Pavement
	(848, pre_wang("7546650097", 8.0)),                     // Brick
	(864, pre("7547291361", 10.0)),                         // Pebble
	(880, pre_wang("7546652947", 400.0 / 27.0)),            // Cobblestone
	(896, pre_organic("9873515198", 0.045)),                // Rock
	(912, pre_organic("9873521380", 0.03)),                 // Sandstone
	(1040, pre_far("7547184629", 10.0, 0.5, 115.0)),        // CorrodedMetal
	(1056, pre("7547162198", 5.0)),                         // DiamondPlate
	(1072, pre("7546645012", 10.0)),                        // Foil (a normal map; white colour)
	(1088, pre("7547288171", 5.0)),                         // Metal
	(1280, pre_wang("7547169285", 400.0 / 39.0)),           // Grass
	(1284, pre_organic("9873288083", 0.05)),                // LeafyGrass
	(1296, pre("7547295153", 10.0)),                        // Sand
	(1312, pre("7547101130", 20.0 / 3.0)),                  // Fabric
	(1328, pre_organic("9439632387", 0.03)),                // Snow
	(1344, pre_organic("9873319819", 0.03)),                // Mud
	(1360, pre_organic("9439044431", 0.05)),                // Ground
	(1376, pre_organic("9873267379", 0.03)),                // Asphalt
	(1392, pre("9439566986", 8.0)),                         // Salt
	(1536, pre("7547171356", 10.0)),                        // Ice (a normal map; white colour)
	(1552, pre_organic("9438851661", 0.03)),                // Glacier
	(1568, pre("7547304948", 4.0)),                         // Glass
	(2304, pack_2022(2304, "14108654559", 0.0)),            // Cardboard
	(2305, pack_2022(2305, "14108663921", 0.0)),            // Carpet
	(2306, pack_2022(2306, "17429458668", 0.0)),            // CeramicTiles
	(2307, pack_2022(2307, "18147685875", 0.0)),            // ClayRoofTiles
	(2308, pack_2022(2308, "91579661064923", 0.0)),         // RoofShingles
	(2309, pack_2022(2309, "14108671010", 0.0)),            // Leather
	(2310, pack_2022(2310, "14108672569", 0.0)),            // Plaster
	(2311, pack_2022(2311, "14108675388", 0.0)),            // Rubber
];

/// The textures a built-in material shows in a place with or without the 2022 materials.
pub fn built_in_textures(material: u32, use_2022: bool) -> Option<BuiltInTextures> {
	if use_2022 {
		PACKS_2022.iter().find(|(value, ..)| *value == material).map(|&(_, pack, noise)| BuiltInTextures {
			normal_strength: match material {
				256 => tune_plastic_normal().unwrap_or(PLASTIC_NORMAL_STRENGTH),
				1072 => FOIL_NORMAL_STRENGTH,
				1568 => GLASS_NORMAL_STRENGTH,
				_ => 1.0,
			},
			far: if material == 832 { Some(GRANITE_FAR) } else { None },
			..pack_2022(material, pack, noise)
		})
	} else {
		PACKS_PRE2022.iter().find(|(value, _)| *value == material).map(|&(_, textures)| textures)
	}
}

fn tune_plastic_normal() -> Option<f32> {
	static TUNE: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| std::env::var("ROR_TUNE_PLASTIC_NORMAL").ok()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_textured_2022_material_has_one_pack() {
		let mut materials: Vec<u32> = PACKS_2022.iter().map(|(material, ..)| *material).collect();
		materials.sort();
		materials.dedup();
		assert_eq!(materials.len(), PACKS_2022.len());
		assert!(built_in_textures(272, true).is_none());
		assert_eq!(built_in_textures(848, true).map(|textures| textures.pack), Some("9920482992"));
		// 2022 Plastic's pack needs the account; its normal map does not
		assert_eq!(public_maps(built_in_textures(256, true).unwrap().pack).and_then(|maps| maps[1]), Some("9475362634"));
		assert_eq!(public_maps("9920482992"), None);
	}

	#[test]
	fn organic_materials_carry_their_noise_frequency() {
		assert_eq!(PACKS_2022.iter().filter(|(.., noise)| *noise > 0.0).count(), 17);
		let rock = built_in_textures(896, true).unwrap();
		assert_eq!(rock.organic, Some(0.045));
		assert!(rock.random_offset);
		assert_eq!(built_in_textures(848, true).unwrap().organic, None);
		let granite = built_in_textures(832, true).unwrap();
		assert_eq!(granite.organic, None);
		assert!(granite.random_offset);
		// Leather and Plaster: Organic at frequency 0, deterministic, in both generations
		for generation in [true, false] {
			let leather = built_in_textures(2309, generation).unwrap();
			assert_eq!((leather.organic, leather.random_offset), (Some(0.0), false));
		}
		assert_eq!(built_in_textures(2311, true).unwrap().organic, None);
	}

	#[test]
	fn every_textured_pre2022_material_has_one_pack() {
		let mut materials: Vec<u32> = PACKS_PRE2022.iter().map(|(material, _)| *material).collect();
		materials.sort();
		materials.dedup();
		assert_eq!(materials.len(), PACKS_PRE2022.len());
		for untextured in [256, 272, 288, 1584] {
			assert!(built_in_textures(untextured, false).is_none());
		}
		let brick = built_in_textures(848, false).unwrap();
		assert_eq!((brick.pack, brick.studs_per_tile, brick.wang, brick.random_offset), ("7546650097", 8.0, true, true));
		// the newer materials look the same in both generations
		assert_eq!(built_in_textures(2306, false), built_in_textures(2306, true));
		assert_eq!(built_in_textures(800, false).and_then(|slate| slate.far).map(|far| far.scale), Some(0.25));
		// pre-2022 DiamondPlate keeps its colour map
		assert_eq!(built_in_textures(1056, false).map(|textures| textures.skip_color), Some(false));
	}
}
