//! `FfmpegBuffer` — owned, refcounted handle to an `AVBufferRef`.
//!
//! Both `AVPacket.buf` and `AVFrame.buf[i]` are FFmpeg's refcounted
//! buffers. This crate's adapter exposes them through a `Bytes`-like
//! type that implements `AsRef<[u8]>` so the buffer can be used as the
//! `B` parameter on `mediadecode::Packet<A, B>` / `Frame<A, B>` without
//! copying. Cloning bumps the refcount; dropping releases one
//! reference and lets FFmpeg free the memory when the last reference
//! goes away.

use core::{fmt, slice};

use ffmpeg_next::ffi::{AVBufferRef, av_buffer_ref, av_buffer_unref};

/// Owned, refcounted handle to a contiguous byte range inside an
/// `AVBufferRef`.
///
/// Holds one reference to the underlying `AVBufferRef`. The `view`
/// (offset + length) carves out a sub-region of the buffer's data —
/// useful when an `AVFrame` packs multiple planes into a single
/// allocation (e.g. NV12 with `data[1] == data[0] + Y_size`). Each
/// plane gets its own `FfmpegBuffer` view at a different offset,
/// every view bumps the refcount, and dropping one doesn't free the
/// underlying buffer until the last view goes away.
///
/// `Clone` shares the same view (offset + length unchanged). `Drop`
/// releases one reference via `av_buffer_unref`.
pub struct FfmpegBuffer {
  inner: *mut AVBufferRef,
  /// Offset from `inner.data` where this view starts.
  offset: usize,
  /// Byte length of this view. Always `<= inner.size - offset`.
  len: usize,
}

// SAFETY: `AVBufferRef` is internally synchronized by FFmpeg's atomic
// refcount and the data buffer it owns is read-only after creation
// (FFmpeg's documented contract — packets and frames are immutable
// after they're handed to the consumer). Hand-off across threads is
// allowed.
unsafe impl Send for FfmpegBuffer {}
unsafe impl Sync for FfmpegBuffer {}

impl FfmpegBuffer {
  /// Constructs an `FfmpegBuffer` by **incrementing** the refcount of
  /// an existing `AVBufferRef`. The view covers the buffer's full
  /// `size` (offset 0). The caller's `*mut AVBufferRef` is unchanged —
  /// it still owns its own reference and must be released independently.
  ///
  /// Returns `None` if `buf` is null or `av_buffer_ref` fails (out of
  /// memory).
  ///
  /// # Safety
  ///
  /// `buf` must either be null or point to a live `AVBufferRef` for
  /// the duration of this call.
  #[inline]
  pub unsafe fn from_ref(buf: *mut AVBufferRef) -> Option<Self> {
    if buf.is_null() {
      return None;
    }
    // SAFETY: caller upholds liveness; av_buffer_ref handles atomicity.
    let new_ref = unsafe { av_buffer_ref(buf) };
    if new_ref.is_null() {
      return None;
    }
    let len = unsafe { (*new_ref).size as usize };
    Some(Self {
      inner: new_ref,
      offset: 0,
      len,
    })
  }

  /// Constructs an `FfmpegBuffer` view over a sub-region of an existing
  /// `AVBufferRef`. The refcount is incremented; the view runs from
  /// `offset` for `len` bytes inside `(*buf).data`.
  ///
  /// Returns `None` if `buf` is null, `av_buffer_ref` fails, or
  /// `offset + len > (*buf).size`.
  ///
  /// # Safety
  ///
  /// `buf` must either be null or point to a live `AVBufferRef` for
  /// the duration of this call.
  #[inline]
  pub unsafe fn from_ref_view(buf: *mut AVBufferRef, offset: usize, len: usize) -> Option<Self> {
    if buf.is_null() {
      return None;
    }
    let buf_size = unsafe { (*buf).size };
    let end = offset.checked_add(len)?;
    if end > buf_size {
      return None;
    }
    let new_ref = unsafe { av_buffer_ref(buf) };
    if new_ref.is_null() {
      return None;
    }
    Some(Self {
      inner: new_ref,
      offset,
      len,
    })
  }

