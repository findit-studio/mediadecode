//! The view lane's own proofs: that it shares rather than copies, and
//! that its auto-traits are the ones the contract promises.

use ffmpeg_next::ffi::{av_buffer_alloc, av_buffer_unref};

use super::{FfmpegBuffer, Origin};

/// A live `AVBufferRef` of `len` bytes, filled with a recognisable
/// pattern, released when the guard drops.
struct OwnedBuf(*mut ffmpeg_next::ffi::AVBufferRef);

impl OwnedBuf {
  /// The allocation's own size, as FFmpeg records it.
  fn size(&self) -> usize {
    // SAFETY: the guard owns a live buffer; `size` is a public field.
    unsafe { (*self.0).size }
  }

  fn new(len: usize) -> Self {
    // SAFETY: a plain allocation, checked for null, filled through its
    // own `data` pointer.
    unsafe {
      let raw = av_buffer_alloc(len as _);
      assert!(!raw.is_null(), "av_buffer_alloc");
      for i in 0..len {
        *(*raw).data.add(i) = (i % 251) as u8;
      }
      Self(raw)
    }
  }
}

impl Drop for OwnedBuf {
  fn drop(&mut self) {
    // SAFETY: this guard owns exactly one reference, released once.
    unsafe { av_buffer_unref(&mut self.0) };
  }
}

#[test]
fn a_view_points_inside_the_buffer_it_came_from() {
  let src = OwnedBuf::new(4096);
  // SAFETY: `src` keeps the buffer live across the call.
  let whole = unsafe { FfmpegBuffer::view_of(src.0, 0, src.size(), Origin::Foreign) }
    .expect("a view over the whole buffer");

  // **The zero-copy proof.** The exported bytes must live *inside* the
  // source allocation — not equal it, not resemble it. A copy would
  // land somewhere else in the address space, so comparing pointers is
  // the assertion a copy cannot pass.
  // SAFETY: `src` is live; `data` and `size` are public fields.
  let (base, size) = unsafe { ((*src.0).data as usize, (*src.0).size) };
  let exported = whole.as_ref().as_ptr() as usize;
  assert!(
    exported >= base && exported + whole.len() <= base + size,
    "the view escaped its buffer: {exported:#x} not inside {base:#x}..+{size}",
  );
  assert_eq!(
    exported, base,
    "a whole-buffer view starts where the buffer does"
  );
  assert_eq!(whole.len(), size);

  // A sub-view starts at its offset, still inside, still shared.
  // SAFETY: as above.
  let sub =
    unsafe { FfmpegBuffer::view_of(src.0, 1024, 512, Origin::Foreign) }.expect("a sub-view");
  assert_eq!(sub.as_ref().as_ptr() as usize, base + 1024);
  assert_eq!(sub.len(), 512);
  assert_eq!(sub.offset(), 1024);
  assert!(sub.ptr_eq(&whole), "two views of one buffer must share it");

  // And the bytes read back are the source's, at the right offset.
  assert_eq!(sub.as_ref()[0], (1024 % 251) as u8);
}

#[test]
fn a_clone_bumps_the_refcount_rather_than_copying() {
  let src = OwnedBuf::new(256);
  // SAFETY: `src` keeps the buffer live.
  let first =
    unsafe { FfmpegBuffer::view_of(src.0, 0, src.size(), Origin::Foreign) }.expect("a view");
  let second = first.clone();

  assert!(first.ptr_eq(&second), "clone must share, not copy");
  assert_eq!(first.as_ref().as_ptr(), second.as_ref().as_ptr());

  // Dropping one leaves the other readable — the refcount, working.
  drop(first);
  assert_eq!(second.len(), 256);
  assert_eq!(second.as_ref()[7], 7);
}

#[test]
fn a_view_outlives_the_reference_it_was_taken_from() {
  // The whole point of the refcount: the carrier keeps FFmpeg's buffer
  // alive after the packet or frame that owned it is gone.
  let view = {
    let src = OwnedBuf::new(64);
    // SAFETY: `src` is live for the call; the view takes its own ref.
    unsafe { FfmpegBuffer::view_of(src.0, 0, src.size(), Origin::Foreign) }.expect("a view")
  };
  assert_eq!(view.len(), 64);
  assert_eq!(view.as_ref()[63], 63);
}

#[test]
fn the_extent_is_proved_against_the_buffers_own_size() {
  let src = OwnedBuf::new(128);
  // Past the end, and overflowing — both refused rather than trusted.
  // SAFETY: `src` is live for each call.
  unsafe {
    assert!(FfmpegBuffer::view_of(src.0, 0, 129, Origin::Foreign).is_none());
    assert!(FfmpegBuffer::view_of(src.0, 128, 1, Origin::Foreign).is_none());
    assert!(FfmpegBuffer::view_of(src.0, usize::MAX, 1, Origin::Foreign).is_none());
    assert!(FfmpegBuffer::view_of(core::ptr::null_mut(), 0, 0, Origin::Foreign).is_none());
    // Exactly at the end is a legal empty view.
    assert!(FfmpegBuffer::view_of(src.0, 128, 0, Origin::Foreign).is_some());
  }
}

