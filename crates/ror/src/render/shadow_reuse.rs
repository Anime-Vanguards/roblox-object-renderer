//! Shadow maps whose caster buffers and projection inputs have not changed. Maps are published
//! only after all their passes have been submitted, then stay read-only. Entries refer weakly to
//! the immutable geometry so they cannot keep a model alive past its preparation cache.
//!
//! A map let go of goes back to the render targets' pool (reuse) when no frame holds it (a frame
//! that drew with it holds it through its lease until it is submitted): the next map kept takes it.
//! Clips drawing the same moments at once (eight of one map clip) kept a 1080p sun atlas (84 MB) a
//! frame, each made by the driver and dropped again a frame later.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use glam::Vec3;
use parking_lot::Mutex;

use super::SunCascade;
use super::prepared::PreparedGeometry;
use super::reuse::Reuse;

const KEPT_FOR: Duration = Duration::from_secs(30);
const KEPT_MAPS: usize = 8;

#[derive(Clone)]
pub(super) struct Maps {
	pub view: wgpu::TextureView,
	pub columns: Vec<u32>,
	pub bytes: u64,
}

struct Kept {
	geometry: Weak<PreparedGeometry>,
	key: Vec<u8>,
	/// Held too by every frame drawing with them, until it is submitted
	maps: Arc<Maps>,
	texture: wgpu::Texture,
	used: Instant,
}

impl Kept {
	fn live(&self) -> bool {
		self.used.elapsed() < KEPT_FOR && self.geometry.strong_count() > 0
	}

	/// Its texture to the pool, unless a frame still holds it (then it goes when that is done).
	fn let_go(self, reuse: &Reuse) {
		if Arc::strong_count(&self.maps) == 1 {
			reuse.give_back(self.texture);
		}
	}
}

struct Seen {
	geometry: Weak<PreparedGeometry>,
	key: Vec<u8>,
	used: Instant,
}

pub(super) struct Shadows {
	kept: Mutex<Vec<Kept>>,
	seen: Mutex<Vec<Seen>>,
	budget: u64,
}

impl Shadows {
	pub fn new(budget: u64) -> Self {
		Self { kept: Mutex::default(), seen: Mutex::default(), budget }
	}

	/// The maps kept for `key`: the caller holds them until its frame is submitted.
	pub fn get(&self, geometry: &Arc<PreparedGeometry>, key: &[u8], reuse: &Reuse) -> Option<Arc<Maps>> {
		let mut kept = self.kept.lock();
		Self::expire(&mut kept, reuse);
		let entry = kept.iter_mut().find(|entry| entry.key == key && Weak::ptr_eq(&entry.geometry, &Arc::downgrade(geometry)))?;
		entry.used = Instant::now();
		Some(entry.maps.clone())
	}

	fn expire(kept: &mut Vec<Kept>, reuse: &Reuse) {
		let mut index = 0;
		while index < kept.len() {
			if kept[index].live() {
				index += 1;
			} else {
				kept.swap_remove(index).let_go(reuse);
			}
		}
	}

	/// A changing projection (a turntable or the time of day) should keep using the texture pool,
	/// rather than retaining an atlas that will never be sampled again and allocating a new one.
	pub fn repeated(&self, geometry: &Arc<PreparedGeometry>, key: &[u8]) -> bool {
		let geometry = Arc::downgrade(geometry);
		let mut seen = self.seen.lock();
		seen.retain(|entry| entry.used.elapsed() < KEPT_FOR && entry.geometry.strong_count() > 0);
		if let Some(entry) = seen.iter_mut().find(|entry| entry.key == key && Weak::ptr_eq(&entry.geometry, &geometry)) {
			entry.used = Instant::now();
			return true;
		}
		if seen.len() >= KEPT_MAPS {
			let oldest = seen.iter().enumerate().min_by_key(|(_, entry)| entry.used).map(|(index, _)| index).unwrap();
			seen.swap_remove(oldest);
		}
		seen.push(Seen { geometry, key: key.to_vec(), used: Instant::now() });
		false
	}