  /// Takes ownership of an existing `AVBufferRef` without bumping the
  /// refcount. The view covers the buffer's full size. Use this when
  /// the caller's reference will be dropped (e.g. transferring out of
  /// an `AVPacket`/`AVFrame`).
  ///
  /// Returns `None` if `buf` is null.
  ///
  /// # Safety
  ///
  /// `buf` must be either null or a live `AVBufferRef` whose reference
  /// the caller is willing to give up. After a successful call, the
  /// caller MUST NOT call `av_buffer_unref` on the same pointer.
  #[inline]
  pub unsafe fn take(buf: *mut AVBufferRef) -> Option<Self> {
    if buf.is_null() {
      return None;
    }
    let len = unsafe { (*buf).size };
    Some(Self {
      inner: buf,
      offset: 0,
      len,
    })
  }

  /// Number of bytes visible through this view.
  #[inline]
  pub fn len(&self) -> usize {
    self.len
  }

  /// True when the view is zero bytes long.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Raw pointer to the start of this view. Valid for [`Self::len`]
  /// bytes for the lifetime of `self`.
  #[inline]
  pub fn as_ptr(&self) -> *const u8 {
    // SAFETY: inner is non-null per constructor invariant; offset is
    // bounds-checked against the buffer's size at construction.
    unsafe { ((*self.inner).data as *const u8).add(self.offset) }
  }

  /// Underlying `*mut AVBufferRef`. Useful when handing the buffer
  /// back to an FFmpeg API that expects a borrowed pointer (do **not**
  /// call `av_buffer_unref` on the result — `self` still owns the ref).
  /// The returned pointer references the **whole** buffer, not just
  /// this view's sub-region.
  #[inline]
  pub fn as_av_buffer_ref(&self) -> *mut AVBufferRef {
    self.inner
  }

  /// Byte offset of this view's start within the underlying buffer.
  #[inline]
  pub fn offset(&self) -> usize {
    self.offset
  }
}

impl Clone for FfmpegBuffer {
  fn clone(&self) -> Self {
    // SAFETY: inner is non-null per invariant; av_buffer_ref atomically
    // bumps the refcount. A null return means OOM, which is exceptional
    // — we panic rather than silently truncate to a dangling Buffer.
    let new_ref = unsafe { av_buffer_ref(self.inner) };
    assert!(
      !new_ref.is_null(),
      "FfmpegBuffer::clone: av_buffer_ref returned null (OOM)",
    );
    Self {
      inner: new_ref,
      offset: self.offset,
      len: self.len,
    }
  }
}

impl Drop for FfmpegBuffer {
  fn drop(&mut self) {
    // SAFETY: inner is a live AVBufferRef per invariant. `av_buffer_unref`
    // takes `**mut AVBufferRef` and zeroes the pointer; we don't read
    // self.inner after this.
    unsafe { av_buffer_unref(&mut self.inner) };
  }
}

impl AsRef<[u8]> for FfmpegBuffer {
  #[inline]
  fn as_ref(&self) -> &[u8] {
    // SAFETY:
    // - `inner` is non-null (constructor invariant).
    // - The data pointer is non-null and valid for the underlying
    //   buffer's `size` bytes per FFmpeg's contract.
    // - `offset + len <= buffer size` is established at construction
    //   (and preserved by Clone), so the view stays in-bounds.
    // - The buffer is immutable for the lifetime we hold the refcount.
    unsafe {
      let data = (*self.inner).data as *const u8;
      if data.is_null() || self.len == 0 {
        return &[];
      }
      slice::from_raw_parts(data.add(self.offset), self.len)
    }
  }
}

