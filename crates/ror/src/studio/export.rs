//! Exporting instances from any open Studio as a binary model, read-only: the instance is
//! serialized with `SerializationService` (nothing in the place changes) and paged out over MCP.

use anyhow::{Context, Result, bail};

use super::mcp::Mcp;

/// Luau that resolves a dotted path (`ReplicatedStorage.Models.Character`, `game.Workspace["A.B"]`) from
/// `game`, or errors naming the children it could not find.
fn resolve_script(path: &str) -> Result<String> {
	let mut segments = Vec::new();
	let mut rest = path.trim();
	while !rest.is_empty() {
		if let Some(after) = rest.strip_prefix('[') {
			let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'').context("expected a quoted name after [")?;
			let end = after[1..].find(quote).context("unterminated quoted name")? + 1;
			segments.push(after[1..end].to_string());
			rest = after[end + 1..].strip_prefix(']').context("expected ]")?;
		} else {
			let end = rest.find(['.', '[']).unwrap_or(rest.len());
			if end > 0 {
				segments.push(rest[..end].to_string());
			}
			rest = &rest[end..];
		}
		rest = rest.strip_prefix('.').unwrap_or(rest);
	}
	if segments.first().is_some_and(|first| first == "game") {
		segments.remove(0);
	}
	if segments.is_empty() {
		bail!("empty instance path");
	}
	let list = segments.iter().map(|segment| format!("{segment:?}")).collect::<Vec<_>>().join(", ");
	Ok(format!(
		r#"local current = game
for index, name in {{{list}}} do
	local child = if index == 1 then (game:FindFirstChild(name) or select(2, pcall(game.GetService, game, name))) else current:FindFirstChild(name)
	if typeof(child) ~= "Instance" then
		local names = {{}}
		for _, c in current:GetChildren() do
			if #names < 25 then table.insert(names, c.Name) end
		end
		error("no " .. name .. " under " .. current:GetFullName() .. " (children: " .. table.concat(names, ", ") .. ")")
	end
	current = child
end"#
	))
}

/// Serializes the instance at `path` in the Studio whose name contains `studio` and returns the
/// binary model (`.rbxm`) bytes. With `materials`, the place's MaterialVariants come along as extra
/// top-level instances (ror applies them to the model's parts; scripts cannot read
/// `MaterialService.Use2022Materials`, so the 2022 materials are assumed).
pub fn export(studio: &str, path: &str, materials: bool) -> Result<Vec<u8>> {
	let mut mcp = Mcp::connect()?;
	let target = mcp.find_studio(studio)?;
	let variants = if materials {
		"for _, variant in game:GetService(\"MaterialService\"):GetDescendants() do if variant:IsA(\"MaterialVariant\") then table.insert(list, variant) end end"
	} else {
		""
	};
	let code = format!(
		"{}\nlocal list = {{current}}\n{variants}\nlocal data = game:GetService(\"SerializationService\"):SerializeInstancesAsync(list)\nreturn buffer.tostring(data)",
		resolve_script(path)?
	);
	let bytes = mcp.run_luau_bytes(&target.id, &code).with_context(|| format!("exporting {path} from {}", target.name.unwrap_or_default()))?;
	if !bytes.starts_with(b"<roblox!") {
		bail!("Studio did not return a binary model ({} bytes)", bytes.len());
	}
	Ok(bytes)
}
