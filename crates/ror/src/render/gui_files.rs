//! What a GUI image kept on disk holds (`<state dir>/guis/<rur build>-<code>/<key>.gui`, and the
//! content store's, render::guis): the premultiplied pixels rur drew, their mips, and the file layout.
//! build.rs hashes this file, ror-image (the mips) and their locked dependencies into
//! `ROR_GUI_CODE`, the folder's second half, so a build whose GUI images could differ never reads
//! another's; render::guis only finds, reads and uploads them.

use anyhow::Result;

/// Premultiplied, with a box-filtered mip chain (minified GUIs average their colours as Studio's
/// multisampled geometry does, in display values).
pub(super) fn gui_image(width: u32, height: u32, mut rgba: Vec<u8>) -> ror_image::Image {
	for pixel in rgba.chunks_exact_mut(4) {
		let alpha = pixel[3] as u32;
		for value in &mut pixel[..3] {
			*value = ((*value as u32 * alpha + 127) / 255) as u8;
		}
	}
	let mut image = ror_image::Image::from_rgba8(width, height, rgba);
	ror_image::generate_mips(&mut image);
	image
}

/// A GUI's image as the GPU takes it: premultiplied RGBA8 with its box-filtered mip chain
/// ([`gui_image`]), each level's rows padded to wgpu's copy alignment one level after another,
/// compressed (zstd) in pieces of [`GUI_CHUNK`] bytes. Kept so in memory and on disk (`<key>.gui`), a
/// frame's images go to the GPU by decompressing every piece on every core straight into one staging
/// buffer, then one copy per level ([`Renderer::upload_guis`]). A GUI clip's frames each draw new
/// images (13 animated telegraphs: 25 MB a frame with their mips): from PNGs, decoding them and making
/// their mips again for every frame took 10 ms of each 15 ms frame, two thirds of it the
/// premultiplying and the mips. Compressed, a frame's images take about what their PNGs did (a clip's
/// GUI stage holds up to 20 frames ahead).
pub struct GuiImage {
	pub(super) width: u32,
	pub(super) height: u32,
	pub(super) levels: u32,
	/// Each piece's end in `compressed` (a piece decompresses to GUI_CHUNK bytes, the last to the rest)
	pub(super) ends: Vec<u32>,
	pub(super) compressed: Vec<u8>,
}

/// A `.gui` file: this, then width, height, levels and the number of pieces (u32, little-endian),
/// each piece's compressed length, then the pieces.
const GUI_FILE_MAGIC: &[u8; 8] = b"RORGUI2\0";
const GUI_FILE_HEADER: usize = 24;
/// The decompressed bytes of a piece: a big image's pieces spread over the cores (one zstd frame each
/// left a frame's upload waiting 3-4 ms on its largest image).
pub(super) const GUI_CHUNK: usize = 512 << 10;

/// Rows padded to this for buffer-to-texture copies.
const ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

impl GuiImage {
	/// Each level's (width, height, padded bytes per row, offset in the decompressed bytes), and their
	/// total size.
	pub(super) fn layout(width: u32, height: u32, levels: u32) -> (Vec<(u32, u32, u32, u64)>, u64) {
		let mut offset = 0u64;
		let mut out = Vec::with_capacity(levels as usize);
		for level in 0..levels {
			let (w, h) = ((width >> level).max(1), (height >> level).max(1));
			let row = (w * 4).next_multiple_of(ROW_ALIGNMENT);
			out.push((w, h, row, offset));
			offset += row as u64 * h as u64;
		}
		(out, offset)
	}

	/// Its decompressed size.
	pub(super) fn size(&self) -> u64 {
		Self::layout(self.width, self.height, self.levels).1
	}

