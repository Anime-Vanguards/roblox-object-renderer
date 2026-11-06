//! Optional RenderDoc captures of views (`ROR_RENDERDOC=<path to renderdoc.dll>`): its in-application API
//! loaded before the GPU device, each view's one submission a capture. No window presents, so RenderDoc's
//! own frame keys never fire; `ROR_RENDERDOC_VIEWS=<n>` or `<first>-<last>` picks views by their order in
//! the process (from 1; default the first), `ROR_RENDERDOC_OUT` the file template. On Vulkan the layer is
//! named explicitly by its manifest beside the DLL, so it needs no registration.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

struct Api {
	table: *const *const c_void,
	views: (u32, u32),
}

// SAFETY: RenderDoc's function table is immutable and its functions are thread-safe
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Option<Api>> = OnceLock::new();
static VIEWS: AtomicU32 = AtomicU32::new(0);

type StartEnd = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;
type SetTemplate = unsafe extern "C" fn(*const std::ffi::c_char);

// RENDERDOC_API_1_6_0's members, by index (renderdoc_app.h)
const SET_CAPTURE_FILE_PATH_TEMPLATE: usize = 11;
const START_FRAME_CAPTURE: usize = 19;
const END_FRAME_CAPTURE: usize = 21;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
	fn LoadLibraryW(name: *const u16) -> *mut c_void;
	fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// Loads RenderDoc when asked, before the instance and device are made.
pub(crate) fn load() {
	API.get_or_init(|| {
		let path = std::env::var_os("ROR_RENDERDOC")?;
		let api = open(std::path::Path::new(&path));
		if api.is_none() {
			eprintln!("ror: ROR_RENDERDOC: could not load RenderDoc from {}", std::path::Path::new(&path).display());
		}
		api
	});
}

#[cfg(windows)]
fn open(path: &std::path::Path) -> Option<Api> {
	use std::os::windows::ffi::OsStrExt;
	if let Some(dir) = path.parent() {
		// SAFETY: before any thread reads the environment (the device is not made yet)
		unsafe {
			std::env::set_var("VK_ADD_LAYER_PATH", dir);
			std::env::set_var("VK_INSTANCE_LAYERS", "VK_LAYER_RENDERDOC_Capture");
			std::env::set_var("ENABLE_VULKAN_RENDERDOC_CAPTURE", "1");
		}
	}
	let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
	// SAFETY: a plain library load and symbol lookup; RENDERDOC_GetAPI's signature is renderdoc_app.h's
	unsafe {
		let module = LoadLibraryW(wide.as_ptr());
		if module.is_null() {
			return None;
		}
		let get_api = GetProcAddress(module, c"RENDERDOC_GetAPI".as_ptr().cast());
		if get_api.is_null() {
			return None;
		}
		let get_api: unsafe extern "C" fn(u32, *mut *mut c_void) -> i32 = std::mem::transmute(get_api);
		let mut table: *mut c_void = std::ptr::null_mut();
		if get_api(10600, &mut table) != 1 || table.is_null() {
			return None;
		}
		let table = table as *const *const c_void;
		if let Ok(template) = std::env::var("ROR_RENDERDOC_OUT") {
			let set: SetTemplate = std::mem::transmute(*table.add(SET_CAPTURE_FILE_PATH_TEMPLATE));
			let template = std::ffi::CString::new(template).ok()?;
			set(template.as_ptr());
		}
		let views = std::env::var("ROR_RENDERDOC_VIEWS").ok().and_then(|text| {
			let (first, last) = text.split_once('-').unwrap_or((&text, &text));
			Some((first.trim().parse().ok()?, last.trim().parse().ok()?))
		});
		Some(Api { table, views: views.unwrap_or((1, 1)) })
	}
}

#[cfg(not(windows))]
fn open(_: &std::path::Path) -> Option<Api> {
	None
}

/// Starts a capture when this view is one asked for; the returned guard ends it.
pub(crate) fn view() -> Option<Capture> {
	let api = API.get()?.as_ref()?;
	let index = VIEWS.fetch_add(1, Ordering::Relaxed) + 1;
	if index < api.views.0 || index > api.views.1 {
		return None;
	}
	// SAFETY: the table's StartFrameCapture, the default device and no window
	unsafe {
		let start: StartEnd = std::mem::transmute(*api.table.add(START_FRAME_CAPTURE));
		start(std::ptr::null_mut(), std::ptr::null_mut());
	}
	Some(Capture { api })
}

pub(crate) struct Capture {
	api: &'static Api,
}

impl Capture {
	/// Ends the capture once the view is submitted.
	pub fn end(self) {
		// SAFETY: as in `view`
		let ok = unsafe {
			let end: StartEnd = std::mem::transmute(*self.api.table.add(END_FRAME_CAPTURE));
			end(std::ptr::null_mut(), std::ptr::null_mut())
		};
		eprintln!("ror: RenderDoc capture {}", if ok == 1 { "written" } else { "failed" });
	}
}
