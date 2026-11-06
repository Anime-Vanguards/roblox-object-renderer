//! x264 in the process (vendor/x264: the x264 ffmpeg 7.1 carries, built with its assembly, and
//! `ror_x264.c`, ror's entry points into it): the H.264 encoder an MP4 clip's frames go to straight from
//! their readback, where they once crossed a pipe into ffmpeg (docs/video.md). Set up as ffmpeg sets up
//! libx264 for `-f rawvideo -pix_fmt nv12 -colorspace bt709 -color_primaries bt709 -color_trc
//! iec61966-2-1 -color_range tv -chroma_sample_location center -c:v libx264 -preset P -crf C -threads T
//! -f mp4`: the same encoder with the same settings, so the same stream.

use std::ffi::{CStr, CString, c_char};
use std::ptr::NonNull;

use anyhow::{Result, anyhow, bail};

#[repr(C)]
struct Handle {
	_opaque: [u8; 0],
}

unsafe extern "C" {
	fn ror_x264_thread_priority(priority: i32);
	fn ror_x264_open(width: i32, height: i32, fps_num: i32, fps_den: i32, timebase_num: i32, timebase_den: i32, preset: *const c_char, crf: f32, threads: i32, error: *mut c_char, error_size: i32) -> *mut Handle;
	fn ror_x264_headers(handle: *mut Handle, sps: *mut *const u8, sps_size: *mut i32, pps: *mut *const u8, pps_size: *mut i32, sei: *mut *const u8, sei_size: *mut i32) -> i32;
	fn ror_x264_encode(handle: *mut Handle, luma: *const u8, luma_stride: i32, chroma: *const u8, chroma_stride: i32, pts: i64, out: *mut *const u8, out_pts: *mut i64, out_dts: *mut i64, keyframe: *mut i32) -> i32;
	fn ror_x264_delayed(handle: *mut Handle) -> i32;
	fn ror_x264_error(handle: *mut Handle) -> *const c_char;
	fn ror_x264_close(handle: *mut Handle);
}

/// What an encoder is opened with.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
	pub width: u32,
	pub height: u32,
	/// Frames a second, as a fraction (the time base is its inverse, as ffmpeg's encoder has it).
	pub fps: (u32, u32),
	pub preset: String,
	pub crf: f32,
	pub threads: u32,
}

/// The stream's parameter sets (without their lengths), and x264's version SEI as the first frame
/// carries it (with its 4-byte length).
pub struct Headers {
	pub sps: Vec<u8>,
	pub pps: Vec<u8>,
	pub sei: Vec<u8>,
}

/// A frame out of the encoder: its NAL units with their 4-byte lengths, its timestamps (in frames).
pub struct Packet<'a> {
	pub data: &'a [u8],
	pub pts: i64,
	pub dts: i64,
	pub keyframe: bool,
}

pub struct Encoder {
	handle: NonNull<Handle>,
	luma_size: usize,
	width: u32,
}

// The encoder is used from one thread at a time (x264's own threads are its business).
unsafe impl Send for Encoder {}

impl Encoder {
	pub fn open(settings: &Settings) -> Result<Encoder> {
		let Settings { width, height, fps: (fps_num, fps_den), ref preset, crf, threads } = *settings;
		if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || fps_num == 0 || fps_den == 0 {
			bail!("x264: {width}x{height} at {fps_num}/{fps_den} fps (even sizes and a rate above 0)");
		}
		let preset = CString::new(preset.as_str())?;
		let mut error = [0 as c_char; 512];
		// below the draws, as ffmpeg's process was: eight clips at once otherwise took the cores the
		// draws' threads needed (video.rs `below_normal`)
		#[cfg(windows)]
		unsafe {
			ror_x264_thread_priority(windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL);
		}
		// SAFETY: plain values and buffers that outlive the call
		let handle = unsafe { ror_x264_open(width as i32, height as i32, fps_num as i32, fps_den as i32, fps_den as i32, fps_num as i32, preset.as_ptr(), crf, threads as i32, error.as_mut_ptr(), error.len() as i32) };
		match NonNull::new(handle) {
			Some(handle) => Ok(Encoder { handle, luma_size: width as usize * height as usize, width }),
			None => Err(anyhow!("x264: {}", unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy())),
		}
	}

