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

// SAFETY: `AVBufferRef`'s refcount is atomically managed by FFmpeg, so
// transferring ownership of an `FfmpegBuffer` across threads is sound —
// `Drop` (which is the only operation that mutates the refcount) calls
// `av_buffer_unref` which uses atomic decrement.
//
// We deliberately do **not** implement `Sync`. Decoder-output buffers
// from FFmpeg are immutable in practice, but the underlying
// `AVBufferRef.data` is reachable through `as_av_buffer_ref` and
// nothing in this type's contract prevents a caller from passing the
// pointer to an FFmpeg API that mutates the bytes — concurrent reads
// from another thread would then race. `Send`-only is the conservative
// stance.
unsafe impl Send for FfmpegBuffer {}

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

  /// Allocates a 1-byte refcounted `AVBufferRef` and exposes a
  /// zero-length view over it. Useful as a placeholder when
  /// constructing an "empty" `mediadecode::VideoFrame` /
  /// `AudioFrame` to pass to a decoder's `receive_frame` — the
  /// decoder overwrites the planes on success, but the slot needs a
  /// non-null buffer to satisfy the array shape.
  ///
  /// # Panics
  ///
  /// Panics if FFmpeg fails to allocate (out-of-memory). Allocations
  /// of one byte never realistically fail; this matches the
  /// behaviour of `Clone` on a populated `FfmpegBuffer`. Callers who
  /// need to recover from OOM should use [`Self::try_empty`].
  #[inline]
  pub fn empty() -> Self {
    Self::try_empty().expect("FfmpegBuffer::empty: av_buffer_alloc returned null (OOM)")
  }

  /// Fallible counterpart to [`Self::empty`]. Returns `None` if the
  /// 1-byte `av_buffer_alloc` fails (out-of-memory). Use this when
  /// you'd rather propagate an error than panic.
  #[inline]
  pub fn try_empty() -> Option<Self> {
    use ffmpeg_next::ffi::av_buffer_alloc;
    let raw = unsafe { av_buffer_alloc(1) };
    if raw.is_null() {
      return None;
    }
    // SAFETY: `raw` is non-null and freshly allocated; we transfer
    // its single reference to the new `FfmpegBuffer`.
    let mut buf = unsafe { Self::take(raw) }?;
    buf.len = 0;
    Some(buf)
  }

  /// Borrows the refcounted payload of an `ffmpeg::Packet` as an
  /// `FfmpegBuffer` view. The packet's `AVBufferRef` is shared via
  /// refcount bump — no copy. The view spans exactly
  /// `(*packet.as_ptr()).data .. data + size` (the *payload*) — not
  /// the entire underlying allocation: `AVPacket.buf` can be larger
  /// than the payload (encoder padding, oversized buffers, sub-range
  /// references after `av_packet_split_side_data`), so exposing the
  /// whole AVBufferRef would corrupt downstream consumers that
  /// trust the buffer to be just the compressed bytes.
  ///
  /// Returns `None` when the packet has no refcounted buffer
  /// (`buf == NULL`) — callers needing universal coverage of stack-
  /// or arena-allocated AVPackets can fall back to
  /// [`Self::copy_from_slice`] over `packet.data()`.
  #[inline]
  pub fn from_packet(packet: &ffmpeg_next::Packet) -> Option<Self> {
    use ffmpeg_next::packet::Ref;
    // SAFETY: `packet` keeps the AVPacket live for the duration of
    // this call; `.buf`, `.data`, `.size` are public fields on
    // AVPacket. `buf` may be null (stack-allocated packets).
    let buf_ptr = unsafe { (*packet.as_ptr()).buf };
    if buf_ptr.is_null() {
      return None;
    }
    let data_ptr = unsafe { (*packet.as_ptr()).data };
    let size_raw = unsafe { (*packet.as_ptr()).size };
    if data_ptr.is_null() || size_raw <= 0 {
      return None;
    }
    let payload_len = size_raw as usize;
    // Compute the offset of `data` inside `buf`. AVPacket guarantees
    // `data` lies within `buf->data .. buf->data + buf->size`, but
    // we verify defensively with `from_ref_view` (which bounds-
    // checks against `buf->size`).
    let buf_data = unsafe { (*buf_ptr).data };
    if buf_data.is_null() {
      return None;
    }
    let offset = (data_ptr as usize).wrapping_sub(buf_data as usize);
    unsafe { Self::from_ref_view(buf_ptr, offset, payload_len) }
  }

  /// Borrows one of an `ffmpeg::Frame`'s plane buffers
  /// (`AVFrame.buf[plane_idx]`) as an `FfmpegBuffer` view. The view
  /// covers the underlying `AVBufferRef`'s full size; for
  /// per-plane subviews into a multi-plane shared allocation see
  /// [`crate::convert::video_frame_from`].
  ///
  /// Returns `None` when `plane_idx >= 8` or the plane has no
  /// buffer attached.
  #[inline]
  pub fn from_frame_plane(frame: &ffmpeg_next::Frame, plane_idx: usize) -> Option<Self> {
    if plane_idx >= 8 {
      return None;
    }
    // SAFETY: `frame` keeps the AVFrame live for the duration of
    // this call; `buf[]` is a public fixed-size array on AVFrame.
    let buf_ptr = unsafe { (*frame.as_ptr()).buf[plane_idx] };
    unsafe { Self::from_ref(buf_ptr) }
  }

  /// Allocates a fresh refcounted `AVBufferRef` and copies `bytes` into
  /// it. Returns `None` if the FFmpeg allocation fails.
  ///
  /// Useful for adapting non-refcounted FFmpeg payloads (e.g. subtitle
  /// `AVSubtitleRect.text` / `.ass` / `.data[0]`) into the refcounted
  /// `FfmpegBuffer` shape the rest of the crate carries.
  #[inline]
  pub fn copy_from_slice(bytes: &[u8]) -> Option<Self> {
    use ffmpeg_next::ffi::av_buffer_alloc;
    let len = bytes.len();
    // av_buffer_alloc(0) is allowed on most platforms but isn't
    // portable; force a 1-byte allocation in that case so the resulting
    // buffer is non-null.
    let alloc_size = len.max(1);
    let raw = unsafe { av_buffer_alloc(alloc_size as _) };
    if raw.is_null() {
      return None;
    }
    if len > 0 {
      // SAFETY: raw is non-null and freshly allocated with `alloc_size >= len`
      // bytes; the source slice is valid for `len` reads.
      unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), (*raw).data, len);
      }
    }
    Some(Self {
      inner: raw,
      offset: 0,
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
  /// bytes for the lifetime of `self`. Returns a dangling-but-aligned
  /// pointer when the view is empty (parallel to `core::ptr::NonNull::dangling`)
  /// — the caller must respect [`Self::len`] before any read.
  #[inline]
  pub fn as_ptr(&self) -> *const u8 {
    // SAFETY: inner is non-null per constructor invariant. We guard
    // against null `data` (possible when the underlying AVBufferRef
    // was created with size 0) before doing pointer arithmetic, since
    // `null.add(offset)` is UB for offset > 0 even before any deref.
    unsafe {
      let data = (*self.inner).data;
      if data.is_null() {
        // Safe sentinel for empty/dataless buffers. The caller must
        // gate any read on `len() == 0`.
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
      }
      (data as *const u8).add(self.offset)
    }
  }

  /// Underlying `*const AVBufferRef`. Useful when handing the buffer
  /// back to an FFmpeg API that expects a borrowed pointer (do **not**
  /// call `av_buffer_unref` on the result — `self` still owns the ref).
  /// The returned pointer references the **whole** buffer, not just
  /// this view's sub-region.
  ///
  /// This intentionally returns `*const`, not `*mut`. FFmpeg APIs that
  /// mutate via the buffer (e.g. `av_buffer_make_writable`) should be
  /// reached through the unsafe constructors which transfer ownership;
  /// shared `&self` access must not allow aliased writes.
  #[inline]
  pub fn as_av_buffer_ref(&self) -> *const AVBufferRef {
    self.inner as *const _
  }

  /// Byte offset of this view's start within the underlying buffer.
  #[inline]
  pub fn offset(&self) -> usize {
    self.offset
  }

  /// Fallible counterpart to [`Clone::clone`]. Returns `None` if
  /// `av_buffer_ref` fails (out-of-memory) instead of panicking.
  /// Use this in OOM-recoverable paths; the `Clone` impl panics on
  /// the same failure to match Rust's standard `Clone` contract.
  #[inline]
  pub fn try_clone(&self) -> Option<Self> {
    // SAFETY: inner is non-null per invariant; av_buffer_ref
    // atomically bumps the refcount and returns null on OOM only.
    let new_ref = unsafe { av_buffer_ref(self.inner) };
    if new_ref.is_null() {
      return None;
    }
    Some(Self {
      inner: new_ref,
      offset: self.offset,
      len: self.len,
    })
  }
}

impl Clone for FfmpegBuffer {
  /// Refcounts the underlying `AVBufferRef`. **Panics** on OOM (see
  /// [`Self::try_clone`] for the fallible variant).
  fn clone(&self) -> Self {
    self
      .try_clone()
      .expect("FfmpegBuffer::clone: av_buffer_ref returned null (OOM)")
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
      // `offset + len <= buffer size` was established at construction
      // (and preserved by Clone), so the resulting pointer + length
      // stays inside the AVBufferRef's allocation.
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
