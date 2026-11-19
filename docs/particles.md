# Particles, beams, and trails

`ror` renders Roblox `ParticleEmitter`, `Beam`, `Trail`, and legacy effect instances (`Fire`, `Smoke`, `Sparkles`, `Explosion`, and `ForceField` character shields).

Implementation: `crates/ror/src/render/particles.rs`, `render/beams.rs`, `shaders/particle.wgsl`, `beam.wgsl`, and `legacy.wgsl`. Timeline simulation lives in `crates/ror/src/scene/playback.rs` and `scene/particles.rs`.

## Static renders (no playback)

Without a playback timeline, a model renders as authored:
- Enabled `ParticleEmitter`s render in steady state (a deterministically seeded snapshot of `Rate * Lifetime` particles distributed across ages).
- `Beam`s render at rest.
- `Trail`s draw nothing, since trails require attachment movement.
- Legacy effects (`Fire`, `Smoke`, `Sparkles`, `ForceField` shields, and `Explosion`) render as particle bursts using Roblox Studio's built-in particle textures.

## Timed renders (with playback)

With a playback timeline ([playback.md](playback.md)), `ror` simulates emissions, property switches, tweens, and movement up to the requested time:
- Particles exist at the exact positions, sizes, colors, transparencies, rotations, and flipbook frames produced by their spawn time and age.
- Trails follow the attachment path left over prior frames.
- Beams scroll their texture at `TextureSpeed` starting from the frame they were enabled.

## Behavior and extension rules

- **Deterministic seeding:** Each emitter maintains independent random streams seeded by its instance path and copy index. Rerenders are identical, while duplicate emitters remain visually distinct like in Studio.
- **Depth sorting:** Particles sort back-to-front within each emitter. Emitters, beams, and transparent parts sort relative to each other by part/attachment positions, with `ZOffset` shifting particles along view depth.
- **Off-screen freezing:** Emitters outside the camera frustum or beyond draw range pause—neither emitting nor aging—matching Studio. In video renders, the active camera track controls which emitters advance.
- **Local lighting:** Particles, beams, and trails sample local lights from a voxel light field rather than per-pixel lighting, matching Studio.
- **Beam and trail geometry:** Beams tessellate into `Segments` quads between attachments bent by `CurveSize0`/`CurveSize1` (oriented via `FaceCamera`). Trails record one point per frame within their `Lifetime` and `MinLength`.

These rules are verified by the `particle-*`, `beam-*`, `trail-*`, and `legacy-*` calibration experiments in `ror studio calibrate` ([parity.md](parity.md)).
