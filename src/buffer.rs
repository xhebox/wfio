//! Page-aligned I/O buffers for `O_DIRECT` / `F_NOCACHE` / `FILE_FLAG_NO_BUFFERING`.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::sync::Arc;

use completion::{AsIoVec, IoVec};

/// Alignment for direct I/O buffers: 4096 bytes satisfies 512B and 4K logical
/// and physical sector alignments across Linux, macOS, and Windows.
pub const DIRECT_ALIGN: usize = 4096;

struct RawAligned {
	ptr: NonNull<u8>,
	layout: Layout,
}

// SAFETY: `RawAligned` owns a heap allocation accessed through shared `IoVec`
// slices; each in-flight slot receives its own non-overlapping slice.
unsafe impl Send for RawAligned {}
unsafe impl Sync for RawAligned {}

impl Drop for RawAligned {
	fn drop(&mut self) {
		// SAFETY: `ptr` was allocated with `layout` in `AlignedBuffer::new`.
		unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
	}
}

/// A 4096-byte-aligned buffer that implements [`AsIoVec`] and can be cheaply
/// cloned or sliced per in-flight I/O slot.
#[derive(Clone)]
pub struct AlignedBuffer {
	raw: Arc<RawAligned>,
	offset: usize,
	len: usize,
}

impl AlignedBuffer {
	/// Allocates `len` bytes aligned to [`DIRECT_ALIGN`], filled with a
	/// deterministic non-zero byte pattern so filesystem compression or zero
	/// page deduplication cannot short-circuit writes.
	pub fn patterned(len: usize) -> Self {
		let alloc_len = len.max(1);
		let layout = Layout::from_size_align(alloc_len, DIRECT_ALIGN)
			.expect("aligned buffer layout is valid");
		// SAFETY: `alloc_len >= 1` and `DIRECT_ALIGN` is a power of two.
		let ptr = unsafe { alloc_zeroed(layout) };
		let Some(ptr) = NonNull::new(ptr) else {
			std::alloc::handle_alloc_error(layout);
		};
		// Fill with a fast deterministic pattern.
		// SAFETY: `ptr` points to `alloc_len` initialized bytes.
		let slice = unsafe { std::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
		for (i, byte) in slice.iter_mut().enumerate() {
			*byte = (i as u8).wrapping_mul(31).wrapping_add(17);
		}
		Self {
			raw: Arc::new(RawAligned { ptr, layout }),
			offset: 0,
			len,
		}
	}

	/// Byte length of this buffer view.
	pub fn len(&self) -> usize {
		self.len
	}

	/// Returns a sub-view `[offset..offset + len]` sharing the backing allocation.
	pub fn slice(&self, offset: usize, len: usize) -> Self {
		assert!(
			offset.saturating_add(len) <= self.len,
			"slice out of bounds"
		);
		Self {
			raw: Arc::clone(&self.raw),
			offset: self.offset + offset,
			len,
		}
	}
}

// SAFETY: `self.raw` keeps the allocation alive for the lifetime of `self`, and
// `self.offset + self.len` stays within the allocation.
unsafe impl AsIoVec for AlignedBuffer {
	fn as_iovec(&self) -> IoVec {
		IoVec {
			// SAFETY: `self.offset` is in bounds of the allocation.
			ptr: unsafe { self.raw.ptr.as_ptr().add(self.offset) },
			len: self.len,
		}
	}
}
