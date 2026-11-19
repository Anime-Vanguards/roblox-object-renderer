# Running Luau scripts as input

Both `ror render` and `ror video` accept a `.luau` script directly. `ror` executes the script in [roblox-engine-emulator](https://github.com/Anime-Vanguards/roblox-engine-emulator) (`ree`) and renders whatever it places in `Workspace`, letting you iterate on procedural models or scripted effects offline before bringing the same script into Studio.

Implementation: `crates/ror/src/script.rs`.

## Example

```luau
-- Build.luau
return function(Context)
	local Model = Instance.new("Model")
	for Index = 0, (Context.Floors or 5) - 1 do
		local Part = Instance.new("Part")
		Part.Anchored = true
		Part.Size = Vector3.new(4 - Index * 0.6, 2, 4 - Index * 0.6)
		Part.Position = Vector3.new(0, 1 + Index * 2, 0)
		Part.Parent = Model
	end
	Model.Parent = workspace
end
```

```sh
ror render Build.luau --views front,iso --context '{"Floors":3}'
ror render Build.luau --time 1                   # After 1 s of tweens and task.wait()
ror render Build.luau --mount ReplicatedStorage/Shared=src/shared
ror video Build.luau --duration 2
```

## How script runs work

- **Harness format:** Write a `ModuleScript` returning `function(Context)` (or `{ Run = function(Context) ... end }`). `Context` receives the JSON fields passed via `--context`. The script may yield (`task.wait`, etc.).
- **Simulation scope:** Scripts run in play mode at 60 FPS with `TweenService`, `task`, `RunService` events, `:Emit()`, and `AnimationTrack`s active. Physics simulation, server scripts, and CSG (`UnionAsync`) are not emulated (see `ree`'s [What is not simulated](https://github.com/Anime-Vanguards/roblox-engine-emulator/blob/main/docs/cli.md#what-is-not-simulated)).
- **Timeline capture:** `ror render` records up to `--time` (default `0`, capturing what the script builds synchronously) and renders that timestamp. `ror video` records through the clip duration. The generated recording serves as the playback—passing `--playback` is rejected, while CLI overrides like `--emit`, `--set`, and `--tween` layer on top ([playback.md](playback.md)).
- **Mounting dependencies (`--mount`):** Pass `--mount Service/Name=<dir|file>` (repeatable) to mount Luau directories or model files for `require`, matching `ree --mount`. Script runs always start in a clean world without mounting unrelated default projects or Studio snapshots. `--offline` is forwarded to `ree`.
- **Outputs:** Standard render/video outputs go to `--out` (default `<state dir>/out/<script>`, or `<script>-video` for video), along with the raw recording (`scene.rbxm`, `playback.json`, `events.json`, and `script.json`) under `<out>/record/`. The CLI summary includes a `script` block with status, frame count, instance count, and errors (also printed as `ror: script error: ...`). If the script leaves `Workspace` empty, `ror` fails with an error explaining why.

## Locating `ree`

`ror` looks for a release build (`cargo build --release`) of `ree` in `ROR_REE`, falling back to `../roblox-engine-emulator` alongside the `ror` checkout.
