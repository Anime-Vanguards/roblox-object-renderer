//! The local lights' voxels baked once a view (notes/render.md "Local lights"): particles, beams and
//! trails with LightInfluence read Studio's voxels per pixel, 64 of them for each light in reach
//! (`voxel_light`), which made those effects the frame's whole cost where they pile up (one effect's
//! aura: 890 particles covering a 1650 px view 97 times over under 2 lights, 89 ms of its 90). The
//! voxels are a fixed field, so a compute pass writes it over the box the lit effects span, each voxel
//! its 27 neighbours' fade summed over the lights (shaders/voxel_field.wgsl) into a 3D texture, and a
//! pixel takes one trilinear sample of it, two voxels along each axis weighed as `voxel_light` weighs
//! them: the same sum whatever the lights (shaders/voxel_read.wgsl; 8 loads of a buffer instead cost
//! that effect 8-10 ms more).
//! A point outside the field, or a view whose lit effects span more than [`MOST_VOXELS`], reads the
//! lights' voxels as before.

use glam::{IVec3, Vec3};

use super::profile::Profile;
use super::reuse::Lease;

/// The most voxels a view's field holds (8 MB): a box of 400 studs a side.
pub const MOST_VOXELS: u64 = 1 << 20;

/// The field's texels: the lights' sum (rgb).
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The field's sides are whole multiples of this many voxels: a view's field the size of the last
/// one's draws into the texture it gave back (render::reuse), as an effect turning with the camera
/// moves its box a voxel either way.
const ROUND: i32 = 8;

/// How far from the camera effects take the voxels' light (common.wgsl VOXEL_LIGHT_REACH), studs.
pub const LIGHT_REACH: f32 = 493.0;

/// How far towards the camera a particle reads the voxels (particle.wgsl), studs.
pub const PARTICLE_READ: f32 = 6.0;

/// Where in the field a view's effects read (the first voxel's index and how many along each axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
	pub origin: IVec3,
	pub size: IVec3,
}

impl Extent {
	/// The voxels a point in `lit` reads (two along each axis round it), within the lights' reach
	/// boxes (`lights`: each light's centre and how far its voxels reach round it, the face's half
	/// extents added for a SurfaceLight): None when no light reaches the effects or the box would
	/// hold more than [`MOST_VOXELS`].
	pub fn of(lit: (Vec3, Vec3), lights: impl Iterator<Item = (Vec3, f32)>) -> Option<Extent> {
		let reached = lights.fold(None, |reached: Option<(Vec3, Vec3)>, (centre, reach)| {
			let (low, high) = (centre - Vec3::splat(reach), centre + Vec3::splat(reach));
			Some(reached.map_or((low, high), |(min, max)| (min.min(low), max.max(high))))
		})?;
		let (low, high) = (lit.0.max(reached.0), lit.1.min(reached.1));
		if low.cmpgt(high).any() || !low.is_finite() || !high.is_finite() {
			return None;
		}
		let first = ((low - 2.0) * 0.25).floor();
		let last = ((high - 2.0) * 0.25).floor() + 1.0;
		let count = last - first + 1.0;
		if (count.x as f64) * (count.y as f64) * (count.z as f64) > MOST_VOXELS as f64 {
			return None;
		}
		let size = (count.as_ivec3() + (ROUND - 1)) / ROUND * ROUND;
		(size.x as u64 * size.y as u64 * size.z as u64 <= MOST_VOXELS).then_some(Extent { origin: first.as_ivec3(), size })
	}

	fn voxels(&self) -> u32 {
		(self.size.x * self.size.y * self.size.z) as u32
	}
}

/// The bake's pipeline.
pub(super) struct Baker {
	layout: wgpu::BindGroupLayout,
	pipeline: wgpu::ComputePipeline,
}