	/// rur's straight-alpha pixels made ready for the GPU.
	pub(super) fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self> {
		let image = gui_image(width, height, rgba);
		let levels = image.levels.len() as u32;
		let (layout, size) = Self::layout(width, height, levels);
		let mut padded = vec![0u8; size as usize];
		for ((w, h, row, offset), data) in layout.iter().zip(&image.levels) {
			let tight = *w as usize * 4;
			anyhow::ensure!(data.len() >= tight * *h as usize, "GUI image level smaller than {w}x{h}");
			for (y, line) in data.chunks_exact(tight).take(*h as usize).enumerate() {
				let start = *offset as usize + y * *row as usize;
				padded[start..start + tight].copy_from_slice(line);
			}
		}
		drop(image);
		let mut compressor = zstd::bulk::Compressor::new(1)?;
		let mut compressed = Vec::new();
		let mut ends = Vec::new();
		for piece in padded.chunks(GUI_CHUNK) {
			compressed.extend_from_slice(&compressor.compress(piece)?);
			ends.push(compressed.len() as u32);
		}
		Ok(Self { width, height, levels, ends, compressed })
	}

	/// A `.gui` file's bytes, if it is one this build reads.
	pub(super) fn from_file(bytes: &[u8]) -> Option<Self> {
		if !bytes.starts_with(GUI_FILE_MAGIC) || bytes.len() < GUI_FILE_HEADER {
			return None;
		}
		let word = |at: usize| bytes.get(at..at + 4).map(|word| u32::from_le_bytes(word.try_into().unwrap()));
		let (width, height, levels, pieces) = (word(8)?, word(12)?, word(16)?, word(20)? as usize);
		if width == 0 || height == 0 || levels == 0 || levels > 32 {
			return None;
		}
		let size = Self::layout(width, height, levels).1 as usize;
		if pieces != size.div_ceil(GUI_CHUNK) {
			return None;
		}
		let data = GUI_FILE_HEADER + pieces * 4;
		let mut ends = Vec::with_capacity(pieces);
		let mut end = 0u32;
		for piece in 0..pieces {
			end = end.checked_add(word(GUI_FILE_HEADER + piece * 4)?)?;
			ends.push(end);
		}
		// all of it: a file cut short, or longer than its pieces, is not one
		if bytes.len() != data + end as usize {
			return None;
		}
		let compressed = bytes[data..].to_vec();
		Some(Self { width, height, levels, ends, compressed })
	}

	/// What a `.gui` file holds.
	pub(super) fn file_bytes(&self) -> Vec<u8> {
		let mut bytes = Vec::with_capacity(GUI_FILE_HEADER + self.ends.len() * 4 + self.compressed.len());
		bytes.extend_from_slice(GUI_FILE_MAGIC);
		for word in [self.width, self.height, self.levels, self.ends.len() as u32] {
			bytes.extend_from_slice(&word.to_le_bytes());
		}
		let mut start = 0;
		for &end in &self.ends {
			bytes.extend_from_slice(&(end - start).to_le_bytes());
			start = end;
		}
		bytes.extend_from_slice(&self.compressed);
		bytes
	}

	/// Its pieces: (compressed bytes, where they go in its decompressed bytes).
	pub(super) fn pieces(&self) -> impl Iterator<Item = (&[u8], usize)> {
		let starts = std::iter::once(0).chain(self.ends.iter().copied());
		starts.zip(self.ends.iter().copied()).enumerate().map(|(index, (start, end))| (&self.compressed[start as usize..end as usize], index * GUI_CHUNK))
	}

	/// Decompresses a piece into `target` (its place in a staging buffer), through ordinary memory:
	/// staging memory is write-combined, and zstd reads back what it wrote (each match copies earlier
	/// output), which straight into it took 9 s for a clip's GUIs.
	pub(super) fn unpack(piece: &[u8], target: &mut [u8]) -> Result<()> {
		thread_local! {
			static SCRATCH: std::cell::RefCell<(Vec<u8>, Option<zstd::bulk::Decompressor<'static>>)> = const { std::cell::RefCell::new((Vec::new(), None)) };
		}
		SCRATCH.with_borrow_mut(|(scratch, decompressor)| {
			scratch.resize(GUI_CHUNK, 0);
			if decompressor.is_none() {
				*decompressor = Some(zstd::bulk::Decompressor::new()?);
			}
			let written = decompressor.as_mut().unwrap().decompress_to_buffer(piece, &mut scratch[..])?;
			anyhow::ensure!(written == target.len(), "GUI image piece of {written} bytes, expected {}", target.len());
			target.copy_from_slice(&scratch[..written]);
			Ok(())
		})
	}
}
