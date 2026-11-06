# Particles, beams and trails

Effects are drawn by `crates/ror/src/render/particles.rs`, `render/beams.rs` and the trail code, with
`shaders/particle.wgsl`, `beam.wgsl` and `legacy.wgsl`; what they do over time comes from
`crates/ror/src/scene/playback.rs` and `scene/particles.rs`.

## Without a playback

A model draws as authored: each enabled ParticleEmitter's steady state (a seeded snapshot of
particles of every age, Rate times Lifetime of them), beams at rest, and no trails (a trail needs
motion). The legacy effects (Fire, Smoke, Sparkles, the ForceField instance's shield and Explosion)
draw as the particles Studio draws them with, with Studio's own textures.

## With a playback

A playback says what is emitted, switched, set, tweened and moved when, and the moment drawn.
Particles are then where and when they were emitted, trails follow the points their attachments
left, and beams scroll by TextureSpeed from the frame they were enabled in. The playback JSON, the
command-line flags and the emit modes are in [playback.md](playback.md).

## Rules that matter when extending the code

- Each emitter draws from random streams of its own, seeded by its path and its copy, so a render
  is the same every time and copies of one emitter differ as in Studio.
- Particles sort back to front within an emitter; emitters, beams and transparent parts sort among
  each other as Studio sorts them (by their parts' and attachments' places, ZOffset moving a particle
  along the view's depth).
- An emitter the camera cannot draw (off screen, past its reach) stands still, neither emitting nor
  ageing, as Studio leaves it; a clip's camera track decides which.
- Particles, beams and trails take local light from a voxel field of the view's lights, not per
  pixel, as Studio's do.
- Beams are drawn as their Segments' quads between their attachments (CurveSize0/1 bend them),
  FaceCamera turning them; trails keep a point a frame within their Lifetime and MinLength.

The experiments that measured these rules are `particle-*`, `beam-*`, `trail-*` and `legacy-*` in
`ror studio calibrate` ([parity.md](parity.md)).
