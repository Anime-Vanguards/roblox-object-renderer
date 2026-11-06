//! ror's tools in one binary (one crate to check, one link): `cargo run -p ror --release --example tools -- <tool> [arguments]`.

use std::process::{Command, ExitCode, Termination};

// Each tool is a module written like a standalone example: `pub fn main`, its arguments from std::env::args()
macro_rules! tools {
	($($name:ident),+ $(,)?) => {
		$(mod $name;)+

		const TOOLS: &[(&str, fn() -> ExitCode)] = &[$((stringify!($name), || $name::main().report())),+];
	};
}

tools!(
	bench_scene,
	bones_scan,
	capture_place,
	characters_scan,
	class_counts,
	class_defaults,
	composite_atlas,
	dds_info,
	decode_image,
	dump_scripts,
	exposure_groups,
	extract_model,
	fetch_asset,
	fetch_image,
	filter_model,
	gpu_start,
	grep_model,
	gui_scan,
	image_levels,
	instance_props,
	light_census,
	lighting_copy,
	lighting_dump,
	lighting_variant,
	mesh_bounds,
	mesh_colors,
	mesh_dump,
	mesh_info,
	pack_layers,
	part_surfaces,
	particle_solo,
	place_tree,
	reflect,
	rerender,
	resize_capture,
	scene_dump,
	scene_parts,
	scene_reuse,
	sky_cube_mips,
	still_beams,
	strip_class,
	strip_classes,
	texture_compare,
	texture_pack,
	tile_models,
	transcode_thumbs,
	union_data,
	unions_scan,
	variant_parts,
	vertex_color_scan,
	vfx_big,
	vfx_census,
	vfx_dump,
	vfx_fuzz,
	vfx_groups,
	vfx_order,
	vfx_solo,
	vram_probe,
	warm_check,
	wrap_place,
);

// Names the tool when this binary starts itself again to run it, so the tool's std::env::args() are only its own
const TOOL: &str = "ROR_TOOLS_RUN";

fn main() -> ExitCode {
	if let Some(wanted) = std::env::var_os(TOOL) {
		// SAFETY: no other thread runs yet
		unsafe { std::env::remove_var(TOOL) };
		return match TOOLS.iter().find(|(name, _)| wanted == **name) {
			Some((_, tool)) => tool(),
			None => {
				eprintln!("ror tools: no tool {wanted:?}");
				ExitCode::FAILURE
			}
		};
	}
	let mut args = std::env::args_os().skip(1);
	let wanted = args.next();
	match wanted.as_deref().and_then(|wanted| TOOLS.iter().find(|(name, _)| *wanted == **name)) {
		Some((name, _)) => match std::env::current_exe() {
			Ok(exe) => run(Command::new(exe).args(args).env(TOOL, name)),
			Err(error) => {
				eprintln!("ror tools: {error}");
				ExitCode::FAILURE
			}
		},
		None if wanted.as_deref().is_none_or(|wanted| wanted == "--help" || wanted == "-h") => {
			println!("{}", usage());
			ExitCode::SUCCESS
		}
		None => {
			eprintln!("ror tools: no tool {:?}\n{}", wanted.unwrap_or_default(), usage());
			ExitCode::from(2)
		}
	}
}

fn usage() -> String {
	let names: Vec<&str> = TOOLS.iter().map(|(name, _)| *name).collect();
	format!("usage: cargo run -p ror --release --example tools -- <tool> [arguments]\ntools: {}", names.join(" "))
}

#[cfg(not(windows))]
fn run(command: &mut Command) -> ExitCode {
	use std::os::unix::process::CommandExt;
	eprintln!("ror tools: {}", command.exec());
	ExitCode::FAILURE
}

// Windows has no exec: the tool runs as a child that handles Ctrl+C itself (as under cargo run) and ends with this process
#[cfg(windows)]
fn run(command: &mut Command) -> ExitCode {
	// SAFETY: a handler that only returns
	unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(leave_to_tool), 1) };
	match command.spawn().and_then(|mut child| {
		end_with_this_process(&child);
		child.wait()
	}) {
		Ok(status) => std::process::exit(status.code().unwrap_or(1)),
		Err(error) => {
			eprintln!("ror tools: {error}");
			ExitCode::FAILURE
		}
	}
}

#[cfg(windows)]
unsafe extern "system" fn leave_to_tool(_: u32) -> i32 {
	1
}

// A kill-on-close job that only this process holds; what the tool starts leaves it, as from Node's job around a process
#[cfg(windows)]
fn end_with_this_process(child: &std::process::Child) {
	use std::os::windows::io::AsRawHandle;

	use windows_sys::Win32::Foundation::CloseHandle;
	use windows_sys::Win32::System::JobObjects::{
		AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
		JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
	};
	// SAFETY: a job object made, set and given the child here; its handle stays open for this process's life
	unsafe {
		let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
		if job.is_null() {
			return;
		}
		let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
		info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
		let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
		if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, size) == 0
			|| AssignProcessToJobObject(job, child.as_raw_handle()) == 0
		{
			CloseHandle(job);
		}
	}
}
