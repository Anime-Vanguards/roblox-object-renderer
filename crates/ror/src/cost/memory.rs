//! Upper-bound memory estimates in bytes for referenced authored mesh and texture assets in
//! `ror cost`: every distinct mesh asset and authored texture asset referenced by a scene is
//! deduplicated by canonical asset ID / content URI and counted once, with measured encoded file
//! bytes, assumed GPU mesh buffer bytes (FileMesh 40 B vertex + 8 B skinning envelope layout,
//! assumed u16/u32 indices and once-per-mesh bone matrices), derived uncompressed RGBA8 base and
//! full mip-chain bytes, derived desktop BC1/BC3 full mip-chain bytes, and derived mobile ETC2
//! full mip-chain bytes per `notes/image.md` (vkFormat 147/151; no ASTC format is recorded).
//!
//! These totals are upper-bound asset estimates, not measured Roblox client VRAM residency: every
//! mip level of every referenced texture is counted whether drawn or streamed, and built-in
//! material texture packs, plain primitive part geometry, Terrain, UI, default Sky textures, and
//! per-instance posed skinning buffers are excluded.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rayon::prelude::*;
use ror_assets::{Assets, ContentId, Download, Transcode};
use ror_image::PixelFormat;
use serde::Serialize;

use super::meshes::{self, MeshStats, Meshes};
use crate::scene::Scene;

/// Heaviest entries kept in `top` for meshes and textures.
const TOP_LIST: usize = 20;

/// Built-in default Sky textures (left out when a place has a default Sky unchanged from Roblox's).
const DEFAULT_SKY_PREFIX: &str = "rbxasset://textures/sky/sky512_";
const DEFAULT_SUN: &str = "rbxasset://sky/sun.jpg";
const DEFAULT_MOON: &str = "rbxasset://sky/moon.jpg";

/// Explicit basis for one memory figure family (`measured`, `derived`, `assumed`, or `unrecorded`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BasisNote {
	pub basis: &'static str,
	pub detail: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub settle_with: Option<&'static str>,
}

/// Basis recorded for every memory metric in [`MemoryReport`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBasis {
	pub mesh_file_bytes: BasisNote,
	pub mesh_gpu_bytes: BasisNote,
	pub texture_file_bytes: BasisNote,
	pub texture_rgba8_bytes: BasisNote,
	pub texture_desktop_bc_bytes: BasisNote,
	pub texture_mobile_etc2_bytes: BasisNote,
	pub texture_mobile_astc_bytes: BasisNote,
}

impl Default for MemoryBasis {
	fn default() -> Self {
		Self {
			mesh_file_bytes: BasisNote {
				basis: "measured",
				detail: "exact encoded byte length of each unique loaded mesh file or inline union MeshData",
				settle_with: None,
			},
			mesh_gpu_bytes: BasisNote {
				basis: "assumed",
				detail: "upper bound from recorded FileMesh binary layout (notes/mesh.md: 12 B pos + 12 B normal + 8 B UV + 4 B tangent + 4 B color = 40 B full vertex, 8 B/vertex skinning envelopes) with assumed u16/u32 indices and 64 B/bone matrices counted once per unique mesh (not per posed instance); client GPU mesh layout is unrecorded",
				settle_with: Some("Measure client GPU vertex/index/skinning buffer layouts via a graphics frame capture or live client memory profiler."),
			},
			texture_file_bytes: BasisNote {
				basis: "measured",
				detail: "exact encoded byte length of loaded KTX2 transcode pack files or uploaded/local image files",
				settle_with: None,
			},
			texture_rgba8_bytes: BasisNote {
				basis: "derived",
				detail: "upper bound: full uncompressed RGBA8 base and mip chain down to 1x1 from decoded texture dimensions (ignores progressive mip streaming)",
				settle_with: None,
			},
			texture_desktop_bc_bytes: BasisNote {
				basis: "derived",
				detail: "upper bound: full BC1/BC3 mip chain from decoded dimensions (uses decoded KTX2/DDS block format when loaded; derives BC1 for opaque non-normal uploads and BC3 for alpha or normal uploads; ignores progressive mip streaming)",
				settle_with: Some("Live client memory profiler capture to measure streamed mip residency."),
			},
			texture_mobile_etc2_bytes: BasisNote {
				basis: "derived",
				detail: "upper bound: full ETC2 mip chain using formats recorded in notes/image.md (vkFormat 147 ETC2 RGB8 8 B/4x4 block for opaque TexturePack layers; vkFormat 151 ETC2 RGBA8 16 B/4x4 block for mobile rollout images /////Ck= and alpha/normal pack layers) applied to desktop transcode or upload dimensions because ror never requests mobile ETC2 files",
				settle_with: Some("Fetch mobile ETC2 transcode packs (/////Ck= and TexturePack option bit 0x08) to measure mobile transcoded dimensions and encoded pack sizes directly."),
			},
			texture_mobile_astc_bytes: BasisNote {
				basis: "unrecorded",
				detail: "no ASTC transcode format is recorded in notes/image.md; no ASTC figure is emitted",
				settle_with: Some("Capture an iOS/Android Roblox client HTTP cache (rbx-storage) and request mobile assetdelivery fidelities to record actual mobile vkFormats, dimensions, and whether ASTC is served."),
			},
		}
	}
}

/// A memory category explicitly excluded from [`MemoryReport`] totals.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUnmodeled {
	pub kind: &'static str,
	pub reason: &'static str,
	pub settle_with: &'static str,
}

fn default_unmodeled() -> Vec<MemoryUnmodeled> {
	vec![
		MemoryUnmodeled {
			kind: "builtInMaterialPacks",
			reason: "Only MaterialVariant maps/packs and authored SurfaceAppearance/part/decal/effect/custom-sky textures are counted; built-in Roblox material texture packs are omitted.",
			settle_with: "Add built-in material pack resolution from MaterialService if full-place material residency is needed.",
		},
		MemoryUnmodeled {
			kind: "primitivePartGeometry",
			reason: "Plain primitive parts (Block, Ball, Cylinder, Wedge, CornerWedge, Truss, non-FileMesh SpecialMesh) are omitted from mesh bytes.",
			settle_with: "Live client geometry memory counter capture.",
		},
		MemoryUnmodeled {
			kind: "terrainAndUi",
			reason: "Roblox Terrain voxels, Terrain Grass, GUI textures/fonts, and default Sky textures are omitted.",
			settle_with: "Live client / Studio memory profiler capture.",
		},
		MemoryUnmodeled {
			kind: "streamingAndRuntimeResidency",
			reason: "Every mip level of every referenced texture is counted whether drawn or streamed; per-instance posed skinning buffers, render targets, shadow maps, and GPU driver padding are omitted.",
			settle_with: "Live client Developer Console / Stats memory capture on the same views.",
		},
		MemoryUnmodeled {
			kind: "mobileAstcFormat",
			reason: "No ASTC format is recorded in notes/image.md, so no mobile ASTC total is printed or emitted.",
			settle_with: "Capture an iOS/Android Roblox client HTTP cache to record actual mobile texture vkFormats and dimensions.",
		},
	]
}

/// Where a loaded texture's dimensions and file bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TextureSource {
	/// Roblox KTX2 texture transcode (`fetch_transcode`).
	Transcode,
	/// Roblox uploaded image asset (`fetch` on `ContentId::Asset`): dimensions may differ from KTX2 transcodes.
	Upload,
	/// Local filesystem image (`file://` or path).
	Local,
	/// Built-in Studio install texture (`rbxasset://`).
	Studio,
}

/// Byte subtotals for textures loaded from one [`TextureSource`].
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureSourceTotals {
	pub count: u64,
	pub file_bytes: u64,
	pub rgba8_mip_bytes: u64,
	pub desktop_bc_mip_bytes: u64,
	pub mobile_etc2_mip_bytes: u64,
}

/// Provenance breakdown showing how many unique textures and bytes came from each source.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureSources {
	pub transcode: TextureSourceTotals,
	pub upload: TextureSourceTotals,
	pub local: TextureSourceTotals,
	pub studio: TextureSourceTotals,
}

/// Assets and progressive packs whose bytes could not be read or decoded.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingMemory {
	pub meshes: u64,
	pub mesh_instances: u64,
	pub textures: u64,
	pub texture_references: u64,
	/// Progressive KTX2 pack files whose encoded `fileBytes` could not be read.
	pub transcode_packs: u64,
	/// Total distinct missing assets (`meshes + textures`).
	pub assets: u64,
	/// Explanation of which bytes could not be counted (`None` when complete).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub uncounted_bytes: Option<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub mesh_ids: Vec<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub texture_ids: Vec<String>,
}

/// Complete deduplicated upper-bound memory estimate for a scene (or union of timeline scenes).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReport {
	/// Always `"upper-bound-estimate"`: full asset chains, not measured client VRAM residency.
	pub kind: &'static str,
	/// True when any mesh, texture, or progressive KTX2 pack file failed to load (totals exclude them).
	pub partial: bool,
	/// Deduplication key: canonical asset ID / content URI (two distinct files with identical bytes count twice).
	pub deduplicated_by: &'static str,
	pub basis: MemoryBasis,
	pub unmodeled: Vec<MemoryUnmodeled>,
	pub missing: MissingMemory,
	pub meshes: MeshMemory,
	pub textures: TextureMemory,
	pub total_gpu_bytes: TotalGpuBytes,
}

impl Default for MemoryReport {
	fn default() -> Self {
		Self {
			kind: "upper-bound-estimate",
			partial: false,
			deduplicated_by: "canonical-asset-id",
			basis: MemoryBasis::default(),
			unmodeled: default_unmodeled(),
			missing: MissingMemory::default(),
			meshes: MeshMemory::default(),
			textures: TextureMemory::default(),
			total_gpu_bytes: TotalGpuBytes::default(),
		}
	}
}