impl fmt::Debug for FfmpegBuffer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FfmpegBuffer")
      .field("len", &self.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ffmpeg_next::ffi::av_buffer_alloc;

  /// Allocate a fresh AVBufferRef of `size` bytes, fill it with `fill`,
  /// and wrap it in our type via `take` (taking ownership of the
  /// caller's reference).
  fn make_buffer(size: usize, fill: u8) -> FfmpegBuffer {
    let raw = unsafe { av_buffer_alloc(size as _) };
    assert!(!raw.is_null(), "av_buffer_alloc failed");
    unsafe {
      let data = (*raw).data;
      core::ptr::write_bytes(data, fill, size);
    }
    unsafe { FfmpegBuffer::take(raw) }.expect("non-null take")
  }

  #[test]
  fn null_take_returns_none() {
    assert!(unsafe { FfmpegBuffer::take(core::ptr::null_mut()) }.is_none());
  }

  #[test]
  fn null_from_ref_returns_none() {
    assert!(unsafe { FfmpegBuffer::from_ref(core::ptr::null_mut()) }.is_none());
  }

  #[test]
  fn allocated_buffer_round_trips_bytes() {
    let buf = make_buffer(16, 0xAB);
    assert_eq!(buf.len(), 16);
    assert!(!buf.is_empty());
    let slice = buf.as_ref();
    assert_eq!(slice.len(), 16);
    assert!(slice.iter().all(|&b| b == 0xAB));
  }

  #[test]
  fn clone_bumps_refcount_and_keeps_data_alive() {
    let original = make_buffer(8, 0x5A);
    let cloned = original.clone();
    // Both references see the same bytes.
    assert_eq!(original.as_ref(), cloned.as_ref());
    assert_eq!(original.as_ptr(), cloned.as_ptr());
    // Drop one — the other must still be valid.
    drop(original);
    assert_eq!(cloned.len(), 8);
    assert!(cloned.as_ref().iter().all(|&b| b == 0x5A));
  }

  #[test]
  fn debug_shows_length() {
    let buf = make_buffer(42, 0);
    let s = format!("{buf:?}");
    assert!(s.contains("len: 42"), "got {s}");
  }

  #[test]
  fn from_ref_view_carves_out_subregion() {
    // 24-byte buffer: bytes 0..8 = 0xAA, 8..16 = 0xBB, 16..24 = 0xCC.
    let raw = unsafe { av_buffer_alloc(24) };
    assert!(!raw.is_null());
    unsafe {
      let data = (*raw).data;
      core::ptr::write_bytes(data, 0xAA, 8);
      core::ptr::write_bytes(data.add(8), 0xBB, 8);
      core::ptr::write_bytes(data.add(16), 0xCC, 8);
    }

    // Three independent views, each with its own refcount.
    let view_a = unsafe { FfmpegBuffer::from_ref_view(raw, 0, 8) }.expect("view_a");
    let view_b = unsafe { FfmpegBuffer::from_ref_view(raw, 8, 8) }.expect("view_b");
    let view_c = unsafe { FfmpegBuffer::from_ref_view(raw, 16, 8) }.expect("view_c");
    assert!(view_a.as_ref().iter().all(|&b| b == 0xAA));
    assert!(view_b.as_ref().iter().all(|&b| b == 0xBB));
    assert!(view_c.as_ref().iter().all(|&b| b == 0xCC));
    assert_eq!(view_a.offset(), 0);
    assert_eq!(view_b.offset(), 8);
    assert_eq!(view_c.offset(), 16);
    assert_eq!(view_a.len(), 8);

    // Drop the original; the views still keep the buffer alive.
    unsafe { av_buffer_unref(&mut { raw }) };
    let _ = (view_a, view_b, view_c);
  }

  #[test]
  fn from_ref_view_rejects_out_of_bounds() {
    let raw = unsafe { av_buffer_alloc(16) };
    assert!(!raw.is_null());
    // Past the end:
    assert!(unsafe { FfmpegBuffer::from_ref_view(raw, 10, 8) }.is_none());
    // Overflow protection (offset + len overflows usize):
    assert!(unsafe { FfmpegBuffer::from_ref_view(raw, usize::MAX, 1) }.is_none());
    unsafe { av_buffer_unref(&mut { raw }) };
  }

  #[test]
  fn empty_buffer_returns_empty_slice() {
    // av_buffer_alloc(0) is valid in FFmpeg; some platforms return a
    // non-null buf with data == null and size == 0. Either way, our
    // as_ref must return an empty slice without dereferencing data.
    let raw = unsafe { av_buffer_alloc(0) };
    if raw.is_null() {
      // Some allocators refuse 0; skip the test in that case.
      return;
    }
    let buf = unsafe { FfmpegBuffer::take(raw) }.expect("non-null take");
    assert_eq!(buf.len(), 0);
    assert!(buf.is_empty());
    assert_eq!(buf.as_ref(), &[] as &[u8]);
  }
}
