//! Lossless capture of a Studio viewport: PrintWindow with PW_RENDERFULLCONTENT on the viewport's
//! child window, in physical pixels (the process is made per-monitor DPI aware first). Also window
//! lookup and resizing (without activating the window).

#![cfg(windows)]

use anyhow::{Result, anyhow};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
	BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::WindowsAndMessaging::{
	EnumChildWindows, EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};

/// The capture Studio's window size in physical pixels: its viewport is then 1644 x 1116 physical
/// (1096 x 744 logical at 150 % scaling), whole logical pixels so captures do not blur.
pub const CAPTURE_WINDOW: (i32, i32) = (2401, 1502);

/// The capture Studio's viewport in physical pixels at that window size (offline replays and the
/// capture cache assume it).
pub const CAPTURE_VIEWPORT: (u32, u32) = (1644, 1116);

const PW_RENDERFULLCONTENT: u32 = 2;

fn dpi_aware() {
	// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
	unsafe {
		SetProcessDpiAwarenessContext(-4isize as _);
	}
}

fn text(hwnd: HWND) -> String {
	let mut buffer = [0u16; 512];
	let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
	String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

fn class(hwnd: HWND) -> String {
	let mut buffer = [0u16; 256];
	let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
	String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

fn rect(hwnd: HWND) -> RECT {
	let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
	unsafe {
		GetWindowRect(hwnd, &mut rect);
	}
	rect
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
	pub hwnd: isize,
	pub class: String,
	pub title: String,
	pub x: i32,
	pub y: i32,
	pub width: i32,
	pub height: i32,
	pub visible: bool,
	pub dpi: u32,
}

fn info(hwnd: HWND) -> WindowInfo {
	let r = rect(hwnd);
	WindowInfo {
		hwnd: hwnd as isize,
		class: class(hwnd),
		title: text(hwnd),
		x: r.left,
		y: r.top,
		width: r.right - r.left,
		height: r.bottom - r.top,
		visible: unsafe { IsWindowVisible(hwnd) } != 0,
		dpi: unsafe { GetDpiForWindow(hwnd) },
	}
}

unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> i32 {
	let list = unsafe { &mut *(list as *mut Vec<HWND>) };
	list.push(hwnd);
	1
}

/// Visible top-level Studio windows whose title contains `title`.
pub fn find_studio_windows(title: &str) -> Vec<WindowInfo> {
	dpi_aware();
	let mut windows: Vec<HWND> = Vec::new();
	unsafe {
		EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM);
	}
	windows
		.into_iter()
		.filter(|&hwnd| unsafe { IsWindowVisible(hwnd) } != 0)
		.map(info)
		.filter(|window| window.title.contains("Roblox Studio") && window.title.contains(title))
		.collect()
}

pub fn children(parent: isize) -> Vec<WindowInfo> {
	dpi_aware();
	let mut windows: Vec<HWND> = Vec::new();
	unsafe {
		EnumChildWindows(parent as HWND, Some(collect), &mut windows as *mut _ as LPARAM);
	}
	windows.into_iter().map(info).collect()
}

pub fn is_minimized(hwnd: isize) -> bool {
	unsafe { IsIconic(hwnd as HWND) != 0 }
}

/// The visible child window of `parent` whose physical size is `width` x `height` (±1 px); the last
/// match in enumeration order (the innermost) wins.
pub fn find_viewport(parent: isize, width: i32, height: i32) -> Option<WindowInfo> {
	children(parent).into_iter().filter(|child| child.visible && (child.width - width).abs() <= 1 && (child.height - height).abs() <= 1).last()
}

/// Resizes a top-level window (physical pixels) without activating or moving it.
pub fn resize(hwnd: isize, width: i32, height: i32) -> Result<()> {
	dpi_aware();
	// a maximized (or minimized) window keeps its state through SetWindowPos: restore it first
	unsafe {
		if IsZoomed(hwnd as HWND) != 0 || IsIconic(hwnd as HWND) != 0 {
			ShowWindow(hwnd as HWND, SW_RESTORE);
		}
	}
	let ok = unsafe { SetWindowPos(hwnd as HWND, std::ptr::null_mut(), 0, 0, width, height, SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE) };
	if ok == 0 {
		return Err(anyhow!("SetWindowPos failed"));
	}
	Ok(())
}

/// The id of the process that owns a window.
pub fn window_process(hwnd: isize) -> u32 {
	let mut pid = 0u32;
	unsafe {
		GetWindowThreadProcessId(hwnd as HWND, &mut pid);
	}
	pid
}

/// Ends a process and waits (up to 15 s) for it to exit. Only for the capture Studio, whose place
/// ror regenerates: nothing in it needs saving.
pub fn terminate(pid: u32) -> Result<()> {
	unsafe {
		let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
		if handle.is_null() {
			return Err(anyhow!("cannot open process {pid}"));
		}
		let ok = TerminateProcess(handle, 1);
		WaitForSingleObject(handle, 15_000);
		CloseHandle(handle);
		if ok == 0 {
			return Err(anyhow!("TerminateProcess({pid}) failed"));
		}
	}
	Ok(())
}

/// Captures a window's content (physical pixels) as RGBA8.
pub fn capture(hwnd: isize) -> Result<(u32, u32, Vec<u8>)> {
	dpi_aware();
	let hwnd = hwnd as HWND;
	let r = rect(hwnd);
	let (width, height) = (r.right - r.left, r.bottom - r.top);
	if width <= 0 || height <= 0 {
		return Err(anyhow!("window has no area"));
	}
	unsafe {
		let screen = GetDC(std::ptr::null_mut());
		let memory = CreateCompatibleDC(screen);
		let mut header: BITMAPINFO = std::mem::zeroed();
		header.bmiHeader = BITMAPINFOHEADER {
			biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
			biWidth: width,
			biHeight: -height,
			biPlanes: 1,
			biBitCount: 32,
			biCompression: BI_RGB,
			biSizeImage: 0,
			biXPelsPerMeter: 0,
			biYPelsPerMeter: 0,
			biClrUsed: 0,
			biClrImportant: 0,
		};
		let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
		let bitmap = CreateDIBSection(memory, &header, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
		if bitmap.is_null() || bits.is_null() {
			DeleteDC(memory);
			ReleaseDC(std::ptr::null_mut(), screen);
			return Err(anyhow!("CreateDIBSection failed"));
		}
		let previous = SelectObject(memory, bitmap);
		let ok = PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT);
		let count = (width * height * 4) as usize;
		let bgra = std::slice::from_raw_parts(bits as *const u8, count);
		let mut rgba = Vec::with_capacity(count);
		for pixel in bgra.chunks_exact(4) {
			rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
		}
		SelectObject(memory, previous);
		DeleteObject(bitmap);
		DeleteDC(memory);
		ReleaseDC(std::ptr::null_mut(), screen);
		if ok == 0 {
			return Err(anyhow!("PrintWindow failed"));
		}
		Ok((width as u32, height as u32, rgba))
	}
}