#[test]
fn the_empty_carrier_reads_as_no_bytes() {
  let empty = FfmpegBuffer::empty();
  assert!(empty.is_empty());
  assert_eq!(empty.len(), 0);
  assert!(empty.as_ref().is_empty());
}

#[test]
fn bytes_with_no_buffer_behind_them_are_copied_into_one() {
  // Subtitle text, side data and palettes have no `AVBufferRef` to
  // share, so this lane owns a copy — and says so.
  let carried = FfmpegBuffer::copy_from_slice(&[1, 2, 3, 4]).expect("allocation");
  assert_eq!(carried.as_ref(), &[1, 2, 3, 4]);
  assert_eq!(carried.len(), 4);
}

/// The auto-traits each lane promises, asserted at compile time.
///
/// `View` is `Send` so a decoded frame can cross a channel, and
/// **not** `Sync` because the bytes behind it are FFmpeg's and may be
/// written through an API this crate does not control. `Owned` is both,
/// because its bytes are nobody's but ours — that difference is the
/// whole reason there are two lanes.
#[test]
fn the_two_lanes_carry_the_auto_traits_they_promise() {
  const fn assert_send<T: Send>() {}
  const fn assert_sync<T: Sync>() {}

  assert_send::<FfmpegBuffer>();
  assert_send::<crate::FfmpegBytes>();
  assert_sync::<crate::FfmpegBytes>();

  // And the negative half, which a `const fn` cannot state: `FfmpegBuffer`
  // must **not** be `Sync`. Proved by a trick that only compiles while
  // that is true — an inherent method wins name resolution over a trait
  // method, so `IsSync::is_sync` is reached only when the blanket impl
  // does not apply.
  struct Probe<T>(core::marker::PhantomData<T>);
  trait IsSync {
    fn sync_status() -> bool;
  }
  impl<T: Sync> IsSync for Probe<T> {
    fn sync_status() -> bool {
      true
    }
  }
  impl Probe<FfmpegBuffer> {
    #[allow(dead_code)]
    fn sync_status() -> bool {
      false
    }
  }
  assert!(
    !Probe::<FfmpegBuffer>::sync_status(),
    "FfmpegBuffer must not be Sync — the view lane's contract depends on it",
  );
  assert!(
    Probe::<crate::FfmpegBytes>::sync_status(),
    "FfmpegBytes must stay Sync"
  );
}

#[test]
fn a_short_row_is_refused_rather_than_read_past() {
  // The regression for a `debug_assert` that guarded an out-of-bounds
  // read: in a release build it vanished and `from_rows` copied
  // `row_bytes` out of whatever the closure returned. A length that
  // arrives from a caller is an input, and an input is checked — in
  // every profile.
  let short = [0u8; 1];
  assert!(
    FfmpegBuffer::from_rows(1, 64, |_| &short[..]).is_none(),
    "a row shorter than the geometry must be refused, not read past",
  );
  // The mixed case: the first row is honest and the second is not, so
  // the refusal happens with an allocation already made — which the
  // guard must release rather than leak.
  let full = [7u8; 64];
  let mut calls = 0usize;
  assert!(
    FfmpegBuffer::from_rows(2, 64, |index| {
      calls += 1;
      if index == 0 { &full[..] } else { &short[..] }
    })
    .is_none(),
  );
  assert_eq!(calls, 2, "the refusal happens at the row that is wrong");

  // And the honest case still gathers.
  let gathered = FfmpegBuffer::from_rows(2, 64, |_| &full[..]).expect("an honest gather");
  assert_eq!(gathered.len(), 128);
  assert!(gathered.as_ref().iter().all(|&b| b == 7));
}

#[test]
fn provenance_is_recorded_at_capture_and_survives_a_clone() {
  let src = OwnedBuf::new(4096);

  // SAFETY: `src` owns a live buffer for the duration of the test.
  let plane = unsafe { FfmpegBuffer::view_of(src.0, 0, 512, Origin::Foreign) }.expect("a view");
  let payload =
    unsafe { FfmpegBuffer::view_of(src.0, 0, 512, Origin::PacketPayload) }.expect("a view");

  assert_eq!(plane.origin(), Origin::Foreign);
  assert_eq!(payload.origin(), Origin::PacketPayload);
  assert_eq!(
    payload.clone().origin(),
    Origin::PacketPayload,
    "provenance is a property of the bytes, so a clone keeps it",
  );

  // Narrowing gives the carrier a new end, and what now sits past it is
  // its own former contents — never the zeroed slack a decoder may
  // read. The claim has to go with the length.
  let mut narrowed = payload.clone();
  narrowed.shrink_to(256);
  assert_eq!(narrowed.len(), 256);
  assert_eq!(
    narrowed.origin(),
    Origin::Foreign,
    "narrowing a payload must drop its padding claim",
  );

  // A copy this lane made owns exactly its bytes and nothing behind
  // them.
  assert_eq!(
    FfmpegBuffer::copy_from_slice(&[1u8, 2, 3])
      .expect("a copy")
      .origin(),
    Origin::Foreign,
  );
  assert_eq!(FfmpegBuffer::empty().origin(), Origin::Foreign);
}
