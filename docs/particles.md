# Particles, beams, and trails

`ror` renders Roblox `ParticleEmitter`, `Beam`, `Trail`, and legacy effect instances (`Fire`, `Smoke`, `Sparkles`, `Explosion`, and `ForceField` character shields).

Implementation: `crates/ror/src/render/particles.rs`, `render/beams.rs`, `shaders/particle.wgsl`, `beam.wgsl`, and `legacy.wgsl`. Timeline simulation lives in `crates/ror/src/scene/playback.rs` and `scene/particles.rs`.

## Static renders (no playback)

Without a playback timeline, a model renders as authored:
- Enabled `ParticleEmitter`s render in steady state (a deterministically seeded snapshot of `Rate * Lifetime` particles distributed across ages).
- `Beam`s render at rest.
- `Trail`s draw nothing, since trails require attachment movement.
- Legacy effects (`Fire`, `Smoke`, `Sparkles`, `ForceField` shields, and `Explosion`) render as particle bursts using Roblox Studio's built-in particle textures. As in a Roblox client, each `Sparkles` star carries a small white core that does not take `SparkleColor` (black `Sparkles` show faint white stars).

## Timed renders (with playback)

With a playback timeline ([playback.md](playback.md)), `ror` simulates emissions, property switches, tweens, and movement up to the requested time:
- Particles exist at the exact positions, sizes, colors, transparencies, rotations, and flipbook frames produced by their spawn time and age. A particle shows while its age is under its `Lifetime` (an age that reaches it is gone); unblended `OneShot`, `Loop`, and `PingPong` flipbooks show the frame before at an exact whole frame.
- Trails follow the attachment path left over prior frames.
- Beams scroll their texture at `TextureSpeed` starting from the frame they were enabled.

## Behavior and extension rules

- **Deterministic seeding:** Each emitter maintains independent random streams seeded by its instance path and copy index. Rerenders are identical, while duplicate emitters remain visually distinct like in Studio.
- **Depth sorting:** Particles sort back-to-front within each emitter. Emitters, beams, trails (by their head), transparent parts, and legacy effects such as `Fire` sort relative to each other by the depth of their part/attachment positions along the camera's forward axis (not their distance from the eye: turning the camera can change which of two draws is in front, moving it sideways cannot), with `ZOffset` shifting particles along view depth. A `Fire` behind a half-transparent pane shows through it; one in front draws over it.
- **ZOffset:** A particle keeps its place and size on screen. A positive `ZOffset` moves only the depth of each corner towards the camera, and what that brings to the near plane (0.1 studs) or past it is clipped corner by corner: a square facing the camera shows whole or not at all, a flat or tilted one can show only its far part. A negative one moves the particle away along its view ray.
- **Occlusion culling:** An emitter or beam with a positive `ZOffset` whose box (where it stands, before the offset) is wholly hidden behind opaque parts is not drawn, as Roblox's occlusion culling drops it before the offset would bring it in front.
- **Off-screen freezing:** Emitters outside the camera frustum or beyond draw range pause—neither emitting nor aging—matching Studio. In video renders, the active camera track controls which emitters advance.
- **Local lighting:** Particles, beams, and trails sample local lights from a voxel light field rather than per-pixel lighting, matching Studio.
- **Beam and trail geometry:** Beams tessellate into `Segments` quads between attachments bent by `CurveSize0`/`CurveSize1` (oriented via `FaceCamera`). Trails record one point per frame within their `Lifetime` and `MinLength`: with `MinLength` set, the newest point follows the attachments, keeping the time it was made, until it is `MinLength` from the point before, a spacing that grows with the distance from the camera beyond 200 studs.

These rules are verified by the `particle-*`, `beam-*`, `trail-*`, and `legacy-*` calibration experiments in `ror studio calibrate` ([parity.md](parity.md)).