	pub fn headers(&mut self) -> Result<Headers> {
		let (mut sps, mut pps, mut sei) = (std::ptr::null(), std::ptr::null(), std::ptr::null());
		let (mut sps_size, mut pps_size, mut sei_size) = (0, 0, 0);
		// SAFETY: x264's buffers, copied before the next call into the encoder
		unsafe {
			if ror_x264_headers(self.handle.as_ptr(), &mut sps, &mut sps_size, &mut pps, &mut pps_size, &mut sei, &mut sei_size) < 0 || sps.is_null() || pps.is_null() || sps_size <= 4 || pps_size <= 4 {
				bail!("x264 gave no headers: {}", self.error());
			}
			let copy = |data: *const u8, size: i32| std::slice::from_raw_parts(data, size as usize).to_vec();
			Ok(Headers { sps: copy(sps, sps_size)[4..].to_vec(), pps: copy(pps, pps_size)[4..].to_vec(), sei: if sei.is_null() { Vec::new() } else { copy(sei, sei_size) } })
		}
	}

	/// Encodes an NV12 frame (`width` x `height` luma, then the interleaved chroma at half the height)
	/// at `pts`, or with `None` takes a frame the encoder still holds ([`Encoder::delayed`]). The packet
	/// put out, if any, borrows the encoder until its next call.
	pub fn encode(&mut self, frame: Option<&[u8]>, pts: i64) -> Result<Option<Packet<'_>>> {
		let (mut out, mut out_pts, mut out_dts, mut keyframe) = (std::ptr::null(), 0i64, 0i64, 0i32);
		let (luma, chroma) = match frame {
			Some(nv12) => {
				if nv12.len() < self.luma_size * 3 / 2 {
					bail!("x264: a frame of {} bytes for {} pixels", nv12.len(), self.luma_size);
				}
				(nv12.as_ptr(), nv12[self.luma_size..].as_ptr())
			}
			None => (std::ptr::null(), std::ptr::null()),
		};
		let stride = self.width as i32;
		// SAFETY: the frame outlives the call (x264 copies it in); its output stays valid until the next
		// call, which the borrow of `self` enforces
		let size = unsafe { ror_x264_encode(self.handle.as_ptr(), luma, stride, chroma, stride, pts, &mut out, &mut out_pts, &mut out_dts, &mut keyframe) };
		if size < 0 {
			bail!("x264 failed to encode: {}", self.error());
		}
		if size == 0 || out.is_null() {
			return Ok(None);
		}
		Ok(Some(Packet { data: unsafe { std::slice::from_raw_parts(out, size as usize) }, pts: out_pts, dts: out_dts, keyframe: keyframe != 0 }))
	}

	/// Frames the encoder holds that [`Encoder::encode`] with no frame still puts out.
	pub fn delayed(&self) -> usize {
		unsafe { ror_x264_delayed(self.handle.as_ptr()) }.max(0) as usize
	}

	fn error(&self) -> String {
		unsafe { CStr::from_ptr(ror_x264_error(self.handle.as_ptr())) }.to_string_lossy().into_owned()
	}
}

impl Drop for Encoder {
	fn drop(&mut self) {
		// SAFETY: opened by ror_x264_open, closed once
		unsafe { ror_x264_close(self.handle.as_ptr()) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn x264_encodes_frames_in_order_and_flushes() {
		let (width, height) = (64u32, 48u32);
		let settings = Settings { width, height, fps: (30, 1), preset: "ultrafast".into(), crf: 19.0, threads: 2 };
		let mut encoder = Encoder::open(&settings).unwrap();
		let headers = encoder.headers().unwrap();
		assert_eq!(headers.sps[0] & 0x1f, 7, "an SPS");
		assert_eq!(headers.pps[0] & 0x1f, 8, "a PPS");
		assert!(String::from_utf8_lossy(&headers.sei).contains("x264 - core 164 r3192 c24e06c"));
		let mut frames = Vec::new();
		let mut record = |packet: Option<Packet<'_>>| {
			if let Some(packet) = packet {
				frames.push((packet.pts, packet.dts, packet.keyframe, packet.data.len()));
			}
		};
		for index in 0..10i64 {
			let frame: Vec<u8> = (0..width * height * 3 / 2).map(|byte| (byte as i64 * 7 + index * 13) as u8).collect();
			record(encoder.encode(Some(&frame), index).unwrap());
		}
		while encoder.delayed() > 0 {
			record(encoder.encode(None, 0).unwrap());
		}
		assert_eq!(frames.iter().map(|frame| frame.0).collect::<Vec<_>>(), (0..10).collect::<Vec<_>>(), "every frame once, in order (no B-frames)");
		assert!(frames.iter().all(|frame| frame.0 == frame.1 && frame.3 > 4));
		assert!(frames[0].2, "the first is a keyframe");
		assert!(Encoder::open(&Settings { preset: "nonsense".into(), ..settings }).is_err());
	}
}