impl Baker {
	pub fn new(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Baker {
		let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
		let buffer = |ty: wgpu::BufferBindingType| wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None };
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("voxel field"),
			entries: &[
				entry(0, buffer(wgpu::BufferBindingType::Uniform)),
				entry(1, buffer(wgpu::BufferBindingType::Storage { read_only: true })),
				entry(2, wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: FORMAT, view_dimension: wgpu::TextureViewDimension::D3 }),
			],
		});
		let source = format!("{}\n{}", super::pipelines::COMMON, include_str!("../../shaders/voxel_field.wgsl"));
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("voxel field"), source: wgpu::ShaderSource::Wgsl(source.into()) });
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("voxel field"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
		let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("voxel field"), layout: Some(&pipeline_layout), module: &module, entry_point: Some("bake"), compilation_options: Default::default(), cache });
		Baker { layout, pipeline }
	}

	/// Records the bake of `extent` from the view's `lights` (`count` of them) into a texture of this
	/// render's.
	pub fn bake(&self, encoder: &mut wgpu::CommandEncoder, lease: &Lease, lights: &wgpu::Buffer, count: u32, extent: Extent, profile: &Profile) -> wgpu::TextureView {
		let voxels = extent.voxels();
		let field = lease
			.texture(&wgpu::TextureDescriptor {
				label: Some("voxel field"),
				size: wgpu::Extent3d { width: extent.size.x as u32, height: extent.size.y as u32, depth_or_array_layers: extent.size.z as u32 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D3,
				format: FORMAT,
				usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&Default::default());
		let origin = extent.origin.to_array();
		let info: [u32; 8] = [origin[0] as u32, origin[1] as u32, origin[2] as u32, 0, extent.size.x as u32, extent.size.y as u32, extent.size.z as u32, count];
		let info = lease.buffer("voxel field info", wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&info));
		let group = lease.device().create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("voxel field"),
			layout: &self.layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: info.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 1, resource: lights.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&field) },
			],
		});
		let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("voxel field"), timestamp_writes: profile.compute_pass("voxel field") });
		pass.set_pipeline(&self.pipeline);
		pass.set_bind_group(0, &group, &[]);
		pass.dispatch_workgroups(voxels.div_ceil(64), 1, 1);
		drop(pass);
		field
	}
}

/// The box `points` span, grown by `margin` (None without any).
pub(super) fn bounds(points: impl Iterator<Item = Vec3>, margin: f32) -> Option<(Vec3, Vec3)> {
	points.fold(None, |bounds: Option<(Vec3, Vec3)>, point| Some(bounds.map_or((point, point), |(low, high)| (low.min(point), high.max(point))))).map(|(low, high)| (low - Vec3::splat(margin), high + Vec3::splat(margin)))
}

/// Two boxes' union.
pub(super) fn union(a: Option<(Vec3, Vec3)>, b: Option<(Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
	match (a, b) {
		(Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
		(a, b) => a.or(b),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_field_holds_both_voxels_round_every_lit_point() {
		assert_eq!(Extent::of((Vec3::ZERO, Vec3::splat(3.0)), [(Vec3::ZERO, 50.0)].into_iter()).expect("reached").size, IVec3::splat(ROUND));
		let extent = Extent::of((Vec3::new(-3.0, 0.5, 10.0), Vec3::new(5.0, 9.0, 13.9)), [(Vec3::ZERO, 50.0)].into_iter()).expect("reached");
		for point in [Vec3::new(-3.0, 0.5, 10.0), Vec3::new(5.0, 9.0, 13.9), Vec3::new(1.0, 4.0, 12.0)] {
			let base = ((point - 2.0) * 0.25).floor().as_ivec3() - extent.origin;
			assert!(base.cmpge(IVec3::ZERO).all() && (base + 1).cmplt(extent.size).all(), "{point}: {base} in {extent:?}");
		}
	}

	#[test]
	fn effects_no_light_reaches_need_none() {
		assert_eq!(Extent::of((Vec3::splat(100.0), Vec3::splat(110.0)), [(Vec3::ZERO, 20.0)].into_iter()), None);
		assert_eq!(Extent::of((Vec3::ZERO, Vec3::ONE), std::iter::empty()), None);
	}

	#[test]
	fn too_large_a_box_reads_the_lights() {
		assert_eq!(Extent::of((Vec3::splat(-2000.0), Vec3::splat(2000.0)), [(Vec3::ZERO, 5000.0)].into_iter()), None);
	}
}