	/// Whether the maps were retained: the caller then removes their texture from its lease.
	pub fn insert(&self, geometry: &Arc<PreparedGeometry>, key: Vec<u8>, maps: Maps, texture: wgpu::Texture, reuse: &Reuse) -> bool {
		if maps.bytes > self.budget {
			return false;
		}
		let geometry = Arc::downgrade(geometry);
		let mut kept = self.kept.lock();
		Self::expire(&mut kept, reuse);
		if let Some(index) = kept.iter().position(|entry| entry.key == key && Weak::ptr_eq(&entry.geometry, &geometry)) {
			kept.swap_remove(index).let_go(reuse);
		}
		kept.push(Kept { geometry, key, maps: Arc::new(maps), texture, used: Instant::now() });
		let mut total: u64 = kept.iter().map(|entry| entry.maps.bytes).sum();
		while total > self.budget || kept.len() > KEPT_MAPS {
			let oldest = kept.iter().enumerate().min_by_key(|(_, entry)| entry.used).map(|(index, _)| index).unwrap();
			let entry = kept.swap_remove(oldest);
			total -= entry.maps.bytes;
			entry.let_go(reuse);
		}
		true
	}

	pub fn trim(&self, reuse: &Reuse) {
		Self::expire(&mut self.kept.lock(), reuse);
		self.seen.lock().retain(|entry| entry.used.elapsed() < KEPT_FOR && entry.geometry.strong_count() > 0);
	}

	/// Lets go of every kept map (not to the pool); returns their bytes.
	pub fn clear(&self) -> u64 {
		self.seen.lock().clear();
		std::mem::take(&mut *self.kept.lock()).iter().map(|entry| entry.maps.bytes).sum()
	}

	pub fn stats(&self) -> (usize, u64) {
		let kept = self.kept.lock();
		(kept.len(), kept.iter().map(|entry| entry.maps.bytes).sum())
	}
}

pub(super) fn local_key(lights: &[super::ShadowLight], size: u32) -> Vec<u8> {
	let mut key = vec![0];
	key.extend_from_slice(&size.to_le_bytes());
	for light in lights {
		let (origin, aim) = (light.origin, light.aim);
		key.extend_from_slice(bytemuck::cast_slice(&[origin.x, origin.y, origin.z, light.reach, aim.x, aim.y, aim.z, aim.w, light.near]));
	}
	key
}

pub(super) fn sun_key(cascades: &[SunCascade], direction: Vec3) -> Vec<u8> {
	let mut key = vec![1];
	key.extend_from_slice(bytemuck::cast_slice(&direction.to_array()));
	for cascade in cascades {
		let SunCascade { index, center, radius, texels, view_proj, far, reach, sigma } = cascade;
		key.extend_from_slice(&(*index as u64).to_le_bytes());
		key.extend_from_slice(&texels.to_le_bytes());
		key.extend_from_slice(bytemuck::cast_slice(&[center.x, center.y, center.z, *radius, *far, *reach, *sigma]));
		key.extend_from_slice(bytemuck::cast_slice(&view_proj.to_cols_array()));
	}
	key
}

#[cfg(test)]
mod tests {
	use super::*;
	use glam::Mat4;

	#[test]
	fn local_maps_follow_order_origin_reach_aim_and_resolution() {
		let light = |origin: Vec3, reach: f32| super::super::ShadowLight { origin, reach, aim: glam::Vec4::ZERO, near: 0.05 };
		let lights = [light(Vec3::ZERO, 10.0), light(Vec3::X, 20.0)];
		let key = local_key(&lights, 128);
		assert_ne!(key, local_key(&[lights[1], lights[0]], 128));
		assert_ne!(key, local_key(&[light(Vec3::Y, 10.0), lights[1]], 128));
		assert_ne!(key, local_key(&[light(Vec3::ZERO, 11.0), lights[1]], 128));
		assert_ne!(key, local_key(&[super::super::ShadowLight { aim: glam::Vec4::new(0.0, 0.0, 1.0, 0.1), ..lights[0] }, lights[1]], 128));
		assert_ne!(key, local_key(&lights, 256));
	}

	#[test]
	fn sun_maps_follow_projection_culling_and_blur_inputs() {
		let mut cascades = [SunCascade { index: 0, center: Vec3::ZERO, radius: 10.0, texels: 128, view_proj: Mat4::IDENTITY, far: 100.0, reach: 50.0, sigma: 1.0 }];
		let key = sun_key(&cascades, Vec3::Y);
		assert_ne!(key, sun_key(&cascades, Vec3::X));
		cascades[0].center.x += 1.0;
		assert_ne!(key, sun_key(&cascades, Vec3::Y));
		cascades[0].center.x -= 1.0;
		cascades[0].sigma = 2.0;
		assert_ne!(key, sun_key(&cascades, Vec3::Y));
		cascades[0].sigma = 1.0;
		cascades[0].view_proj = Mat4::from_translation(Vec3::Z);
		assert_ne!(key, sun_key(&cascades, Vec3::Y));
	}
}
