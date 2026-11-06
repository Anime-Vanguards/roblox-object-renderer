//! Draws issued as runs: consecutive draws of one mesh's index range and one material whose instances
//! follow one another become one instanced draw, and a mesh's buffers or a material's bind group are
//! set only when they change. The order is the draws' own: the GPU draws an instanced draw's
//! instances one after another (Vulkan's primitive order: by instance, then by primitive), so the
//! image is the one the separate draws made, depth ties included. A map's 1080p clip drew 612-648
//! opaque parts a frame in its main pass as 279-285 runs, with 230 material binds instead of 640.

use std::ops::Range;

struct Run<'b> {
	vertices: &'b wgpu::Buffer,
	indices: &'b wgpu::Buffer,
	range: Range<u32>,
	material: Option<&'b wgpu::BindGroup>,
	instances: Range<u32>,
}

/// The draws of one pipeline in a pass: [`Runs::draw`] each in order, then [`Runs::finish`]. A new
/// `Runs` after every `set_pipeline` or other change of the pass's state.
pub(super) struct Runs<'b> {
	/// The bind group slot the materials go to
	slot: u32,
	pending: Option<Run<'b>>,
	vertices: Option<&'b wgpu::Buffer>,
	indices: Option<&'b wgpu::Buffer>,
	material: Option<&'b wgpu::BindGroup>,
}

impl<'b> Runs<'b> {
	pub fn new(slot: u32) -> Self {
		Self { slot, pending: None, vertices: None, indices: None, material: None }
	}

	/// Draws `range` of a mesh's indices for `instance` (with `material` in the slot, when given),
	/// joining the run before it when that is the same mesh range and material at the instance before.
	pub fn draw(&mut self, pass: &mut wgpu::RenderPass<'_>, vertices: &'b wgpu::Buffer, indices: &'b wgpu::Buffer, range: Range<u32>, instance: u32, material: Option<&'b wgpu::BindGroup>) {
		if let Some(run) = &mut self.pending
			&& run.instances.end == instance
			&& std::ptr::eq(run.vertices, vertices)
			&& std::ptr::eq(run.indices, indices)
			&& run.range == range
			&& run.material.map(std::ptr::from_ref) == material.map(std::ptr::from_ref)
		{
			run.instances.end += 1;
			return;
		}
		self.flush(pass);
		self.pending = Some(Run { vertices, indices, range, material, instances: instance..instance + 1 });
	}

	/// Draws the last run.
	pub fn finish(mut self, pass: &mut wgpu::RenderPass<'_>) {
		self.flush(pass);
	}

	fn flush(&mut self, pass: &mut wgpu::RenderPass<'_>) {
		let Some(run) = self.pending.take() else { return };
		if let Some(material) = run.material
			&& !self.material.is_some_and(|bound| std::ptr::eq(bound, material))
		{
			pass.set_bind_group(self.slot, material, &[]);
			self.material = Some(material);
		}
		if !self.vertices.is_some_and(|bound| std::ptr::eq(bound, run.vertices)) {
			pass.set_vertex_buffer(0, run.vertices.slice(..));
			self.vertices = Some(run.vertices);
		}
		if !self.indices.is_some_and(|bound| std::ptr::eq(bound, run.indices)) {
			pass.set_index_buffer(run.indices.slice(..), wgpu::IndexFormat::Uint32);
			self.indices = Some(run.indices);
		}
		pass.draw_indexed(run.range, 0, run.instances);
	}
}