/// Upper-bound estimated GPU bytes across unique loaded meshes and full texture mip chains.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalGpuBytes {
	/// True when any mesh, texture, or pack failed to load; sums cover loaded assets only.
	pub partial: bool,
	/// Distinct meshes + textures whose bytes could not be counted.
	pub uncounted_assets: u64,
	/// Assumed mesh GPU bytes + derived uncompressed RGBA8 full mip-chain bytes.
	pub rgba8: u64,
	/// Assumed mesh GPU bytes + derived desktop BC1/BC3 full mip-chain bytes.
	pub desktop_bc: u64,
	/// Assumed mesh GPU bytes + derived mobile ETC2 full mip-chain bytes (`notes/image.md` vkFormat 147/151 at desktop/upload dimensions).
	pub mobile_etc2: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshMemory {
	/// Distinct mesh assets referenced by the scene (counted once each).
	pub unique: u64,
	/// Distinct meshes that loaded and are included in the byte totals.
	pub loaded: u64,
	/// Total part instances referencing those meshes.
	pub instances: u64,
	/// Encoded file bytes across unique loaded meshes.
	pub file_bytes: u64,
	/// Vertex buffer GPU bytes across unique loaded meshes.
	pub vertex_bytes: u64,
	/// Index buffer GPU bytes across unique loaded meshes.
	pub index_bytes: u64,
	/// Skinning GPU bytes across unique loaded meshes.
	pub skinning_bytes: u64,
	/// Total packed GPU geometry bytes (`vertexBytes + indexBytes + skinningBytes`).
	pub gpu_bytes: u64,
	/// Distinct meshes that failed to load or decode (their bytes are uncounted).
	pub missing: u64,
	/// Part instances referencing missing meshes.
	pub missing_instances: u64,
	/// Heaviest unique meshes by GPU bytes (ties by id).
	pub top: Vec<MeshEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEntry {
	pub id: String,
	pub instances: u64,
	pub vertices: u32,
	pub indices: u32,
	pub lods: usize,
	pub skinned: bool,
	pub file_bytes: u64,
	pub vertex_bytes: u64,
	pub index_bytes: u64,
	pub skinning_bytes: u64,
	pub gpu_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureMemory {
	/// Distinct authored texture assets referenced by the scene (counted once each).
	pub unique: u64,
	/// Distinct textures that loaded and are included in the byte totals.
	pub loaded: u64,
	/// Total texture references across parts, decals, surface appearances, effects and custom skies.
	pub references: u64,
	/// Encoded bytes on disk / downloaded across unique loaded textures.
	pub file_bytes: u64,
	/// True when any texture or progressive KTX2 pack file failed to load (making `fileBytes` partial).
	pub file_bytes_partial: bool,
	/// Uncompressed RGBA8 level-0 (base mip) GPU bytes across unique loaded textures.
	pub rgba8_base_bytes: u64,
	/// Uncompressed RGBA8 full mip-chain GPU bytes across unique loaded textures.
	pub rgba8_mip_bytes: u64,
	/// Desktop block-compressed (BC1/BC3) full mip-chain GPU bytes (derived upper bound).
	pub desktop_bc_mip_bytes: u64,
	/// Mobile ETC2 full mip-chain GPU bytes derived from `notes/image.md` (vkFormat 147 ETC2 RGB8
	/// 8 B/4x4 block for opaque TexturePack layers, vkFormat 151 ETC2 RGBA8 16 B/4x4 block for
	/// mobile rollout images and alpha/normal pack layers) at desktop transcode or upload sizes.
	pub mobile_etc2_mip_bytes: u64,
	/// Distinct textures that failed to load or decode (their bytes are uncounted).
	pub missing: u64,
	/// Scene references pointing to missing textures.
	pub missing_references: u64,
	/// Missing progressive KTX2 pack files across partially cached transcodes (`fileBytes` uncounted for those packs).
	pub missing_packs: u64,
	/// Counts and byte subtotals broken down by where each loaded texture came from.
	pub sources: TextureSources,
	/// Heaviest unique textures by RGBA8 mip-chain GPU bytes (ties by id).
	pub top: Vec<TextureEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureEntry {
	pub id: String,
	pub references: u64,
	pub source: TextureSource,
	pub width: u32,
	pub height: u32,
	pub layers: u32,
	pub depth: u32,
	pub format: &'static str,
	pub desktop_bc_format: &'static str,
	pub mobile_etc2_format: &'static str,
	pub has_alpha: bool,
	pub packs_loaded: u32,
	pub packs_expected: u32,
	pub file_bytes: u64,
	pub rgba8_base_bytes: u64,
	pub rgba8_mip_bytes: u64,
	pub desktop_bc_mip_bytes: u64,
	pub mobile_etc2_mip_bytes: u64,
}

/// Decoded metadata for one unique texture asset, cached across frames in timeline mode.
#[derive(Clone, Debug)]
pub struct TextureInfo {
	pub source: TextureSource,
	pub width: u32,
	pub height: u32,
	pub layers: u32,
	pub depth: u32,
	pub format: PixelFormat,
	pub has_alpha: bool,
	pub file_bytes: u64,
	pub packs_loaded: u32,
	pub packs_expected: u32,
}

/// Cache of decoded texture metadata keyed by canonical texture key.
#[derive(Default)]
pub struct TextureCache {
	entries: HashMap<String, Option<TextureInfo>>,
}

#[derive(Clone, Debug)]
struct TextureWanted {
	/// Original URI or pack URI for fetching.
	uri: String,
	/// Transcode layer when inspecting a TexturePack layer directly.
	pack_layer: Option<Transcode>,
	/// Whether to prefer the Roblox KTX2 transcode before raw upload (true for regular image assets,
	/// false for MaterialVariant maps and clothing templates).
	prefer_transcode: bool,
	/// Whether any reference uses this texture as a normal map.
	is_normal: bool,
	/// Reference count across the scene.
	references: u64,
}

fn transcodes_enabled() -> bool {
	std::env::var("ROR_TRANSCODES").map_or(true, |value| value.trim() != "0")
}

fn is_asset_id(uri: &str) -> bool {
	matches!(ContentId::parse(uri), Ok(ContentId::Asset { .. }))
}

fn is_default_sky_texture(uri: &str) -> bool {
	let trimmed = uri.trim();
	trimmed.starts_with(DEFAULT_SKY_PREFIX) || trimmed == DEFAULT_SUN || trimmed == DEFAULT_MOON
}

/// Measures deduplicated mesh and texture memory for one scene.
pub fn measure(scene: &Scene, meshes: &Meshes, assets: &Assets) -> MemoryReport {
	let mut cache = TextureCache::default();
	measure_cached(scene, meshes, assets, &mut cache)
}

/// Measures deduplicated mesh and texture memory for one scene, reusing `cache`.
pub fn measure_cached(scene: &Scene, meshes: &Meshes, assets: &Assets, cache: &mut TextureCache) -> MemoryReport {
	measure_many(&[scene], &meshes.stats, assets, cache)
}

/// Incremental collector for deduplicated mesh and texture references across one or more scenes
/// (allows multi-frame timeline runs to accumulate memory without retaining every frame's `Scene`).
#[derive(Default)]
pub struct MemoryCollector {
	mesh_instances: BTreeMap<String, u64>,
	wanted: BTreeMap<String, TextureWanted>,
	pack_cache: HashMap<String, Option<PackMaps>>,
	warmed_packs: std::collections::HashSet<String>,
}

impl MemoryCollector {
	/// Records one scene's mesh and texture references into the collector.
	pub fn add_scene(&mut self, scene: &Scene, assets: &Assets) {
		let mut frame_counts: HashMap<String, u64> = HashMap::new();
		for part in &scene.parts {
			if let Some(key) = meshes::mesh_key(&part.geometry) {
				*frame_counts.entry(key).or_default() += 1;
			}
		}
		for (key, count) in frame_counts {
			let entry = self.mesh_instances.entry(key).or_default();
			*entry = (*entry).max(count);
		}

		if !assets.offline() {
			let offline_assets = assets.with_offline(true);
			let mut uncached_packs = Vec::new();
			for part in &scene.parts {
				if let Some(pack) = part.surface_appearance.as_ref().and_then(|a| a.texture_pack.as_deref())
					&& is_asset_id(pack)
					&& self.warmed_packs.insert(meshes::canonical(pack))
					&& offline_assets.fetch(pack).is_err()
				{
					uncached_packs.push(Download::Asset(pack.trim().to_string()));
				}
				if let Some(pack) = scene.materials.variant(part.material, &part.material_variant).and_then(|v| v.texture_pack.as_deref())
					&& is_asset_id(pack)
					&& self.warmed_packs.insert(meshes::canonical(pack))
					&& offline_assets.fetch(pack).is_err()
				{
					uncached_packs.push(Download::Asset(pack.trim().to_string()));
				}
			}
			if !uncached_packs.is_empty() {
				assets.warm(&uncached_packs, &ktx2_pack_count_fast);
			}
		}

		let use_transcodes = transcodes_enabled();
		let mut frame_refs: BTreeMap<String, TextureWanted> = BTreeMap::new();
		collect_scene_textures(scene, assets, use_transcodes, &mut self.pack_cache, &mut frame_refs);
		for (key, item) in frame_refs {
			self.wanted
				.entry(key)
				.and_modify(|existing| {
					existing.references = existing.references.max(item.references);
					existing.is_normal |= item.is_normal;
				})
				.or_insert(item);
		}
	}

	/// Inspects collected textures once and produces the honest upper-bound [`MemoryReport`].
	pub fn finish(self, mesh_stats: &HashMap<String, Arc<MeshStats>>, assets: &Assets, cache: &mut TextureCache) -> MemoryReport {
		let Self { mesh_instances, wanted, .. } = self;
		let offline_assets = assets.with_offline(true);
		let use_transcodes = transcodes_enabled();

		let mut mesh_mem = MeshMemory { unique: mesh_instances.len() as u64, ..Default::default() };
		let mut mesh_entries = Vec::with_capacity(mesh_instances.len());
		let mut missing_mesh_ids = Vec::new();
		for (id, instances) in mesh_instances {
			mesh_mem.instances += instances;
			if let Some(stats) = mesh_stats.get(&id) {
				mesh_mem.loaded += 1;
				mesh_mem.file_bytes += stats.file_bytes;
				mesh_mem.vertex_bytes += stats.vertex_bytes;
				mesh_mem.index_bytes += stats.index_bytes;
				mesh_mem.skinning_bytes += stats.skinning_bytes;
				mesh_mem.gpu_bytes += stats.gpu_bytes;
				mesh_entries.push(MeshEntry {
					id,
					instances,
					vertices: stats.vertices,
					indices: stats.indices,
					lods: stats.lods.len(),
					skinned: stats.skinned,
					file_bytes: stats.file_bytes,
					vertex_bytes: stats.vertex_bytes,
					index_bytes: stats.index_bytes,
					skinning_bytes: stats.skinning_bytes,
					gpu_bytes: stats.gpu_bytes,
				});
			} else {
				mesh_mem.missing += 1;
				mesh_mem.missing_instances += instances;
				if missing_mesh_ids.len() < TOP_LIST {
					missing_mesh_ids.push(id);
				}
			}
		}
		mesh_entries.sort_by(|a, b| b.gpu_bytes.cmp(&a.gpu_bytes).then_with(|| a.id.cmp(&b.id)));
		mesh_entries.truncate(TOP_LIST);
		mesh_mem.top = mesh_entries;

		// Phase 1: inspect every uncached texture from disk/memory once (no network, no zstd decode
		// on Roblox KTX2 files). Only genuinely missing assets or missing progressive packs hit the
		// network in Phase 2, so warm runs never call `assets.warm` or re-read cached pack 0 files.
		let uncached: Vec<(&String, &TextureWanted)> = wanted.iter().filter(|(key, _)| !cache.entries.contains_key(*key)).collect();
		if !uncached.is_empty() {
			let probed: Vec<(&String, &TextureWanted, ProbeResult)> = uncached
				.into_par_iter()
				.map(|(key, item)| (key, item, probe_texture_offline(item, &offline_assets, assets.offline(), use_transcodes)))
				.collect();

			let mut needs_network: Vec<(&String, &TextureWanted)> = Vec::new();
			let mut missing_packs: Vec<(&String, TextureInfo, String, Transcode, Vec<u32>)> = Vec::new();
			for (key, item, outcome) in probed {
				match outcome {
					ProbeResult::Done(info) => {
						cache.entries.insert(key.clone(), info);
					}
					ProbeResult::MissingPacks { info, uri, layer, packs } => {
						missing_packs.push((key, info, uri, layer, packs));
					}
					ProbeResult::NeedsNetwork => {
						needs_network.push((key, item));
					}
				}
			}

			// Phase 2a: warm and inspect only textures whose pack 0 / upload was not cached on disk.
			if !needs_network.is_empty() {
				let mut downloads = Vec::new();
				for (_, item) in &needs_network {
					if let Some(layer) = item.pack_layer {
						if is_asset_id(&item.uri) {
							downloads.push(Download::Transcode(item.uri.clone(), layer));
						}
					} else if is_asset_id(&item.uri) {
						downloads.push(if item.prefer_transcode && use_transcodes {
							Download::Transcode(item.uri.clone(), Transcode::Image)
						} else {
							Download::Asset(item.uri.clone())
						});
					}
				}
				if !downloads.is_empty() {
					assets.warm(&downloads, &ktx2_pack_count_fast);
				}
				let fetched: Vec<(String, Option<TextureInfo>)> = needs_network
					.into_par_iter()
					.map(|(key, item)| (key.clone(), inspect_texture(item, assets, use_transcodes)))
					.collect();
				for (key, info) in fetched {
					cache.entries.insert(key, info);
				}
			}

			// Phase 2b: for transcodes whose pack 0 was already read once in Phase 1, fetch ONLY the
			// missing packs 1..N without reading or parsing pack 0 again.
			if !missing_packs.is_empty() {
				let completed: Vec<(String, Option<TextureInfo>)> = missing_packs
					.into_par_iter()
					.map(|(key, mut info, uri, layer, packs)| {
						for pack in packs {
							if let Ok(fetched) = assets.fetch_transcode(&uri, layer, pack) {
								info.file_bytes += fetched.bytes.len() as u64;
								info.packs_loaded += 1;
							}
						}
						(key.clone(), Some(info))
					})
					.collect();
				for (key, info) in completed {
					cache.entries.insert(key, info);
				}
			}
		}

		let mut tex_mem = TextureMemory { unique: wanted.len() as u64, ..Default::default() };
		let mut tex_entries = Vec::with_capacity(wanted.len());
		let mut missing_tex_ids = Vec::new();
		for (id, item) in wanted {
			tex_mem.references += item.references;
			let Some(Some(info)) = cache.entries.get(&id) else {
				tex_mem.missing += 1;
				tex_mem.missing_references += item.references;
				if missing_tex_ids.len() < TOP_LIST {
					missing_tex_ids.push(id);
				}
				continue;
			};
			let entry = build_texture_entry(id, item.references, item.is_normal, item.pack_layer.is_some(), info);
			tex_mem.loaded += 1;
			tex_mem.file_bytes += entry.file_bytes;
			tex_mem.rgba8_base_bytes += entry.rgba8_base_bytes;
			tex_mem.rgba8_mip_bytes += entry.rgba8_mip_bytes;
			tex_mem.desktop_bc_mip_bytes += entry.desktop_bc_mip_bytes;
			tex_mem.mobile_etc2_mip_bytes += entry.mobile_etc2_mip_bytes;
			tex_mem.missing_packs += u64::from(entry.packs_expected.saturating_sub(entry.packs_loaded));
			let bucket = match entry.source {
				TextureSource::Transcode => &mut tex_mem.sources.transcode,
				TextureSource::Upload => &mut tex_mem.sources.upload,
				TextureSource::Local => &mut tex_mem.sources.local,
				TextureSource::Studio => &mut tex_mem.sources.studio,
			};
			bucket.count += 1;
			bucket.file_bytes += entry.file_bytes;
			bucket.rgba8_mip_bytes += entry.rgba8_mip_bytes;
			bucket.desktop_bc_mip_bytes += entry.desktop_bc_mip_bytes;
			bucket.mobile_etc2_mip_bytes += entry.mobile_etc2_mip_bytes;
			tex_entries.push(entry);
		}
		tex_mem.file_bytes_partial = tex_mem.missing > 0 || tex_mem.missing_packs > 0;
		tex_entries.sort_by(|a, b| b.rgba8_mip_bytes.cmp(&a.rgba8_mip_bytes).then_with(|| a.id.cmp(&b.id)));
		tex_entries.truncate(TOP_LIST);
		tex_mem.top = tex_entries;

		let uncounted_assets = mesh_mem.missing + tex_mem.missing;
		let partial = uncounted_assets > 0 || tex_mem.missing_packs > 0;
		let uncounted_bytes = partial.then(|| {
			let mut parts = Vec::new();
			if mesh_mem.missing > 0 {
				parts.push(format!("{} missing meshes ({} instances)", mesh_mem.missing, mesh_mem.missing_instances));
			}
			if tex_mem.missing > 0 {
				parts.push(format!("{} missing textures ({} references)", tex_mem.missing, tex_mem.missing_references));
			}
			if tex_mem.missing_packs > 0 {
				parts.push(format!("{} missing progressive KTX2 pack files (excluded from fileBytes)", tex_mem.missing_packs));
			}
			format!("unknown: bytes of {} could not be read; reported totals exclude them and are partial", parts.join(", "))
		});
		let missing = MissingMemory {
			meshes: mesh_mem.missing,
			mesh_instances: mesh_mem.missing_instances,
			textures: tex_mem.missing,
			texture_references: tex_mem.missing_references,
			transcode_packs: tex_mem.missing_packs,
			assets: uncounted_assets,
			uncounted_bytes,
			mesh_ids: missing_mesh_ids,
			texture_ids: missing_tex_ids,
		};
		let total_gpu_bytes = TotalGpuBytes {
			partial,
			uncounted_assets,
			rgba8: mesh_mem.gpu_bytes + tex_mem.rgba8_mip_bytes,
			desktop_bc: mesh_mem.gpu_bytes + tex_mem.desktop_bc_mip_bytes,
			mobile_etc2: mesh_mem.gpu_bytes + tex_mem.mobile_etc2_mip_bytes,
		};
		MemoryReport {
			partial,
			missing,
			meshes: mesh_mem,
			textures: tex_mem,
			total_gpu_bytes,
			..MemoryReport::default()
		}
	}
}

/// Measures deduplicated mesh and texture memory across the union of `scenes` (e.g. a timeline's
/// frames), using decoded mesh entries from `mesh_stats` and caching texture lookups in `cache`.
pub fn measure_many(scenes: &[&Scene], mesh_stats: &HashMap<String, Arc<MeshStats>>, assets: &Assets, cache: &mut TextureCache) -> MemoryReport {
	let mut collector = MemoryCollector::default();
	for scene in scenes {
		collector.add_scene(scene, assets);
	}
	collector.finish(mesh_stats, assets, cache)
}

/// Concise single-line memory summary for `analysis.txt` and `cost` reports.
pub fn summary_line(memory: &MemoryReport, phone: bool) -> String {
	let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
	let m = &memory.meshes;
	let t = &memory.textures;
	let tot = &memory.total_gpu_bytes;
	let kind_label = if memory.partial { "PARTIAL upper-bound estimate" } else { "upper-bound estimate" };
	let mesh_part = if m.missing > 0 {
		format!("{} unique meshes ({} loaded, {} missing — bytes uncounted; {:.2} MB est GPU, {:.2} MB files)", m.unique, m.loaded, m.missing, mb(m.gpu_bytes), mb(m.file_bytes))
	} else {
		format!("{} unique meshes ({:.2} MB est GPU, {:.2} MB files)", m.unique, mb(m.gpu_bytes), mb(m.file_bytes))
	};
	let (format_tag, tex_compressed, tot_compressed) = if phone {
		("ETC2", t.mobile_etc2_mip_bytes, tot.mobile_etc2)
	} else {
		("BCn", t.desktop_bc_mip_bytes, tot.desktop_bc)
	};
	let source_bytes = |s: &TextureSourceTotals| if phone { s.mobile_etc2_mip_bytes } else { s.desktop_bc_mip_bytes };
	let mut prov = Vec::new();
	if t.sources.transcode.count > 0 {
		prov.push(format!("{} transcodes ({:.2} MB {format_tag})", t.sources.transcode.count, mb(source_bytes(&t.sources.transcode))));
	}
	if t.sources.upload.count > 0 {
		prov.push(format!("{} uploads ({:.2} MB {format_tag}; dims may differ from KTX2)", t.sources.upload.count, mb(source_bytes(&t.sources.upload))));
	}
	if t.sources.local.count > 0 {
		prov.push(format!("{} local", t.sources.local.count));
	}
	if t.sources.studio.count > 0 {
		prov.push(format!("{} studio", t.sources.studio.count));
	}
	if t.missing > 0 {
		prov.push(format!("{} missing — bytes uncounted", t.missing));
	} else {
		prov.push("0 missing".into());
	}
	if t.missing_packs > 0 {
		prov.push(format!("{} pack files missing", t.missing_packs));
	}
	let total_prefix = if memory.partial { "PARTIAL total est GPU upper bound" } else { "total est GPU upper bound" };
	let tail = if memory.partial {
		format!("uncounted: {} meshes, {} textures, {} packs; excludes built-in materials, primitives, terrain, UI", m.missing, t.missing, t.missing_packs)
	} else {
		"excludes built-in materials, primitives, terrain, UI".into()
	};
	let basis_desc = if phone {
		"measured files, assumed mesh layout, derived full-chain ETC2 [vkFormat 147/151] & RGBA8, no ASTC recorded"
	} else {
		"measured files, assumed mesh layout, derived full-chain BC1/BC3 & RGBA8"
	};
	format!(
		"memory ({kind_label}; {basis_desc}): {mesh_part}, {} unique textures ({}; {:.2} MB {format_tag}, {:.2} MB RGBA8, {:.2} MB files); {total_prefix} {:.2} MB {format_tag} / {:.2} MB RGBA8 ({tail})",
		t.unique,
		prov.join(", "),
		mb(tex_compressed),
		mb(t.rgba8_mip_bytes),
		mb(t.file_bytes),
		mb(tot_compressed),
		mb(tot.rgba8)
	)
}

struct PackMaps {
	usage: u32,
	color: Option<String>,
	normal: Option<String>,
	metalness: Option<String>,
	roughness: Option<String>,
	emissive: Option<String>,
}

fn parse_pack_xml(bytes: &[u8]) -> Option<PackMaps> {
	let text = std::str::from_utf8(bytes).ok()?;
	if !text.contains("<texturepack_version>") {
		return None;
	}
	let tag = |name: &str| -> Option<String> {
		let open = format!("<{name}");
		let mut from = 0;
		let start = loop {
			let at = text[from..].find(&open)? + from + open.len();
			match text[at..].chars().next() {
				Some('>') => break at + 1,
				Some(' ') => break text[at..].find('>')? + at + 1,
				_ => from = at,
			}
		};
		let end = text[start..].find('<')? + start;
		let value = text[start..end].trim();
		(!value.is_empty()).then(|| value.to_string())
	};
	let map = |name: &str| -> Option<String> {
		let value = tag(name)?;
		Some(if value.bytes().all(|byte| byte.is_ascii_digit()) { format!("rbxassetid://{value}") } else { value })
	};
	Some(PackMaps {
		usage: tag("usage").and_then(|v| v.parse().ok()).unwrap_or(0),
		color: map("color"),
		normal: map("normal"),
		metalness: map("metalness"),
		roughness: map("roughness"),
		emissive: map("emissive"),
	})
}

fn add_uri(out: &mut BTreeMap<String, TextureWanted>, uri: &str, prefer_transcode: bool, is_normal: bool) {
	let trimmed = uri.trim();
	if trimmed.is_empty() {
		return;
	}
	let key = meshes::canonical(trimmed);
	out.entry(key)
		.and_modify(|item| {
			item.references += 1;
			item.is_normal |= is_normal;
		})
		.or_insert_with(|| TextureWanted {
			uri: trimmed.to_string(),
			pack_layer: None,
			prefer_transcode,
			is_normal,
			references: 1,
		});
}

fn add_pack_layer(out: &mut BTreeMap<String, TextureWanted>, pack: &str, layer: Transcode, suffix: &str, is_normal: bool) {
	let key = format!("{}#{suffix}", meshes::canonical(pack));
	out.entry(key)
		.and_modify(|item| {
			item.references += 1;
			item.is_normal |= is_normal;
		})
		.or_insert_with(|| TextureWanted {
			uri: pack.trim().to_string(),
			pack_layer: Some(layer),
			prefer_transcode: true,
			is_normal,
			references: 1,
		});
}

fn collect_scene_textures(
	scene: &Scene,
	assets: &Assets,
	use_transcodes: bool,
	pack_cache: &mut HashMap<String, Option<PackMaps>>,
	out: &mut BTreeMap<String, TextureWanted>,
) {
	for part in &scene.parts {
		if let Some(texture) = &part.texture {
			add_uri(out, texture, true, false);
		}
		for decal in &part.decals {
			add_uri(out, &decal.texture, true, false);
		}
		if let Some(appearance) = &part.surface_appearance {
			let explicit = [
				(&appearance.color_map, false),
				(&appearance.normal_map, true),
				(&appearance.metalness_map, false),
				(&appearance.roughness_map, false),
				(&appearance.emissive_mask, false),
			];
			if let Some(pack) = appearance.texture_pack.as_deref().filter(|pack| use_transcodes && is_asset_id(pack)) {
				let xml = pack_cache.entry(meshes::canonical(pack)).or_insert_with(|| assets.fetch(pack).ok().and_then(|f| parse_pack_xml(&f.bytes)));
				let is_sa_pack = xml.as_ref().is_none_or(|xml| xml.usage == 0);
				if is_sa_pack {
					add_pack_layer(out, pack, Transcode::PackColor, "color", false);
					add_pack_layer(out, pack, Transcode::PackNormal, "normal", true);
					add_pack_layer(out, pack, Transcode::PackMetalnessRoughness, "mr", false);
					if let Some(emissive) = &appearance.emissive_mask {
						add_uri(out, emissive, true, false);
					}
				} else if let Some(xml) = xml {
					for (map, is_normal) in [(&xml.color, false), (&xml.normal, true), (&xml.metalness, false), (&xml.roughness, false), (&xml.emissive, false)] {
						if let Some(uri) = map {
							add_uri(out, uri, true, is_normal);
						}
					}
				}
			} else {
				let mut any = false;
				for (map, is_normal) in explicit {
					if let Some(uri) = map {
						any = true;
						add_uri(out, uri, true, is_normal);
					}
				}
				if !any && let Some(pack) = appearance.texture_pack.as_deref() {
					if let Some(xml) = pack_cache.entry(meshes::canonical(pack)).or_insert_with(|| assets.fetch(pack).ok().and_then(|f| parse_pack_xml(&f.bytes))) {
						for (map, is_normal) in [(&xml.color, false), (&xml.normal, true), (&xml.metalness, false), (&xml.roughness, false), (&xml.emissive, false)] {
							if let Some(uri) = map {
								add_uri(out, uri, true, is_normal);
							}
						}
					} else {
						// Pack XML could not be read; record the pack itself so it surfaces under `missing`.
						add_uri(out, pack, false, false);
					}
				}
			}
		}
		if let Some(composite) = &part.composite {
			for layer in [&composite.pants, &composite.shirt, &composite.graphic].into_iter().flatten() {
				add_uri(out, &layer.image, false, false);
			}
		}
		if let Some(variant) = scene.materials.variant(part.material, &part.material_variant) {
			let mut any = false;
			for (map, is_normal) in [
				(&variant.color_map, false),
				(&variant.normal_map, true),
				(&variant.metalness_map, false),
				(&variant.roughness_map, false),
				(&variant.emissive_mask, false),
			] {
				if let Some(uri) = map {
					any = true;
					add_uri(out, uri, false, is_normal);
				}
			}
			if !any && let Some(pack) = variant.texture_pack.as_deref() {
				if let Some(xml) = pack_cache.entry(meshes::canonical(pack)).or_insert_with(|| assets.fetch(pack).ok().and_then(|f| parse_pack_xml(&f.bytes))) {
					for (map, is_normal) in [(&xml.color, false), (&xml.normal, true), (&xml.metalness, false), (&xml.roughness, false), (&xml.emissive, false)] {
						if let Some(uri) = map {
							add_uri(out, uri, false, is_normal);
						}
					}
				} else {
					add_uri(out, pack, false, false);
				}
			}
		}
	}
	for emitter in &scene.particles {
		if let Some(texture) = &emitter.texture {
			add_uri(out, texture, true, false);
		}
	}
	for beam in &scene.beams {
		if let Some(texture) = &beam.texture {
			add_uri(out, texture, true, false);
		}
	}
	for trail in &scene.trails {
		if let Some(texture) = &trail.texture {
			add_uri(out, texture, true, false);
		}
	}
	if let Some(sky) = &scene.environment.sky {
		for face in &sky.skybox {
			if !is_default_sky_texture(face) {
				add_uri(out, face, true, false);
			}
		}
		if !is_default_sky_texture(&sky.sun_texture) {
			add_uri(out, &sky.sun_texture, true, false);
		}
		if !is_default_sky_texture(&sky.moon_texture) {
			add_uri(out, &sky.moon_texture, true, false);
		}
	}
}

const KTX2_IDENTIFIER: [u8; 12] = [0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb, b'\r', b'\n', 0x1a, b'\n'];

struct Ktx2HeaderMeta {
	width: u32,
	height: u32,
	layers: u32,
	depth: u32,
	format: PixelFormat,
	/// `None` when the format can carry alpha and KVD has no `avgAlpha` (falls back to full decode).
	has_alpha: Option<bool>,
	packs_expected: u32,
}

fn u32_le(bytes: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn parse_ktx2_kvd_entries(kvd: &[u8]) -> (Option<u32>, Option<u32>, Option<u32>, Vec<(u32, u32)>) {
	let mut transcoded_w = None;
	let mut transcoded_h = None;
	let mut avg_alpha = None;
	let mut pack_entries: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
	let mut offset = 0usize;
	while offset + 4 <= kvd.len() {
		let len = u32_le(kvd, offset) as usize;
		let start = offset + 4;
		let Some(end) = start.checked_add(len).filter(|&e| e <= kvd.len()) else {
			break;
		};
		let entry = &kvd[start..end];
		if let Some(nul) = entry.iter().position(|&b| b == 0) {
			let key = std::str::from_utf8(&entry[..nul]).ok();
			let mut raw_val = &entry[nul + 1..];
			while let [rest @ .., 0] = raw_val {
				raw_val = rest;
			}
			let val = std::str::from_utf8(raw_val).ok().map(str::trim);
			if let (Some(k), Some(v)) = (key, val) {
				match k {
					"transcodedWidth" => transcoded_w = v.parse::<u32>().ok().filter(|&n| n > 0),
					"transcodedHeight" => transcoded_h = v.parse::<u32>().ok().filter(|&n| n > 0),
					"avgAlpha" => avg_alpha = v.parse::<u32>().ok(),
					_ if k.starts_with("pack_") => {
						if let Ok(idx) = k["pack_".len()..].parse::<u32>() {
							let mut parts = v.split(',').map(|p| p.trim().parse::<u32>().ok());
							if let (Some(first), Some(count)) = (parts.next().flatten(), parts.next().flatten())
								&& count > 0
							{
								pack_entries.insert(idx, (first, count));
							}
						}
					}
					_ => {}
				}
			}
		}
		offset = end.div_ceil(4) * 4;
	}
	let mut table = Vec::new();
	for idx in 0u32.. {
		let Some(&pair) = pack_entries.get(&idx) else {
			break;
		};
		table.push(pair);
	}
	(transcoded_w, transcoded_h, avg_alpha, table)
}

/// Parses a KTX2 file's 80-byte header and key/value data without decompressing level payloads.
fn parse_ktx2_header(bytes: &[u8], layer: Transcode) -> Option<Ktx2HeaderMeta> {
	if bytes.len() < 80 || bytes[..12] != KTX2_IDENTIFIER {
		return None;
	}
	let vk = u32_le(bytes, 12);
	let (format, opaque) = match vk {
		131 | 132 => (PixelFormat::Bc1, true),
		133 | 134 => (PixelFormat::Bc1, false),
		135 | 136 => (PixelFormat::Bc2, false),
		137 | 138 => (PixelFormat::Bc3, false),
		139 => (PixelFormat::Bc4, true),
		141 => (PixelFormat::Bc5, true),
		145 | 146 => (PixelFormat::Bc7, false),
		9 | 15 | 16 | 22 | 23 | 29 | 30 | 36 => (PixelFormat::Rgba8, true),
		37 | 43 | 44 | 50 => (PixelFormat::Rgba8, false),
		_ => return None,
	};
	let raw_w = u32_le(bytes, 20);
	if raw_w == 0 {
		return None;
	}
	let raw_h = u32_le(bytes, 24).max(1);
	let depth = u32_le(bytes, 28).max(1);
	let layer_count = u32_le(bytes, 32).max(1);
	let face_count = u32_le(bytes, 36);
	if !matches!(face_count, 1 | 6) {
		return None;
	}
	let layers = layer_count.checked_mul(face_count)?;
	let kvd_offset = u32_le(bytes, 56) as usize;
	let kvd_len = u32_le(bytes, 60) as usize;
	let kvd = if kvd_len > 0 { bytes.get(kvd_offset..kvd_offset.checked_add(kvd_len)?)? } else { &[] };
	let (transcoded_w, transcoded_h, avg_alpha, table) = parse_ktx2_kvd_entries(kvd);
	let (width, height) = match (transcoded_w, transcoded_h) {
		(Some(w), Some(h)) => (w, h),
		_ => {
			let shift = table.first().map_or(0, |&(first_level, _)| first_level.min(16));
			(raw_w.checked_shl(shift).unwrap_or(raw_w).max(1), raw_h.checked_shl(shift).unwrap_or(raw_h).max(1))
		}
	};
	let has_alpha = if opaque || layer == Transcode::PackNormal {
		Some(false)
	} else {
		avg_alpha.map(|alpha| alpha < 255)
	};
	Some(Ktx2HeaderMeta {
		width,
		height,
		layers,
		depth,
		format,
		has_alpha,
		packs_expected: (table.len() as u32).max(1),
	})
}

/// Fast header-only pack-count callback for [`Assets::warm`], avoiding zstd decompression.
fn ktx2_pack_count_fast(pack0: &[u8]) -> u32 {
	if pack0.len() >= 80 && pack0[..12] == KTX2_IDENTIFIER {
		let kvd_offset = u32_le(pack0, 56) as usize;
		let kvd_len = u32_le(pack0, 60) as usize;
		if let Some(end) = kvd_offset.checked_add(kvd_len)
			&& end <= pack0.len()
		{
			let (_, _, _, table) = parse_ktx2_kvd_entries(&pack0[kvd_offset..end]);
			return table.len() as u32;
		}
	}
	crate::render::transcode_packs(pack0)
}

/// Fast header-only reader for opaque RGB/grayscale PNGs (no alpha channel and no `tRNS` chunk),
/// avoiding full zlib pixel decompression when dimensions and opacity are known from headers alone.
fn parse_png_opaque_header(bytes: &[u8]) -> Option<(u32, u32, u32, u32, PixelFormat, bool)> {
	if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
		return None;
	}
	let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
	let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
	let color_type = bytes[25];
	if width == 0 || height == 0 || !matches!(color_type, 0 | 2) {
		return None;
	}
	let mut offset = 33usize;
	while offset + 12 <= bytes.len() {
		let len = u32::from_be_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]) as usize;
		let kind = &bytes[offset + 4..offset + 8];
		if kind == b"tRNS" {
			return None;
		}
		if kind == b"IDAT" {
			return Some((width, height, 1, 1, PixelFormat::Rgba8, false));
		}
		offset = offset.checked_add(12)?.checked_add(len)?;
	}
	None
}

enum ProbeResult {
	Done(Option<TextureInfo>),
	MissingPacks { info: TextureInfo, uri: String, layer: Transcode, packs: Vec<u32> },
	NeedsNetwork,
}

fn inspect_transcode_pack0(pack0: &[u8], layer: Transcode) -> Option<(TextureInfo, u32)> {
	if let Some(meta) = parse_ktx2_header(pack0, layer)
		&& let Some(has_alpha) = meta.has_alpha
	{
		let info = TextureInfo {
			source: TextureSource::Transcode,
			width: meta.width,
			height: meta.height,
			layers: meta.layers.max(1),
			depth: meta.depth.max(1),
			format: meta.format,
			has_alpha,
			file_bytes: pack0.len() as u64,
			packs_loaded: 1,
			packs_expected: meta.packs_expected,
		};
		return Some((info, meta.packs_expected));
	}
	// Fallback when header lacks alpha info (e.g. non-Roblox BC3 without `avgAlpha`).
	let first = ror_image::decode(pack0).ok()?;
	let table = ror_image::ktx2_pack_table(&first);
	let (width, height) = match (
		first.meta("transcodedWidth").and_then(|value| value.trim().parse::<u32>().ok()),
		first.meta("transcodedHeight").and_then(|value| value.trim().parse::<u32>().ok()),
	) {
		(Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
		_ => {
			let shift = table.first().map_or(0, |&(first_level, _)| first_level.min(16));
			(first.width.checked_shl(shift).unwrap_or(first.width).max(1), first.height.checked_shl(shift).unwrap_or(first.height).max(1))
		}
	};
	let packs_expected = (table.len() as u32).max(1);
	let has_alpha = if layer == Transcode::Image && first.dxt5nm {
		first.meta("avgAlpha").and_then(|alpha| alpha.trim().parse::<u32>().ok()).is_none_or(|alpha| alpha < 255)
	} else {
		first.has_alpha
	};
	let info = TextureInfo {
		source: TextureSource::Transcode,
		width,
		height,
		layers: first.layers.max(1),
		depth: first.depth.max(1),
		format: first.format,
		has_alpha,
		file_bytes: pack0.len() as u64,
		packs_loaded: 1,
		packs_expected,
	};
	Some((info, packs_expected))
}

fn probe_transcode_offline(uri: &str, layer: Transcode, offline: &Assets, run_offline: bool) -> Option<ProbeResult> {
	let pack0 = offline.fetch_transcode(uri, layer, 0).ok()?;
	let (mut info, packs_expected) = inspect_transcode_pack0(&pack0.bytes, layer)?;
	let mut missing = Vec::new();
	for pack in 1..packs_expected {
		if let Ok(fetched) = offline.fetch_transcode(uri, layer, pack) {
			info.file_bytes += fetched.bytes.len() as u64;
			info.packs_loaded += 1;
		} else {
			missing.push(pack);
		}
	}
	if missing.is_empty() || run_offline {
		Some(ProbeResult::Done(Some(info)))
	} else {
		Some(ProbeResult::MissingPacks { info, uri: uri.to_string(), layer, packs: missing })
	}
}

fn probe_texture_offline(item: &TextureWanted, offline: &Assets, run_offline: bool, use_transcodes: bool) -> ProbeResult {
	if let Some(layer) = item.pack_layer {
		return match probe_transcode_offline(&item.uri, layer, offline, run_offline) {
			Some(res) => res,
			None if run_offline || !is_asset_id(&item.uri) => ProbeResult::Done(None),
			None => ProbeResult::NeedsNetwork,
		};
	}
	if is_asset_id(&item.uri) && use_transcodes && item.prefer_transcode {
		if let Some(res) = probe_transcode_offline(&item.uri, Transcode::Image, offline, run_offline) {
			return res;
		}
		if run_offline {
			return ProbeResult::Done(inspect_raw(&item.uri, offline));
		}
		return ProbeResult::NeedsNetwork;
	}
	if let Some(info) = inspect_raw(&item.uri, offline) {
		return ProbeResult::Done(Some(info));
	}
	if is_asset_id(&item.uri) && use_transcodes {
		if let Some(res) = probe_transcode_offline(&item.uri, Transcode::Image, offline, run_offline) {
			return res;
		}
	}
	if run_offline || !is_asset_id(&item.uri) {
		ProbeResult::Done(None)
	} else {
		ProbeResult::NeedsNetwork
	}
}

fn inspect_transcode(uri: &str, layer: Transcode, assets: &Assets) -> Option<TextureInfo> {
	let pack0 = assets.fetch_transcode(uri, layer, 0).ok()?;
	let (mut info, packs_expected) = inspect_transcode_pack0(&pack0.bytes, layer)?;
	for pack in 1..packs_expected {
		if let Ok(fetched) = assets.fetch_transcode(uri, layer, pack) {
			info.file_bytes += fetched.bytes.len() as u64;
			info.packs_loaded += 1;
		}
	}
	Some(info)
}

fn inspect_raw(uri: &str, assets: &Assets) -> Option<TextureInfo> {
	let fetched = assets.fetch(uri).ok()?;
	let source = match ContentId::parse(uri) {
		Ok(ContentId::StudioAsset(_)) => TextureSource::Studio,
		Ok(ContentId::File(_)) => TextureSource::Local,
		_ => TextureSource::Upload,
	};
	if let Some((width, height, layers, depth, format, has_alpha)) = parse_png_opaque_header(&fetched.bytes) {
		return Some(TextureInfo {
			source,
			width,
			height,
			layers,
			depth,
			format,
			has_alpha,
			file_bytes: fetched.bytes.len() as u64,
			packs_loaded: 1,
			packs_expected: 1,
		});
	}
	let ext = uri.rsplit('.').next().filter(|ext| ext.len() <= 8).unwrap_or("");
	let image = ror_image::decode_with_extension(&fetched.bytes, ext).ok()?;
	Some(TextureInfo {
		source,
		width: image.width,
		height: image.height,
		layers: image.layers.max(1),
		depth: image.depth.max(1),
		format: image.format,
		has_alpha: image.has_alpha,
		file_bytes: fetched.bytes.len() as u64,
		packs_loaded: 1,
		packs_expected: 1,
	})
}

fn inspect_texture(item: &TextureWanted, assets: &Assets, use_transcodes: bool) -> Option<TextureInfo> {
	if let Some(layer) = item.pack_layer {
		return inspect_transcode(&item.uri, layer, assets);
	}
	if is_asset_id(&item.uri) && use_transcodes && item.prefer_transcode {
		if let Some(info) = inspect_transcode(&item.uri, Transcode::Image, assets) {
			return Some(info);
		}
		return inspect_raw(&item.uri, assets);
	}
	if let Some(info) = inspect_raw(&item.uri, assets) {
		return Some(info);
	}
	if is_asset_id(&item.uri) && use_transcodes {
		return inspect_transcode(&item.uri, Transcode::Image, assets);
	}
	None
}

fn format_name(format: PixelFormat) -> &'static str {
	match format {
		PixelFormat::Rgba8 => "rgba8",
		PixelFormat::Bc1 => "bc1",
		PixelFormat::Bc2 => "bc2",
		PixelFormat::Bc3 => "bc3",
		PixelFormat::Bc4 => "bc4",
		PixelFormat::Bc5 => "bc5",
		PixelFormat::Bc7 => "bc7",
	}
}

fn mip_chain_bytes(width: u32, height: u32, depth: u32, layers: u32, per_slice: impl Fn(u32, u32) -> u64) -> u64 {
	let mut w = width.max(1);
	let mut h = height.max(1);
	let mut d = depth.max(1);
	let layers = layers.max(1) as u64;
	let mut total = 0u64;
	loop {
		total += per_slice(w, h) * d as u64 * layers;
		if w == 1 && h == 1 && d == 1 {
			break;
		}
		w = (w >> 1).max(1);
		h = (h >> 1).max(1);
		d = (d >> 1).max(1);
	}
	total
}

fn block_bytes_for(w: u32, h: u32, bw: u32, bh: u32, bytes_per_block: u64) -> u64 {
	u64::from(w.div_ceil(bw)) * u64::from(h.div_ceil(bh)) * bytes_per_block
}

fn build_texture_entry(id: String, references: u64, is_normal: bool, is_pack_layer: bool, info: &TextureInfo) -> TextureEntry {
	let rgba8_base_bytes = u64::from(info.width.max(1)) * u64::from(info.height.max(1)) * 4 * u64::from(info.depth.max(1)) * u64::from(info.layers.max(1));
	let rgba8_mip_bytes = mip_chain_bytes(info.width, info.height, info.depth, info.layers, |w, h| u64::from(w) * u64::from(h) * 4);
	// Roblox desktop KTX2 transcodes use BC1 (8 B/4x4 block) and BC3 (16 B/4x4 block) only (notes/image.md);
	// never emit a fabricated "bc7" label for uploaded images.
	let (bc_block_bytes, desktop_bc_format) = match info.format.block_bytes() {
		Some(bytes) => (bytes as u64, format_name(info.format)),
		None if !info.has_alpha && !is_normal => (8, "bc1"),
		None => (16, "bc3"),
	};
	let desktop_bc_mip_bytes = mip_chain_bytes(info.width, info.height, info.depth, info.layers, |w, h| block_bytes_for(w, h, 4, 4, bc_block_bytes));
	// Mobile ETC2 strictly follows notes/image.md: vkFormat 147 (ETC2 RGB8, 8 B/4x4 block) for opaque
	// TexturePack layers (option bit 0x08), and vkFormat 151 (ETC2 RGBA8, 16 B/4x4 block) for mobile
	// rollout images (/////Ck=) and alpha/normal pack layers. No ASTC format is recorded.
	let (etc2_block_bytes, mobile_etc2_format) = if is_pack_layer && bc_block_bytes == 8 { (8u64, "etc2_rgb8") } else { (16u64, "etc2_rgba8") };
	let mobile_etc2_mip_bytes = mip_chain_bytes(info.width, info.height, info.depth, info.layers, |w, h| block_bytes_for(w, h, 4, 4, etc2_block_bytes));
	TextureEntry {
		id,
		references,
		source: info.source,
		width: info.width,
		height: info.height,
		layers: info.layers,
		depth: info.depth,
		format: format_name(info.format),
		desktop_bc_format,
		mobile_etc2_format,
		has_alpha: info.has_alpha,
		packs_loaded: info.packs_loaded,
		packs_expected: info.packs_expected,
		file_bytes: info.file_bytes,
		rgba8_base_bytes,
		rgba8_mip_bytes,
		desktop_bc_mip_bytes,
		mobile_etc2_mip_bytes,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use glam::{Affine3A, Vec3};
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{ContentId, Enum, Vector3};

	fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: bool) -> u64 {
		let file = std::fs::File::create(path).unwrap();
		let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
		encoder.set_color(if rgba { png::ColorType::Rgba } else { png::ColorType::Rgb });
		encoder.set_depth(png::BitDepth::Eight);
		let mut writer = encoder.write_header().unwrap();
		let pixel: &[u8] = if rgba { &[180, 90, 220, 128] } else { &[180, 90, 220] };
		let mut data = Vec::with_capacity(width as usize * height as usize * pixel.len());
		for _ in 0..(width * height) {
			data.extend_from_slice(pixel);
		}
		writer.write_image_data(&data).unwrap();
		drop(writer);
		std::fs::metadata(path).unwrap().len()
	}

	fn write_dds_dxt1(path: &std::path::Path, width: u32, height: u32) -> u64 {
		let mut header = vec![0u8; 128];
		header[0..4].copy_from_slice(b"DDS ");
		let put = |buf: &mut [u8], off: usize, v: u32| buf[4 + off..8 + off].copy_from_slice(&v.to_le_bytes());
		put(&mut header, 0, 124);
		put(&mut header, 4, 0x000A_1007);
		put(&mut header, 8, height);
		put(&mut header, 12, width);
		let blocks = width.div_ceil(4) * height.div_ceil(4);
		put(&mut header, 16, blocks * 8);
		put(&mut header, 24, 1);
		put(&mut header, 72, 32);
		put(&mut header, 76, 0x4);
		header[4 + 80..4 + 84].copy_from_slice(b"DXT1");
		put(&mut header, 104, 0x1000);
		header.extend(std::iter::repeat_n(0u8, (blocks * 8) as usize));
		std::fs::write(path, &header).unwrap();
		header.len() as u64
	}

	#[test]
	fn deduplicates_meshes_and_textures_and_matches_closed_form_bytes() {
		let dir = std::env::temp_dir().join(format!("ror-memory-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();

		let mesh_path = dir.join("quad4.mesh");
		let mesh_text = concat!(
			"version 1.01\n4\n",
			"[-1,0,-1][0,1,0][0,0,0][1,0,-1][0,1,0][1,0,0][1,0,1][0,1,0][0,1,0]",
			"[-1,0,-1][0,1,0][0,0,0][1,0,1][0,1,0][1,0,0][-1,0,1][0,1,0][0,1,0]",
			"[-1,1,-1][0,1,0][0,0,0][1,1,-1][0,1,0][1,0,0][1,1,1][0,1,0][0,1,0]",
			"[-1,1,-1][0,1,0][0,0,0][1,1,1][0,1,0][1,0,0][-1,1,1][0,1,0][0,1,0]\n"
		);
		std::fs::write(&mesh_path, mesh_text).unwrap();
		let mesh_file_bytes = mesh_text.len() as u64;

		let tex_rgb = dir.join("rgb64.png");
		let tex_rgba = dir.join("rgba128x64.png");
		let tex_dds = dir.join("dxt32.dds");
		let rgb_bytes = write_png(&tex_rgb, 64, 64, false);
		let rgba_bytes = write_png(&tex_rgba, 128, 64, true);
		let dds_bytes = write_dds_dxt1(&tex_dds, 32, 32);

		let uri = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace('\\', "/"));
		let mut model = ModelBuilder::new("Mem");
		let mp1 = model.child(
			model.model,
			InstanceBuilder::new("MeshPart")
				.with_name("A")
				.with_property("Size", Vector3::new(2.0, 2.0, 2.0))
				.with_property("InitialSize", Vector3::new(2.0, 1.0, 2.0))
				.with_property("MeshId", ContentId::from(uri(&mesh_path)))
				.with_property("TextureID", ContentId::from(uri(&tex_rgb)))
				.with_property("CFrame", crate::scene::build::cframe(Affine3A::IDENTITY)),
		);
		let mp2 = model.child(
			model.model,
			InstanceBuilder::new("MeshPart")
				.with_name("B")
				.with_property("Size", Vector3::new(2.0, 2.0, 2.0))
				.with_property("InitialSize", Vector3::new(2.0, 1.0, 2.0))
				.with_property("MeshId", ContentId::from(uri(&mesh_path)))
				.with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(Vec3::X * 4.0))),
		);
		model.child(mp2, InstanceBuilder::new("Decal").with_property("Texture", ContentId::from(uri(&tex_rgb))).with_property("Face", Enum::from_u32(1)));
		model.child(mp1, InstanceBuilder::new("Texture").with_property("Texture", ContentId::from(uri(&tex_dds))).with_property("Face", Enum::from_u32(5)));
		let host = model.part(&PartSpec { name: "Host".into(), transparency: 1.0, ..Default::default() });
		model.child(host, InstanceBuilder::new("ParticleEmitter").with_property("Texture", ContentId::from(uri(&tex_rgba))).with_property("Rate", 10.0f32));

		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(dir.join("cache")) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert_eq!(report.kind, "upper-bound-estimate");
		assert_eq!(report.deduplicated_by, "canonical-asset-id");
		assert_eq!(report.basis.mesh_file_bytes.basis, "measured");
		assert_eq!(report.basis.mesh_gpu_bytes.basis, "assumed");
		assert_eq!(report.basis.texture_mobile_etc2_bytes.basis, "derived");
		assert_eq!(report.basis.texture_mobile_astc_bytes.basis, "unrecorded");
		assert!(report.basis.texture_mobile_astc_bytes.settle_with.is_some());
		assert!(report.unmodeled.iter().any(|u| u.kind == "builtInMaterialPacks"));

		assert_eq!(report.meshes.unique, 1);
		assert_eq!(report.meshes.instances, 2);
		assert_eq!(report.meshes.file_bytes, mesh_file_bytes);
		assert_eq!(report.meshes.gpu_bytes, 408);

		assert_eq!(report.textures.unique, 3);
		assert_eq!(report.textures.references, 4);
		assert_eq!(report.textures.file_bytes, rgb_bytes + rgba_bytes + dds_bytes);
		assert_eq!(report.textures.rgba8_base_bytes, 64 * 64 * 4 + 128 * 64 * 4 + 32 * 32 * 4);
		assert_eq!(report.textures.rgba8_mip_bytes, 21_844 + 43_692 + 5_460);
		assert_eq!(report.textures.desktop_bc_mip_bytes, 2_744 + 10_960 + 696);
		// Non-pack images use vkFormat 151 (ETC2 RGBA8, 16 B/4x4 block) per notes/image.md
		assert_eq!(report.textures.mobile_etc2_mip_bytes, 5_488 + 10_960 + 1_392);
		assert_eq!(report.total_gpu_bytes.rgba8, 408 + 21_844 + 43_692 + 5_460);
		assert_eq!(report.total_gpu_bytes.desktop_bc, 408 + 2_744 + 10_960 + 696);
		assert_eq!(report.total_gpu_bytes.mobile_etc2, 408 + 5_488 + 10_960 + 1_392);
		assert!(report.textures.top.iter().all(|e| e.desktop_bc_format != "bc7"));
		assert!(report.textures.top.iter().any(|e| e.has_alpha && e.desktop_bc_format == "bc3"));

		let json = serde_json::to_value(&report).unwrap();
		assert_eq!(json["kind"], "upper-bound-estimate");
		assert_eq!(json["textures"]["mobileEtc2MipBytes"], 5_488 + 10_960 + 1_392);
		assert_eq!(json["totalGpuBytes"]["mobileEtc2"], 408 + 5_488 + 10_960 + 1_392);
		assert!(json["textures"].get("mobileAstc4x4MipBytes").is_none());
		assert!(json["textures"].get("mobileAstc6x6MipBytes").is_none());
		assert!(json["totalGpuBytes"].get("mobileAstc4x4").is_none());
		assert!(json["totalGpuBytes"].get("mobileAstc6x6").is_none());

		assert!(!report.partial);
		assert_eq!(report.missing.assets, 0);
		assert_eq!(report.missing.uncounted_bytes, None);
		assert_eq!(report.meshes.loaded, 1);
		assert_eq!(report.textures.loaded, 3);
		assert_eq!(report.textures.sources.local.count, 3);
		assert_eq!(report.textures.sources.local.desktop_bc_mip_bytes, report.textures.desktop_bc_mip_bytes);
		assert!(!report.textures.file_bytes_partial);
		let phone_line = summary_line(&report, true);
		let desktop_line = summary_line(&report, false);
		assert!(phone_line.contains("upper-bound estimate") && !phone_line.contains("PARTIAL") && phone_line.contains("0 missing") && phone_line.contains("ETC2") && phone_line.contains("no ASTC recorded") && !phone_line.contains("ASTC4x4"));
		assert!(desktop_line.contains("upper-bound estimate") && !desktop_line.contains("PARTIAL") && desktop_line.contains("0 missing") && desktop_line.contains("BC1/BC3") && !desktop_line.contains("ASTC") && !desktop_line.to_ascii_lowercase().contains("phone"));
		let _ = std::fs::remove_dir_all(&dir);
	}

	fn cache_write_key(state_dir: &std::path::Path, key: &str, bytes: &[u8]) {
		const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
		const PRIME: u128 = 0x0000000001000000000000000000013b;
		let mut hash = OFFSET;
		for b in key.as_bytes() {
			hash ^= *b as u128;
			hash = hash.wrapping_mul(PRIME);
		}
		let hex = format!("{hash:032x}");
		let dir = state_dir.join("assets").join(&hex[..2]);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join(format!("{hex}.bin")), bytes).unwrap();
	}

	fn make_ktx2_bc(vk: u32, width: u32, height: u32, kv: &[(&str, &str)]) -> Vec<u8> {
		let block_bytes = if vk == 131 { 8usize } else { 16usize };
		let blocks = (width.div_ceil(4) * height.div_ceil(4)) as usize;
		let payload = vec![0u8; blocks * block_bytes];
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
		let mut dfd = Vec::new();
		dfd.extend_from_slice(&28u32.to_le_bytes());
		dfd.extend_from_slice(&0u32.to_le_bytes());
		dfd.extend_from_slice(&((24u32 << 16) | 2).to_le_bytes());
		dfd.extend_from_slice(&[128, 1, 1, 0, 3, 3, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0]);
		let dfd_offset = 80 + 24;
		let kvd_offset = dfd_offset + dfd.len();
		let data_offset = kvd_offset + kvd.len();
		let mut file = Vec::new();
		file.extend_from_slice(&[0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb, b'\r', b'\n', 0x1a, b'\n']);
		for v in [vk, 1, width, height, 0, 0, 1, 1, 0u32] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		for v in [dfd_offset as u32, dfd.len() as u32, kvd_offset as u32, kvd.len() as u32] {
			file.extend_from_slice(&v.to_le_bytes());
		}
		file.extend_from_slice(&0u64.to_le_bytes());
		file.extend_from_slice(&0u64.to_le_bytes());
		file.extend_from_slice(&(data_offset as u64).to_le_bytes());
		file.extend_from_slice(&(payload.len() as u64).to_le_bytes());
		file.extend_from_slice(&(payload.len() as u64).to_le_bytes());
		file.extend_from_slice(&dfd);
		file.extend_from_slice(&kvd);
		file.extend_from_slice(&payload);
		file
	}

	#[test]
	fn missing_assets_partial_packs_and_upload_provenance_are_reported_explicitly() {
		let dir = std::env::temp_dir().join(format!("ror-memory-missing-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let cache_dir = dir.join("cache");
		std::fs::create_dir_all(&cache_dir).unwrap();

		// Asset 1001: cached uploaded RGB PNG (no KTX2 transcode cached -> Upload provenance)
		let upload_png = dir.join("u1001.png");
		write_png(&upload_png, 32, 32, false);
		cache_write_key(&cache_dir, "rbxassetid://1001", &std::fs::read(&upload_png).unwrap());

		// Asset 1002: cached KTX2 pack 0 declaring 2 packs, pack 1 absent offline -> Transcode provenance + 1 missing pack
		let ktx0 = make_ktx2_bc(131, 16, 16, &[("transcodedWidth", "32"), ("transcodedHeight", "32"), ("pack_0", "1,5"), ("pack_1", "0,1"), ("packIndex", "0")]);
		let key1002_p0 = format!("rbxassetid://1002#ktx2/7rdo/{}", Transcode::Image.fidelity(0));
		cache_write_key(&cache_dir, &key1002_p0, &ktx0);

		let mut model = ModelBuilder::new("Partial");
		// Missing mesh + missing texture 9999
		model.child(
			model.model,
			InstanceBuilder::new("MeshPart")
				.with_name("Broken")
				.with_property("Size", Vector3::new(2.0, 2.0, 2.0))
				.with_property("InitialSize", Vector3::new(2.0, 2.0, 2.0))
				.with_property("MeshId", ContentId::from("rbxassetid://8888"))
				.with_property("TextureID", ContentId::from("rbxassetid://9999"))
				.with_property("CFrame", crate::scene::build::cframe(Affine3A::IDENTITY)),
		);
		// Loaded upload 1001 + partially-packed transcode 1002
		let p = model.part(&PartSpec { name: "Holder".into(), ..Default::default() });
		model.child(p, InstanceBuilder::new("Decal").with_property("Texture", ContentId::from("rbxassetid://1001")).with_property("Face", Enum::from_u32(1)));
		model.child(p, InstanceBuilder::new("Decal").with_property("Texture", ContentId::from("rbxassetid://1002")).with_property("Face", Enum::from_u32(5)));

		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&cache_dir) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert!(report.partial);
		assert!(report.total_gpu_bytes.partial);
		assert_eq!(report.total_gpu_bytes.uncounted_assets, 2);
		assert_eq!(report.meshes.missing, 1);
		assert_eq!(report.meshes.loaded, 0);
		assert_eq!(report.meshes.missing_instances, 1);
		assert_eq!(report.missing.mesh_ids, vec!["rbxassetid://8888"]);
		assert_eq!(report.textures.unique, 3);
		assert_eq!(report.textures.loaded, 2);
		assert_eq!(report.textures.missing, 1);
		assert_eq!(report.textures.missing_references, 1);
		assert_eq!(report.textures.missing_packs, 1);
		assert!(report.textures.file_bytes_partial);
		assert_eq!(report.missing.texture_ids, vec!["rbxassetid://9999"]);
		assert!(report.missing.uncounted_bytes.as_deref().is_some_and(|s| s.contains("1 missing meshes") && s.contains("1 missing textures") && s.contains("1 missing progressive")));
		assert_eq!(report.textures.sources.upload.count, 1);
		assert_eq!(report.textures.sources.transcode.count, 1);
		assert_eq!(
			report.textures.sources.upload.desktop_bc_mip_bytes + report.textures.sources.transcode.desktop_bc_mip_bytes,
			report.textures.desktop_bc_mip_bytes
		);

		let line = summary_line(&report, false);
		assert!(line.contains("PARTIAL upper-bound estimate"), "{line}");
		assert!(line.contains("1 missing — bytes uncounted"), "{line}");
		assert!(line.contains("1 pack files missing"), "{line}");
		assert!(line.contains("1 transcodes"), "{line}");
		assert!(line.contains("1 uploads") && line.contains("dims may differ"), "{line}");
		assert!(line.contains("PARTIAL total est GPU upper bound"), "{line}");
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn normal_map_upload_is_priced_as_bc3_not_bc1() {
		let dir = std::env::temp_dir().join(format!("ror-memory-normal-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();

		let color_png = dir.join("color64.png");
		let normal_png = dir.join("normal64.png");
		write_png(&color_png, 64, 64, false);
		write_png(&normal_png, 64, 64, false);
		let uri = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace('\\', "/"));

		let mut model = ModelBuilder::new("Norm");
		let part = model.part(&PartSpec { name: "Block".into(), ..Default::default() });
		model.child(
			part,
			InstanceBuilder::new("SurfaceAppearance")
				.with_property("ColorMap", ContentId::from(uri(&color_png)))
				.with_property("NormalMap", ContentId::from(uri(&normal_png))),
		);
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(dir.join("cache")) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert_eq!(report.textures.unique, 2);
		assert_eq!(report.textures.missing, 0);
		let color = report.textures.top.iter().find(|e| e.id.contains("color64.png")).expect("color entry");
		let normal = report.textures.top.iter().find(|e| e.id.contains("normal64.png")).expect("normal entry");
		assert_eq!(color.desktop_bc_format, "bc1");
		assert_eq!(color.desktop_bc_mip_bytes, 2_744);
		assert_eq!(normal.desktop_bc_format, "bc3");
		assert_eq!(normal.desktop_bc_mip_bytes, 5_488);
		assert_eq!(report.textures.desktop_bc_mip_bytes, 2_744 + 5_488);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn sky_excludes_default_roblox_textures_and_counts_custom_sky_textures() {
		let dir = std::env::temp_dir().join(format!("ror-memory-sky-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();

		let custom_face = dir.join("custom_sky.png");
		let custom_bytes = write_png(&custom_face, 32, 32, false);
		let custom_uri = format!("file://{}", custom_face.to_string_lossy().replace('\\', "/"));

		let mut dom = rbx_dom_weak::WeakDom::new(InstanceBuilder::new("DataModel"));
		let root = dom.root_ref();
		let lighting = dom.insert(root, InstanceBuilder::new("Lighting"));
		dom.insert(
			lighting,
			InstanceBuilder::new("Sky")
				.with_property("SkyboxBk", ContentId::from(custom_uri.as_str()))
				.with_property("SkyboxDn", ContentId::from("rbxasset://textures/sky/sky512_dn.tex"))
				.with_property("SkyboxFt", ContentId::from("rbxasset://textures/sky/sky512_ft.tex"))
				.with_property("SkyboxLf", ContentId::from("rbxasset://textures/sky/sky512_lf.tex"))
				.with_property("SkyboxRt", ContentId::from("rbxasset://textures/sky/sky512_rt.tex"))
				.with_property("SkyboxUp", ContentId::from("rbxasset://textures/sky/sky512_up.tex"))
				.with_property("SunTextureId", ContentId::from("rbxasset://sky/sun.jpg"))
				.with_property("MoonTextureId", ContentId::from("rbxasset://sky/moon.jpg")),
		);
		let document = crate::scene::load::Document::new(dom);
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(dir.join("cache")) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert_eq!(report.textures.unique, 1, "only custom sky face should be counted: {:?}", report.textures.top);
		assert_eq!(report.textures.loaded, 1);
		assert_eq!(report.textures.missing, 0);
		assert_eq!(report.textures.file_bytes, custom_bytes);
		assert!(report.textures.top[0].id.contains("custom_sky.png"));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn texture_pack_counts_color_normal_and_mr_layers_with_recorded_etc2_formats() {
		let dir = std::env::temp_dir().join(format!("ror-memory-pack-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let cache_dir = dir.join("cache");
		std::fs::create_dir_all(&cache_dir).unwrap();

		let pack_xml = b"<texturepack_version>1</texturepack_version><usage>0</usage>";
		cache_write_key(&cache_dir, "rbxassetid://2001", pack_xml);
		let color_ktx = make_ktx2_bc(131, 64, 64, &[("transcodedWidth", "64"), ("transcodedHeight", "64")]);
		let normal_ktx = make_ktx2_bc(137, 64, 64, &[("transcodedWidth", "64"), ("transcodedHeight", "64"), ("colorSpace", "Linear")]);
		let mr_ktx = make_ktx2_bc(131, 64, 64, &[("transcodedWidth", "64"), ("transcodedHeight", "64")]);
		cache_write_key(&cache_dir, &format!("rbxassetid://2001#ktx2/7rdo/{}", Transcode::PackColor.fidelity(0)), &color_ktx);
		cache_write_key(&cache_dir, &format!("rbxassetid://2001#ktx2/7rdo/{}", Transcode::PackNormal.fidelity(0)), &normal_ktx);
		cache_write_key(&cache_dir, &format!("rbxassetid://2001#ktx2/7rdo/{}", Transcode::PackMetalnessRoughness.fidelity(0)), &mr_ktx);

		let mut model = ModelBuilder::new("Pack");
		let part = model.part(&PartSpec { name: "Block".into(), ..Default::default() });
		model.child(part, InstanceBuilder::new("SurfaceAppearance").with_property("TexturePack", ContentId::from("rbxassetid://2001")));
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&cache_dir) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert_eq!(report.textures.unique, 3, "color, normal, and mr layers must all be counted");
		assert_eq!(report.textures.loaded, 3);
		assert_eq!(report.textures.missing, 0);
		let color = report.textures.top.iter().find(|e| e.id == "rbxassetid://2001#color").expect("color layer");
		let normal = report.textures.top.iter().find(|e| e.id == "rbxassetid://2001#normal").expect("normal layer");
		let mr = report.textures.top.iter().find(|e| e.id == "rbxassetid://2001#mr").expect("mr layer");
		assert_eq!((color.desktop_bc_format, color.mobile_etc2_format), ("bc1", "etc2_rgb8"));
		assert_eq!((normal.desktop_bc_format, normal.mobile_etc2_format), ("bc3", "etc2_rgba8"));
		assert_eq!((mr.desktop_bc_format, mr.mobile_etc2_format), ("bc1", "etc2_rgb8"));
		assert_eq!(color.mobile_etc2_mip_bytes, 2_744);
		assert_eq!(normal.mobile_etc2_mip_bytes, 5_488);
		assert_eq!(mr.mobile_etc2_mip_bytes, 2_744);
		assert_eq!(report.textures.desktop_bc_mip_bytes, 2_744 + 5_488 + 2_744);
		assert_eq!(report.textures.mobile_etc2_mip_bytes, 2_744 + 5_488 + 2_744);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn ktx2_header_parser_reads_cached_packs_without_zstd_decompression() {
		let dir = std::env::temp_dir().join(format!("ror-memory-ktx2-fast-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let cache_dir = dir.join("cache");
		std::fs::create_dir_all(&cache_dir).unwrap();

		// Build a KTX2 pack 0 declaring supercompressionScheme=2 (zstd) with intentionally bogus
		// level payload bytes so `ror_image::decode` would fail if called, proving memory reads
		// header + KVD only without decompressing payloads.
		let mut pack0 = make_ktx2_bc(137, 32, 32, &[("transcodedWidth", "64"), ("transcodedHeight", "64"), ("avgAlpha", "180"), ("pack_0", "1,6"), ("pack_1", "0,1"), ("packIndex", "0")]);
		pack0[44..48].copy_from_slice(&2u32.to_le_bytes()); // supercompressionScheme = Zstd
		assert!(ror_image::decode(&pack0).is_err(), "bogus zstd payload must fail full decode");
		assert_eq!(ktx2_pack_count_fast(&pack0), 2);

		let pack1 = vec![0xAAu8; 321];
		cache_write_key(&cache_dir, &format!("rbxassetid://3001#ktx2/7rdo/{}", Transcode::Image.fidelity(0)), &pack0);
		cache_write_key(&cache_dir, &format!("rbxassetid://3001#ktx2/7rdo/{}", Transcode::Image.fidelity(1)), &pack1);

		let mut model = ModelBuilder::new("FastKtx2");
		let part = model.part(&PartSpec { name: "Block".into(), ..Default::default() });
		model.child(part, InstanceBuilder::new("Decal").with_property("Texture", ContentId::from("rbxassetid://3001")).with_property("Face", Enum::from_u32(1)));
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&cache_dir) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert!(!report.partial);
		assert_eq!(report.textures.unique, 1);
		assert_eq!(report.textures.loaded, 1);
		assert_eq!(report.textures.missing, 0);
		assert_eq!(report.textures.file_bytes, (pack0.len() + pack1.len()) as u64);
		let entry = &report.textures.top[0];
		assert_eq!((entry.width, entry.height, entry.format, entry.has_alpha), (64, 64, "bc3", true));
		assert_eq!((entry.packs_loaded, entry.packs_expected), (2, 2));
		assert_eq!(entry.desktop_bc_mip_bytes, 5_488);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn pack_only_missing_marks_memory_report_partial() {
		let dir = std::env::temp_dir().join(format!("ror-memory-pack-only-partial-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let cache_dir = dir.join("cache");
		std::fs::create_dir_all(&cache_dir).unwrap();

		// Zero missing meshes and zero missing textures: ONLY progressive KTX2 pack 1 is absent offline.
		let pack0 = make_ktx2_bc(131, 32, 32, &[("transcodedWidth", "64"), ("transcodedHeight", "64"), ("pack_0", "1,6"), ("pack_1", "0,1"), ("packIndex", "0")]);
		cache_write_key(&cache_dir, &format!("rbxassetid://4001#ktx2/7rdo/{}", Transcode::Image.fidelity(0)), &pack0);

		let mut model = ModelBuilder::new("PackOnlyPartial");
		let part = model.part(&PartSpec { name: "Block".into(), ..Default::default() });
		model.child(part, InstanceBuilder::new("Decal").with_property("Texture", ContentId::from("rbxassetid://4001")).with_property("Face", Enum::from_u32(1)));
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let assets = Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&cache_dir) });
		let meshes = Meshes::load(&scene.parts, &assets);
		let report = measure(&scene, &meshes, &assets);

		assert_eq!(report.meshes.missing, 0);
		assert_eq!(report.textures.missing, 0);
		assert_eq!(report.missing.assets, 0);
		assert_eq!(report.textures.missing_packs, 1);
		assert_eq!(report.missing.transcode_packs, 1);
		assert!(report.textures.file_bytes_partial, "file_bytes_partial must be true when a progressive pack is missing");
		assert!(report.partial, "pack-only missing must mark MemoryReport.partial = true even with 0 missing meshes/textures");
		assert!(report.total_gpu_bytes.partial, "pack-only missing must mark total_gpu_bytes.partial = true");
		assert!(
			report.missing.uncounted_bytes.as_deref().is_some_and(|s| s.contains("1 missing progressive")),
			"uncounted_bytes must explain missing progressive pack: {:?}",
			report.missing.uncounted_bytes
		);
		let line = summary_line(&report, false);
		assert!(line.contains("PARTIAL upper-bound estimate") && line.contains("PARTIAL total est GPU upper bound"), "{line}");
		let _ = std::fs::remove_dir_all(&dir);
	}
}
