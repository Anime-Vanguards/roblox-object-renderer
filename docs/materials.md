# Materials and shading rules

This page documents how `ror` reproduces Roblox materials, `SurfaceAppearance`, vertex colors, special materials (`Neon`, `Glass`, `ForceField`), decals, characters, and `Highlight`s.

Implementation: `crates/ror/src/render/materials.rs`, `render/texels.rs`, and `shaders/mesh.wgsl`. For general lighting, tone mapping, and the color pipeline, see [rendering.md](rendering.md).

## Built-in materials

- **Material generations:** `ror` supports both 2022 and pre-2022 materials, controlled by `MaterialService.Use2022Materials` (load a place's setting and `MaterialVariant`s with `--materials-from <place>`). A place file without a `MaterialService` draws the pre-2022 materials, as Roblox loads such a file, and `ror` says so in its warnings; a model, a script's scene and a place whose service has `Use2022Materials` on draw the 2022 ones. Pre-2022 packs are listed in `render::materials::PACKS_PRE2022`.
  - *Verification:* Measured in a Roblox 0.747 client with `fixtures/materials-generation.luau` (floors under a `PointLight`, slabs under the sun and the sky, large slabs at several distances, other shapes), in places written with and without the service.
- **Texture packs:** Built-in materials, `SurfaceAppearance.TexturePack`, and `MaterialVariant.TexturePack` are defined by XML texture pack assets containing `texturepack_version`, `usage`, `alphamode`, `tiling`, and optional asset IDs for `color`, `normal`, `metalness`, `roughness`, and `emissive`. Built-in 2022 packs are listed in `render::materials::PACKS_2022`.
- **KTX2 transcodes:** Like Studio, `ror` renders Roblox's KTX2 transcodes (BC1/BC3 with Roblox mips) rather than raw uploaded PNGs (`ROR_TRANSCODES=0` forces raw uploads).
- **Textureless materials:** `SmoothPlastic`, `Neon`, and `ForceField` use no texture maps; `Plastic` and `Foil` use only a normal map.
- **Tiling:** Per-material `StudsPerTile`, Organic per-part offsets, and Wang tiling parameters are measured from Studio and stored alongside each pack table.

## SurfaceAppearance and MaterialVariant

- **`SurfaceAppearance` override:** When present, `ColorMap`, `NormalMap`, `MetalnessMap`, and `RoughnessMap` completely override the part's base `Material` and `TextureID`. `AlphaMode` (`Overlay`, `Transparency`, `TintMask`) determines how `ColorMap` alpha is interpreted.
- **`AlphaMode = TintMask`:** Blends the part `Color` in stored (gamma-encoded) space before decoding: `texel * (1 - a + a * Color)`. At `a = 0`, the texel is untouched; at `a = 1`, it multiplies by `Color`.
  - Measured on 8-step alpha ramps (`tintmask-ramp`): averages within 2.1/255 of Studio per cell (vs. 3.4/255 for gamma-2 mixing and 6.8/255 for linear mixing). Built-in materials using TintMask mix in gamma 2 instead.
  - **Emission:** TintMask surfaces emit using the tinted color rather than the raw texel, scaled by the emissive mask red channel, `EmissiveTint`, and `EmissiveStrength` (`tintmask-emission`). Recoloring a part shifts its glow accordingly.
- **`MaterialVariant`:** Loaded from `MaterialService` via the part's `MaterialVariant` property or `MaterialService` base-material overrides, bringing its own maps, `StudsPerTile`, pattern, and emissive mask.
- **Metallic diffuse conservation:** When `EnvironmentSpecularScale` (`s`) is enabled, diffuse sky light scales by `(1 - metalness)(1 - k * s * F)` so diffuse + specular on a white cube matches diffuse alone.

## MeshPart color and vertex colors

How `Part.Color`, mesh vertex colors (`FileMesh` or `EditableMesh` `AddColor`/`SetFaceColors`), textures, and `SurfaceAppearance` combine:

| Setup | Resulting albedo |
|---|---|
| Untextured `MeshPart` | `Color² * v²` per channel (vertex colors multiply `Color` before gamma-2 decode) |
| `TextureID` or `TextureContent` | `texel² * 0.971 * v²` (`0.971` is Roblox's color-texture attenuation). `Part.Color` shows nowhere, under any texture alpha; a white texture reveals vertex colors directly. |
| `SurfaceAppearance` (`Overlay`) | Vertex colors multiply `Part.Color` in areas revealed by `ColorMap` alpha (or everywhere if `ColorMap` is empty), never the `ColorMap` itself. |
| `SurfaceAppearance` (`Transparency` or `TintMask`) | Vertex colors have no effect. |

*Verification:* Measured in a Roblox 0.747 client using `fixtures/vertex-colour-maps.luau` under flat ambient light across white and `(204, 153, 102)` `EditableImage`s, a white built-in `TextureID`, two part colors, and `Transparency` 0 / 0.5. `ror` matches within 1/255 on textured/corner-colored quads and within 4/255 on `Overlay` at alpha 0.5.

### Texture alpha on MeshParts and SpecialMeshes

A `MeshPart`'s `TextureID` / `TextureContent` and a `SpecialMesh`'s `TextureId` replace the part's color. Their alpha is not coverage up to `Transparency` 0.01: the part draws opaque, showing the texture's stored color under any alpha (alpha 0 included), and casts its whole shadow. From `Transparency` 0.02 the part's opacity is `(1 - Transparency) * alpha`, times a `MeshPart`'s vertex alpha, and its shadow is cut where that is 0.252 or less, as a `SurfaceAppearance` in `AlphaMode` Transparency cuts it ([rendering.md](rendering.md)). Image files, `EditableImage`s and uploaded images behave alike. A `SpecialMesh`'s `FileMesh` vertex colors and vertex alpha do nothing, textured or not.

*Verification:* Measured in a Roblox 0.747 client (quality level 21) with `fixtures/texture-alpha.luau`: `(204, 153, 102)` images at alpha 0, 64, 128, 192 and 255 as files, `EditableImage`s and an uploaded image on red quads at `Transparency` 0, 0.01, 0.02 and 0.5 before a green wall, under flat light, the sun and shadowed point lights, in two sessions that agree exactly. `ror` matches within 1/255 on every colour reading and on every shadow.

## Neon

- Unlit by sun or ambient light, but still affected by `Lighting.ExposureCompensation`.
- Emissive radiance is `25.75 * c⁴` per channel (`c` in `0..1`), clamped at `7.84` before blending like all surfaces. Saturated colors shift toward white/yellow as channels clip (e.g., bright orange looks yellow).
- Feeds into the bloom pass ([rendering.md](rendering.md#post-effects-and-bloom)).

## Glass

- **Refraction:** Samples only the opaque scene buffer, offset by `P - k * cos^p(θ) * n⊥` (where `n⊥` is the view-plane component of the shading normal and `θ` is the angle to the view axis; `k` decreases as `Transparency` rises). Transparent parts, particles, and beams behind Glass do not refract through it.
- **Surface:** Composites tint, Fresnel sky reflection, and sun specular glint over the refracted background.
- **Transparency > 1:** Draws nothing unless inside a `Highlight` (even a disabled one), where Studio still renders faint glass rim rings.

## ForceField

- **Material:** Unlit procedural pattern animated over time (`shaders/mesh.wgsl`, `forcefield-*` experiments), strongest at grazing angles and scaled by `Color` and `Transparency`.
- **Shadows:** Cast by `Transparency` as for any other material: full below 0.748, none from it up ([rendering.md](rendering.md#lighting-and-shadows)).
- **`ForceField` instance:** On a character, draws Studio's legacy shield particles instead ([particles.md](particles.md)).

## Decals and Textures

- Rendered on their assigned face using the host part's lighting, tinted by `Color3` and `Transparency`, and ordered per face by `ZIndex`. `Texture` tiles via `StudsPerTileU` / `StudsPerTileV` and offsets.
- **`Decal.UVOffset` and `UVScale`:** Map face UVs as `uv * UVScale + UVOffset`, clipped to the face bounds (e.g., `UVScale = (2, 2)` confines the image to the top-left quarter; `UVOffset = (0.25, 0)` shifts it left by a quarter).
- On `MeshPart`s and `SpecialMesh`es, decals follow per-mesh projection rules implemented in `fs_decal` (`shaders/mesh.wgsl`) and `render/mod.rs`.

## Characters and skinned meshes

- R6 and R15 characters load base rigs from `rbxasset://avatar/...`, apply `CharacterMesh`es, and composite `BodyColors` beneath `Shirt`, `Pants`, and `ShirtGraphic` on classic clothing templates (`render::composite`, `scene::character`).
- An R15 limb group (the torso, an arm, a leg) draws as one texture: the first part from the upper one down with a `TextureID` gives every part of the group its colour and that texture, drawn over the clothing at the composite's resolution; without one, the upper part's colour. A part with a `SurfaceAppearance` shows it alone, without clothing.
- Skinned meshes are posed by their `Bone` hierarchy on the CPU before drawing with their assigned materials. A part's Bones are its `Bone` children and theirs (not those under a nested part); among joined parts a `WeldConstraint`'s Bones win over a `Weld`'s and a `Weld`'s over a `Motor6D`'s. A vertex without weights lands on the world origin, as in a Roblox client.

## Highlights

Renders fill and outline passes over the `Adornee` (or parent) hierarchy respecting `DepthMode` (`AlwaysOnTop` or `Occluded`), `FillTransparency`, and `OutlineTransparency` (`render::highlights`).

- The outline is a two-pixel band across the silhouette's edge. It holds one layer of the fill and the outline together, as opaque as the more opaque of the two, their colors mixed in linear light by the outline's share of that opacity, as a Roblox client draws it: a nearly transparent outline over an opaque fill shows mostly the fill (a white outline at `OutlineTransparency` 0.9 over a red fill reads (255, 81, 81)), and with `OutlineTransparency` 1 the fill covers the band.
