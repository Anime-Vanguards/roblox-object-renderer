//! Bounds-checked little-endian reading over a byte slice.

use crate::MeshError;

/// A cursor over a byte slice. Every read checks the remaining length and reports what was being
/// read when the data runs out, so malformed input becomes a [`MeshError::Truncated`].
#[derive(Clone)]
pub(crate) struct Reader<'a> {
	data: &'a [u8],
	pos: usize,
}

impl<'a> Reader<'a> {
	pub(crate) fn new(data: &'a [u8]) -> Self {
		Self { data, pos: 0 }
	}

	pub(crate) fn pos(&self) -> usize {
		self.pos
	}

	pub(crate) fn remaining(&self) -> usize {
		self.data.len() - self.pos
	}

	pub(crate) fn bytes(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], MeshError> {
		if len > self.remaining() {
			return Err(MeshError::Truncated(what));
		}
		let slice = &self.data[self.pos..self.pos + len];
		self.pos += len;
		Ok(slice)
	}

	/// `count` items of `size` bytes each, as one slice.
	pub(crate) fn array(&mut self, count: usize, size: usize, what: &'static str) -> Result<&'a [u8], MeshError> {
		let Some(len) = count.checked_mul(size) else {
			return Err(MeshError::Truncated(what));
		};
		self.bytes(len, what)
	}

	pub(crate) fn skip(&mut self, len: usize, what: &'static str) -> Result<(), MeshError> {
		self.bytes(len, what).map(|_| ())
	}

	pub(crate) fn fixed<const N: usize>(&mut self, what: &'static str) -> Result<[u8; N], MeshError> {
		let bytes = self.bytes(N, what)?;
		let mut out = [0; N];
		out.copy_from_slice(bytes);
		Ok(out)
	}

	pub(crate) fn u8(&mut self, what: &'static str) -> Result<u8, MeshError> {
		Ok(self.fixed::<1>(what)?[0])
	}

	pub(crate) fn u16(&mut self, what: &'static str) -> Result<u16, MeshError> {
		Ok(u16::from_le_bytes(self.fixed(what)?))
	}

	pub(crate) fn u32(&mut self, what: &'static str) -> Result<u32, MeshError> {
		Ok(u32::from_le_bytes(self.fixed(what)?))
	}

	/// A `u32` length followed by that many bytes.
	pub(crate) fn sized(&mut self, what: &'static str) -> Result<&'a [u8], MeshError> {
		let len = self.u32(what)? as usize;
		self.bytes(len, what)
	}
}

#[inline]
pub(crate) fn le_u16(bytes: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

#[inline]
pub(crate) fn le_u32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[inline]
pub(crate) fn le_f32(bytes: &[u8], at: usize) -> f32 {
	f32::from_bits(le_u32(bytes, at))
}

#[inline]
pub(crate) fn le_vec3(bytes: &[u8], at: usize) -> [f32; 3] {
	[le_f32(bytes, at), le_f32(bytes, at + 4), le_f32(bytes, at + 8)]
}
