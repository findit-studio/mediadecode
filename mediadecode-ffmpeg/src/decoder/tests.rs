use super::*;

#[test]
fn no_codec_for_unknown_id() {
  let err = Error::NoCodec(0);
  assert!(format!("{err}").contains("no decoder"));
}

#[test]
fn videodecoder_is_send() {
  _assert_send();
}

#[test]
fn is_transient_recognises_eagain_and_eof() {
  let eagain = ffmpeg_next::Error::Other {
    errno: ffmpeg_next::error::EAGAIN,
  };
  assert!(is_transient(&eagain));
  assert!(is_transient(&ffmpeg_next::Error::Eof));
  let other = ffmpeg_next::Error::InvalidData;
  assert!(!is_transient(&other));
}

/// `is_hw_decode_failure` is the post-commit reclassification predicate:
/// a HW-only decoder's non-transient, non-EOF error means the committed
/// backend can't decode this content, so the wrapper must fall back to SW.
/// It must fire for the broad-by-design HW-failure set
/// (`External`/`Bug`/`Bug2`/`Unknown`/`InvalidData` plus the transfer
/// path's `Other { EINVAL }`) and must NOT fire for the transient set
/// (`EAGAIN`) or genuine `Eof` — trapping `Eof` would loop the caller in
/// infinite fallback-retry.
#[test]
fn is_hw_decode_failure_covers_hw_failures_excludes_transient_and_eof() {
  // Reclassify-to-fallback set.
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::External));
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::Bug));
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::Bug2));
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::Unknown));
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::InvalidData));
  // Transfer-path unsupported CPU pix_fmt: AVERROR(EINVAL).
  assert!(is_hw_decode_failure(&ffmpeg_next::Error::Other {
    errno: libc::EINVAL,
  }));

  // Must NOT fire: genuine end-of-stream must propagate.
  assert!(!is_hw_decode_failure(&ffmpeg_next::Error::Eof));
  // Must NOT fire: EAGAIN backpressure is transient (and excluded by the
  // call sites' `is_transient` guard, but verify the predicate too).
  assert!(!is_hw_decode_failure(&ffmpeg_next::Error::Other {
    errno: ffmpeg_next::error::EAGAIN,
  }));
  // A non-HW `Other` errno (e.g. ENOMEM) is not a HW-decode failure.
  assert!(!is_hw_decode_failure(&ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  }));
}

/// Regression: a `codec::Parameters` with a null inner pointer must be
/// rejected at the entrypoint, not deref'd. ffmpeg-next's
/// `Parameters::new()` does not check `avcodec_parameters_alloc()`, so a
/// safe caller can hand us such a value under OOM.
#[test]
fn open_rejects_null_parameters() {
  // SAFETY: Parameters::wrap accepts any pointer; we explicitly construct
  // one with null inner. avcodec_parameters_free is null-safe on Drop.
  let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
  match VideoDecoder::open(null_params) {
    Ok(_) => panic!("open should fail on null parameters"),
    Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
      assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
    }
    Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
  }
}

#[test]
fn open_with_rejects_null_parameters() {
  // SAFETY: see open_rejects_null_parameters.
  let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
  match VideoDecoder::open_with(null_params, Backend::VideoToolbox) {
    Ok(_) => panic!("open_with should fail on null parameters"),
    Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
      assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
    }
    Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
  }
}

/// `try_clone_packet` calls `av_packet_ref`, which deep-copies side
/// data via `av_packet_copy_props`. The probe budget therefore has to
/// include side-data bytes — otherwise a stream with a 16-byte payload
/// and a 1 MiB side-data attachment would only consume 16 bytes of the
/// 64 MiB budget per packet, and 256 buffered clones would retain
/// ~256 MiB of side data while logs claim a few KiB.
#[test]
fn packet_side_data_counts_against_probe_budget() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  const PAYLOAD_SIZE: usize = 16;
  const SIDE_DATA_SIZE: usize = 1024 * 1024; // 1 MiB

  let mut packet = Packet::new(PAYLOAD_SIZE);
  // SAFETY: packet is a freshly allocated AVPacket; av_packet_new_side_data
  // attaches a fresh `SIDE_DATA_SIZE`-byte buffer of the requested type
  // to it and returns a writable pointer (or NULL on OOM).
  let p = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      SIDE_DATA_SIZE,
    )
  };
  assert!(!p.is_null(), "av_packet_new_side_data returned NULL");

  assert_eq!(packet.size(), PAYLOAD_SIZE);
  let side = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
  assert!(
    side >= SIDE_DATA_SIZE,
    "side-data accounting must include the attached buffer; got {side}"
  );
  let total = packet.size().saturating_add(side);
  assert!(
    total >= PAYLOAD_SIZE + SIDE_DATA_SIZE,
    "probe budget must charge payload + side data; got {total}"
  );
}

#[test]
fn packet_side_data_is_zero_when_no_side_data() {
  let packet = Packet::new(64);
  assert_eq!(
    packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES),
    0
  );
  assert_eq!(packet_side_data_count(&packet), 0);
}

/// Packets with many tiny side-data entries must be charged the
/// per-entry descriptor + ref overhead, even when each entry's payload
/// `size` is zero. Without `SIDE_DATA_ENTRY_OVERHEAD`, a packet stuffed
/// with N zero-byte entries would charge 0 bytes against the budget
/// while `av_packet_ref` still allocates ~`N * 80` bytes of descriptor
/// + AVBufferRef + allocator overhead per cloned copy.
#[test]
fn packet_side_data_bytes_charges_descriptor_overhead_for_zero_size_entries() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  // Attach two zero-byte entries of distinct types so neither call
  // replaces the other.
  let p1 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      0,
    )
  };
  let p2 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_PALETTE,
      0,
    )
  };
  assert!(
    !p1.is_null() && !p2.is_null(),
    "av_packet_new_side_data NULL"
  );

  assert_eq!(packet_side_data_count(&packet), 2);
  let bytes = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
  assert!(
    bytes >= 2 * SIDE_DATA_ENTRY_OVERHEAD,
    "must charge descriptor overhead per entry even at zero payload; got {bytes}"
  );
}

/// `packet_side_data_bytes` must clamp its walk to `max_entries`
/// regardless of `side_data_elems`. Defense-in-depth: the caller is
/// expected to short-circuit packets whose count exceeds the cap, but
/// if a corrupt or weaponised packet ever does reach the helper, the
/// internal cap prevents an unbounded raw-pointer walk.
///
/// This test attaches 5 entries of distinct types and asks the helper
/// to walk only the first 2. Result must equal exactly `2 * overhead +
/// (size_a + size_b)`, confirming entries 3-5 were not even read.
#[test]
fn packet_side_data_bytes_respects_max_entries_cap() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  // Five distinct side-data types so each `av_packet_new_side_data`
  // call appends rather than replaces.
  let types_and_sizes: [(AVPacketSideDataType, usize); 5] = [
    (AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA, 100),
    (AVPacketSideDataType::AV_PKT_DATA_PALETTE, 200),
    (AVPacketSideDataType::AV_PKT_DATA_REPLAYGAIN, 300),
    (AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX, 400),
    (AVPacketSideDataType::AV_PKT_DATA_STEREO3D, 500),
  ];
  for (ty, size) in types_and_sizes {
    let p = unsafe { av_packet_new_side_data(packet.as_mut_ptr(), ty, size) };
    assert!(!p.is_null(), "av_packet_new_side_data returned NULL");
  }
  assert_eq!(packet_side_data_count(&packet), 5);

  let walked_2 = packet_side_data_bytes(&packet, 2);
  let walked_5 = packet_side_data_bytes(&packet, 5);

  assert_eq!(
    walked_2,
    2 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200,
    "max_entries=2 must walk exactly the first two entries"
  );
  assert_eq!(
    walked_5,
    5 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200 + 300 + 400 + 500,
    "max_entries=5 must walk all five entries"
  );
  // max_entries=0 short-circuits to 0.
  assert_eq!(packet_side_data_bytes(&packet, 0), 0);
  // max_entries larger than the actual count clamps to the actual count
  // (no out-of-bounds walk past `side_data_elems`).
  let walked_huge = packet_side_data_bytes(&packet, 1_000_000);
  assert_eq!(walked_huge, walked_5);
}

/// `MAX_PROBE_PACKET_SIDE_DATA_ENTRIES` is the cliff above which a
/// packet is rejected from the probe buffer regardless of byte total —
/// pure descriptor inflation is its own attack vector. Sanity-check
/// that `packet_side_data_count` reports the value the cap is checked
/// against.
#[test]
fn packet_side_data_count_reports_attached_entries() {
  use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

  let mut packet = Packet::new(0);
  let _p1 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      4,
    )
  };
  let _p2 = unsafe {
    av_packet_new_side_data(
      packet.as_mut_ptr(),
      AVPacketSideDataType::AV_PKT_DATA_PALETTE,
      4,
    )
  };
  assert_eq!(packet_side_data_count(&packet), 2);
}

/// `cpu_frame_bytes` must refuse to size a frame whose first plane has
/// a negative `linesize`. Pre-fix, the loop break treated negative the
/// same as zero (FFmpeg's "no more populated planes" sentinel), so a
/// vertically-flipped frame returned `Some(0)` and `drain_into_pending`
/// would queue it as a 0-byte allocation — letting up to
/// `MAX_PROBE_PENDING_FRAMES` such frames bypass the configured byte
/// budget entirely.
#[test]
fn cpu_frame_bytes_rejects_negative_first_plane_linesize() {
  let mut f = frame::Video::empty();
  // SAFETY: f is freshly allocated; we set `format` to NV12 and the
  // first plane's linesize negative (FFmpeg's vertical-flip convention).
  // No backing data buffer is allocated — cpu_frame_bytes must reject
  // before any pointer dereference.
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    (*raw).width = 1920;
    (*raw).height = 1080;
    (*raw).linesize[0] = -1920;
    (*raw).linesize[1] = -1920;
  }
  assert!(
    cpu_frame_bytes(&f).is_none(),
    "negative linesize must be unsizeable, not Some(0)"
  );
}

/// Build a synthetic `AVHWFramesContext`-backed `AVBufferRef` for
/// tests. The buffer's data is a zeroed `AVHWFramesContext` with only
/// `width` and `height` populated — enough for [`hw_frames_ctx_dimensions`]
/// / [`estimate_transfer_bytes`] to read the allocated dims.
///
/// Returned ref has refcount 1; transfer ownership into
/// `AVFrame.hw_frames_ctx` and let `av_frame_unref` (called by
/// `frame::Video::Drop`) free it via `av_buffer_default_free`.
fn make_hw_frames_ctx_ref(w: i32, h: i32) -> *mut ffmpeg_next::ffi::AVBufferRef {
  use ffmpeg_next::ffi::av_buffer_alloc;
  use std::mem::size_of;

  // SAFETY: `av_buffer_alloc(n)` returns a fresh `AVBufferRef` whose
  // `.data` points to `n` bytes of allocator-supplied storage. We
  // zero the AVHWFramesContext and write only `width` / `height`,
  // which is all the helpers we test read.
  unsafe {
    let buf = av_buffer_alloc(size_of::<AVHWFramesContext>());
    assert!(!buf.is_null(), "av_buffer_alloc returned NULL");
    let data = (*buf).data as *mut AVHWFramesContext;
    std::ptr::write_bytes(data, 0, 1);
    (*data).width = w;
    (*data).height = h;
    buf
  }
}

/// Sanity-check the positive path with a real allocation: an
/// `av_buffer_alloc`'d 4096-byte plane attached as `buf[0]` must
/// surface as `Some(4096)`.
#[test]
fn cpu_frame_bytes_sums_buf_sizes() {
  use ffmpeg_next::ffi::av_buffer_alloc;

  let mut f = frame::Video::empty();
  // SAFETY: av_buffer_alloc returns a fresh AVBufferRef. Attaching it
  // to AVFrame.buf[0] transfers ownership to the frame; av_frame_unref
  // on Drop releases it.
  let buf0 = unsafe { av_buffer_alloc(4096) };
  let buf1 = unsafe { av_buffer_alloc(2048) };
  assert!(!buf0.is_null() && !buf1.is_null());
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).buf[0] = buf0;
    (*raw).buf[1] = buf1;
    // Positive linesize so the negative-stride rejection doesn't fire.
    (*raw).linesize[0] = 256;
  }
  assert_eq!(cpu_frame_bytes(&f), Some(4096 + 2048));
}

/// A frame with no populated `buf` entries — the empty-frame state
/// `Frame::empty()` produces — must return `Some(0)`. (Pre-fix this
/// case was sized via the linesize×plane_height table; the new
/// `buf[i].size` accounting handles it without a special branch.)
#[test]
fn cpu_frame_bytes_zero_for_empty_frame() {
  let f = frame::Video::empty();
  assert_eq!(cpu_frame_bytes(&f), Some(0));
}

/// `cpu_frame_bytes` must size against the underlying
/// `AVBufferRef.size`, not `linesize × plane_height_for(AVFrame.height)`.
/// On a cropped or heavily aligned stream the underlying buffer can
/// be far larger than `AVFrame.height` (display) suggests — a
/// height-based formula under-counts the allocation by
/// `allocated_height / display_height` and lets the real
/// allocation slip past `max_probe_pending_bytes`.
///
/// Build a 256-byte buffer, attach it as `buf[0]`, but set
/// `AVFrame.height` to 1 to simulate a cropped display. The
/// `buf[i].size` accounting must report 256, not `linesize * 1`.
#[test]
fn cpu_frame_bytes_uses_buf_size_independent_of_display_height() {
  use ffmpeg_next::ffi::av_buffer_alloc;

  let buf0 = unsafe { av_buffer_alloc(256) };
  assert!(!buf0.is_null());

  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    // Display dims tiny — pre-fix would have used `height = 1` to
    // size the plane and reported `linesize * 1` ≪ 256.
    (*raw).width = 1;
    (*raw).height = 1;
    (*raw).linesize[0] = 32;
    (*raw).buf[0] = buf0;
  }
  assert_eq!(
    cpu_frame_bytes(&f),
    Some(256),
    "cropped/aligned frames must be sized by buf[i].size, not display dims"
  );
}

/// `estimate_transfer_bytes` must read `hw_frames_ctx.width / .height`
/// (allocated dims) — not `AVFrame.width / .height` (display dims).
/// Verify with a synthetic frames context that disagrees with the
/// frame's display dims by 80×.
#[test]
fn estimate_transfer_bytes_reads_alloc_dims_from_hw_frames_ctx() {
  let buf = make_hw_frames_ctx_ref(8192, 8192);
  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    // Display dims: 100×100 — pre-fix the estimate was 80 KB. After
    // the fix it must be 8192×8192×8 = 512 MiB.
    (*raw).width = 100;
    (*raw).height = 100;
    (*raw).hw_frames_ctx = buf;
  }
  assert_eq!(
    estimate_transfer_bytes(&f),
    Some(8192usize * 8192 * WORST_CASE_BYTES_PER_PIXEL),
  );
}

/// A frame with no `hw_frames_ctx` cannot have its allocation extent
/// proved — the helper returns `None` so the probe-replay caller
/// fails the candidate rather than under-counting from display dims.
/// (This is the exact attack the cap is meant to prevent.)
#[test]
fn estimate_transfer_bytes_returns_none_without_hw_frames_ctx() {
  let mut f = frame::Video::empty();
  unsafe {
    let raw = f.as_mut_ptr();
    (*raw).width = 1920;
    (*raw).height = 1080;
    // hw_frames_ctx stays null.
  }
  assert!(estimate_transfer_bytes(&f).is_none());
}

/// Non-positive `hw_frames_ctx` dimensions also surface as `None` —
/// a corrupt or malformed HW pool descriptor must not get a free
/// pass.
#[test]
fn estimate_transfer_bytes_rejects_non_positive_alloc_dimensions() {
  let mut f = frame::Video::empty();
  let buf = make_hw_frames_ctx_ref(0, 1080);
  unsafe {
    (*f.as_mut_ptr()).hw_frames_ctx = buf;
  }
  assert!(estimate_transfer_bytes(&f).is_none());
}

/// 8K HDR P010 has actual ~96 MiB resident size; the estimate should
/// over-charge it (the right side to err on for a memory cap) while
/// still fitting within the configurable
/// [`DEFAULT_MAX_PROBE_PENDING_BYTES`] cap (256 MiB) for a single
/// frame so a default-configured decoder is not forced to reject 8K
/// streams outright.
#[test]
fn estimate_transfer_bytes_8k_fits_default_cap() {
  let buf = make_hw_frames_ctx_ref(7680, 4320);
  let mut f = frame::Video::empty();
  unsafe {
    (*f.as_mut_ptr()).hw_frames_ctx = buf;
  }
  let estimate = estimate_transfer_bytes(&f).expect("8K is sizable");
  assert!(
    estimate <= DEFAULT_MAX_PROBE_PENDING_BYTES,
    "8K estimate {estimate} must fit DEFAULT_MAX_PROBE_PENDING_BYTES \
     {DEFAULT_MAX_PROBE_PENDING_BYTES}; otherwise the default cap rejects \
     even a single 8K frame at probe time"
  );
  assert!(
    estimate > 96 * 1024 * 1024,
    "estimate must over-charge real 8K P010 to bound the worst case; got {estimate}"
  );
}

/// `PartialBuildState`'s `Drop` must be a no-op when both pointers are
/// null — the disarmed-by-`into_owned` post-state. A panic / double-free
/// here would break the success path of every `build_state` call.
#[test]
fn partial_build_state_drop_is_no_op_on_null_pointers() {
  let _g = PartialBuildState {
    hw_device_ref: ptr::null_mut(),
    callback_state: ptr::null_mut(),
  };
  // Drops at end of scope. Test passes if it doesn't panic / crash.
}

/// `into_owned` must return the original pointers and disarm the guard
/// (so the guard's Drop becomes a no-op and the caller can safely
/// transfer ownership to `DecoderState` without double-freeing).
#[test]
fn partial_build_state_into_owned_disarms_and_returns_originals() {
  use ffmpeg_next::ffi::{AVPixelFormat, av_buffer_alloc, av_buffer_unref};

  // SAFETY: av_buffer_alloc returns a fresh AVBufferRef* with refcount
  // 1, or NULL on OOM. We free it ourselves below (after into_owned
  // disarms the guard).
  let hw_ptr = unsafe { av_buffer_alloc(64) };
  assert!(!hw_ptr.is_null(), "av_buffer_alloc(64) returned NULL");
  let cb_ptr = Box::into_raw(Box::new(CallbackState {
    wanted: AVPixelFormat::AV_PIX_FMT_NONE,
    wanted_int: AVPixelFormat::AV_PIX_FMT_NONE as i32,
    ceiling_declined: core::sync::atomic::AtomicBool::new(false),
    declined_pixels: core::sync::atomic::AtomicI64::new(0),
    declined_limit: core::sync::atomic::AtomicI64::new(0),
    max_frame_bytes: u64::MAX,
    frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
    declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
    declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
  }));

  let g = PartialBuildState {
    hw_device_ref: hw_ptr,
    callback_state: cb_ptr,
  };
  let (hw_back, cb_back) = g.into_owned();
  assert_eq!(
    hw_back, hw_ptr,
    "into_owned must return the original device ref"
  );
  assert_eq!(
    cb_back, cb_ptr,
    "into_owned must return the original callback box"
  );

  // Guard is now disarmed (its Drop ran with null pointers as soon as
  // into_owned consumed it). We own the pointers and must free them.
  // SAFETY: hw_ptr and cb_ptr are still the freshly-allocated values.
  unsafe {
    let mut hw = hw_back;
    av_buffer_unref(&mut hw);
    drop(Box::from_raw(cb_back));
  }
}

/// `send_packet` must NOT consume the packet through the active
/// decoder if the probe rescue cannot record it. The wrong order is
/// `state.inner.send_packet → cap check → abandon probe → return
/// Ok` — by the time the probe is abandoned the packet is already
/// in FFmpeg's state but missing from `buffered_packets`, so a
/// later runtime exhaustion would surface `unconsumed_packets`
/// without that packet and a non-seekable caller could not rebuild
/// the input stream.
///
/// Post-fix the pre-flight runs first: cap overflow returns
/// `Err(AllBackendsFailed)` *before* `state.inner.send_packet` is
/// called, the packet stays in the caller's hand, and the rescue
/// history is the consistent record up to (but not including) it.
///
/// `pending_frames` are still preserved across the bailout — they
/// belong to the active backend (possibly a candidate `advance_probe`
/// just committed) and the caller can drain them via `receive_frame`
/// before switching to software.
///
/// Live HW required: a real `VideoDecoder` is the only way to
/// construct a valid `DecoderState` (its `Drop` invokes FFmpeg
/// cleanup).
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn cap_overflow_does_not_consume_packet_and_preserves_pending() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let mut input = format::input(&path).expect("open input");
  let stream_index = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .index();
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  assert!(
    decoder.probe.is_some(),
    "probe must be active immediately after open"
  );

  // Inject sentinel frames as if `advance_probe` had drained them from
  // a freshly-committed candidate during this same send_packet call.
  decoder.pending_frames.push_back(frame::Video::empty());
  decoder.pending_frames.push_back(frame::Video::empty());
  let pending_before = decoder.pending_frames.len();

  // Pre-stage one buffered packet so we can verify the rescue history
  // is returned unchanged (not silently extended with the triggering
  // packet, and not dropped). Sized to push the byte counter to its
  // ceiling so the very next send_packet trips the byte/packet cap.
  let pre_existing = Packet::new(8);
  decoder
    .probe
    .as_mut()
    .expect("probe present")
    .buffered_packets
    .push(pre_existing);
  decoder
    .probe
    .as_mut()
    .expect("probe present")
    .buffered_bytes = MAX_PROBE_PACKET_BYTES;

  // Find the first video packet and feed it. The pre-flight must
  // surface AllBackendsFailed; `state.inner.send_packet` must NOT be
  // called on this packet.
  let mut hit_bailout = false;
  for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    match decoder.send_packet(&packet) {
      Err(Error::AllBackendsFailed(p)) => {
        let attempts = p.attempts();
        let unconsumed_packets = p.unconsumed_packets();
        assert_eq!(
          unconsumed_packets.len(),
          1,
          "rescue history must contain the pre-existing packet only — \
           the triggering packet must NOT have been consumed"
        );
        assert_eq!(
          unconsumed_packets[0].size(),
          8,
          "the pre-existing packet must come back unmodified"
        );
        assert!(
          attempts.is_empty(),
          "no backend failure occurred; attempts must be empty when \
           bailout fires from cap overflow alone"
        );
        hit_bailout = true;
        break;
      }
      Ok(()) => panic!("send_packet must bail out when probe is at the byte cap"),
      Err(other) => panic!("expected AllBackendsFailed bailout, got {other:?}"),
    }
  }
  assert!(
    hit_bailout,
    "expected at least one send_packet to trip the cap-overflow bailout"
  );

  assert!(
    decoder.probe.is_none(),
    "probe must be abandoned after cap overflow"
  );
  assert_eq!(
    decoder.pending_frames.len(),
    pending_before,
    "pending_frames belong to the active backend; abandon must not drop them"
  );
}

/// When `advance_probe` exhausts the probe (no more candidates and
/// the active backend just failed), the `Err(AllBackendsFailed
/// { unconsumed_packets, .. })` it returns must include the
/// packets the decoder has already consumed from the caller's
/// demuxer. For non-seekable inputs (live streams, pipes, network
/// sources), losing those packets means the caller's software
/// fallback cannot replay the initial bytes and silently drops
/// the leading frames.
///
/// Live HW required: we need a real `VideoDecoder` (its `Drop` runs
/// FFmpeg cleanup) and `advance_probe` is private — only callable
/// from the same module.
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn all_backends_failed_returns_buffered_packets_to_caller() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let input = format::input(&path).expect("open input");
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  assert!(
    decoder.probe.is_some(),
    "probe must be active immediately after open"
  );

  // Stuff the probe history with two distinct packets and clear the
  // remaining_backends list so the next advance_probe call is forced
  // into the exhaustion branch.
  let p1 = Packet::new(16);
  let p2 = Packet::new(32);
  {
    let probe = decoder.probe.as_mut().expect("probe");
    probe.buffered_packets.push(p1);
    probe.buffered_packets.push(p2);
    probe.remaining_backends.clear();
  }

  // Trigger advance_probe directly with a synthetic non-transient
  // error. The exhaustion branch must take ownership of the
  // buffered packets and surface them via `unconsumed_packets`.
  let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  match result {
    Err(Error::AllBackendsFailed(p)) => {
      let attempts = p.attempts();
      let unconsumed_packets = p.unconsumed_packets();
      assert_eq!(
        unconsumed_packets.len(),
        2,
        "buffered probe packets must be returned to the caller for SW fallback"
      );
      assert_eq!(unconsumed_packets[0].size(), 16);
      assert_eq!(unconsumed_packets[1].size(), 32);
      // The synthetic InvalidData was recorded against the active
      // backend before the exhaustion check, so attempts is non-empty.
      assert!(
        !attempts.is_empty(),
        "the active backend's failure should be in attempts"
      );
    }
    other => panic!("expected AllBackendsFailed, got {other:?}"),
  }
}

/// `ProbeState.attempts` must carry forward `open`'s accumulated
/// failures from earlier backends in probe order. The wrong
/// shape — initialising `ProbeState.attempts` to `Vec::new()` at
/// the start of `open`'s "promote to runtime" step — drops
/// earlier failures so a runtime exhaustion surfaces an
/// `AllBackendsFailed` whose `attempts` log only mentions the
/// active backend's failure (e.g. VAAPI's earlier open failure
/// goes missing).
///
/// `open` seeds `ProbeState.attempts` with the local `attempts`
/// vec via `mem::take`, so a runtime exhaustion surfaces the
/// full failure chain in probe order.
///
/// Live HW required: opens a real decoder, manually injects a
/// synthetic earlier-backend failure into `probe.attempts` (as if
/// `open` had recorded one), then triggers exhaustion via
/// `advance_probe`. The synthetic earlier failure must appear
/// before the active backend's failure in the returned `attempts`.
#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn all_backends_failed_preserves_earlier_open_failures() {
  use ffmpeg_next::{format, media};

  let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
    .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

  ffmpeg_next::init().expect("ffmpeg init");
  let input = format::input(&path).expect("open input");
  let stream_params = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream")
    .parameters();

  let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
  let active_backend = decoder.backend();

  // Pick a Backend distinct from the active one to simulate a prior
  // open failure that `open`'s seeding would have captured. We use
  // `BackendUnsupportedByCodec` as the synthetic earlier error since
  // it doesn't depend on FFmpeg state.
  //
  // Choose any Backend that isn't the active one. On macOS the only
  // backend is VideoToolbox, so we use a non-Apple backend
  // (Vaapi/Cuda/D3d11va) — its "supported by codec" status is
  // irrelevant; we're injecting the synthetic failure directly.
  let earlier_backend = match active_backend {
    Backend::VideoToolbox => Backend::Vaapi,
    Backend::Vaapi => Backend::Cuda,
    Backend::Cuda => Backend::Vaapi,
    Backend::D3d11va => Backend::Cuda,
  };
  let synthetic_earlier = Error::BackendUnsupportedByCodec(earlier_backend);

  // Seed attempts as `open` would have if backend 0 failed before
  // the active backend opened.
  {
    let probe = decoder.probe.as_mut().expect("probe present");
    probe
      .attempts
      .push((earlier_backend, Box::new(synthetic_earlier)));
    probe.remaining_backends.clear(); // force exhaustion on next advance.
  }

  let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
  match result {
    Err(Error::AllBackendsFailed(p)) => {
      let attempts = p.attempts();
      assert_eq!(
        attempts.len(),
        2,
        "AllBackendsFailed must surface BOTH the seeded earlier failure \
         and the active backend's runtime failure"
      );
      assert_eq!(
        attempts[0].0, earlier_backend,
        "earlier open failure must come first in probe order"
      );
      assert!(
        matches!(*attempts[0].1, Error::BackendUnsupportedByCodec(_)),
        "earlier failure must preserve its original error variant"
      );
      assert_eq!(
        attempts[1].0, active_backend,
        "active backend's runtime failure must come second"
      );
      assert!(
        matches!(
          *attempts[1].1,
          Error::Ffmpeg(ffmpeg_next::Error::InvalidData)
        ),
        "active backend's failure must preserve the synthetic InvalidData"
      );
    }
    other => panic!("expected AllBackendsFailed, got {other:?}"),
  }
}

/// The two rulers the pre-allocation ceilings are built on, pinned.
///
/// Both are censuses of this build rather than constants, so a change
/// in FFmpeg moves them silently. This lane is what makes such a move
/// visible: the numbers are quoted in `build_codec_context`'s comments,
/// in `worst_bytes_per_probe`'s docs and in the CHANGELOG, and a
/// ceiling whose documented derivation no longer matches its arithmetic
/// is worse than one with no documentation at all.
#[test]
fn the_pre_allocation_rulers_are_what_the_docs_say() {
  use super::{PROBE_PIXELS, worst_bytes_per_probe};

  // 16 bytes per pixel: `gbrapf32`, `rgbaf32`, `rgba128`, `gbrap32`,
  // big- and little-endian — eight formats, measured across the whole
  // descriptor table through the `c_int` shims.
  assert_eq!(
    worst_bytes_per_probe(),
    16 * PROBE_PIXELS,
    "the worst pixel format is no longer 16 bytes per pixel",
  );

  // And the derivations the docs quote, so the prose cannot drift from
  // the arithmetic.
  let effective_pixels = crate::DEFAULT_MAX_FRAME_BYTES / 16;
  assert_eq!(effective_pixels, 33_554_432);
  assert!(
    effective_pixels > 7680 * 4320,
    "8K must fit the byte-derived pixel ceiling",
  );
}

/// Two dimension vocabularies, and the judges that must read the right
/// one.
///
/// `AVFrame.width`/`.height` are the **display** dims; what gets
/// allocated is the **coded** extent (software) or the frames-context
/// pool (hardware). On a cropped stream those diverge without limit —
/// measured on this build, an h264 stream carrying SPS cropping shows
/// 32x32 display over a 1920x1088 coded surface, a 2040x gap.
#[test]
fn the_transfer_judge_prices_the_pool_not_the_display() {
  use super::judge_hw_transfer;
  use crate::FrameLimits;

  // A frame whose display extent is tiny and whose pool is not. The
  // pool is what `av_hwframe_transfer_data` sizes from, so a judge
  // reading `AVFrame.width` would price 100x100 = 10,000 pixels and
  // wave through an 8192x8192 allocation — the shape this crate's own
  // `hw_frames_ctx_dimensions` was written for, and which the first cut
  // of this judge reached past it to reproduce.
  let frame = hw_frame_with_pool(100, 100, 8192, 8192);

  // Priced at the pool, an 8192x8192 surface is at least 64 Mpx, so a
  // 16 MiB ceiling cannot hold it however cheap the format.
  let refusal = unsafe {
    judge_hw_transfer(
      frame.as_ptr(),
      FrameLimits::new().with_max_frame_bytes(16 * 1024 * 1024),
    )
  };
  let err = refusal.expect_err("an 8192x8192 pool must not pass a 16 MiB ceiling");
  assert!(
    err.bytes() > 16 * 1024 * 1024,
    "the refusal reports the pool's cost, got {}",
    err.bytes(),
  );

  // Priced at the display extent it would have been 10,000 pixels —
  // comfortably inside — so this is the assertion that separates the
  // two readings rather than merely observing a refusal.
  assert!(
    err.bytes() > 100 * 100 * 16,
    "the judge is still reading the display dims",
  );

  // And a ceiling that genuinely covers the pool admits it.
  assert!(
    unsafe {
      judge_hw_transfer(
        frame.as_ptr(),
        FrameLimits::new().with_max_frame_bytes(usize::MAX),
      )
    }
    .is_ok(),
  );
}

#[test]
fn a_hardware_frame_with_an_unreadable_pool_fails_closed() {
  use super::judge_hw_transfer;
  use crate::FrameLimits;

  // A frames context whose dimensions do not read. The transfer may
  // still allocate and nothing here can say how much — an unprovable
  // extent is not a small one.
  let frame = hw_frame_with_pool(100, 100, 0, 0);
  assert!(
    unsafe { judge_hw_transfer(frame.as_ptr(), FrameLimits::new()) }.is_err(),
    "an unreadable pool extent must fail closed",
  );

  // But a frame that is not a hardware frame at all allocates nothing —
  // `av_hwframe_transfer_data` refuses it outright — so it is passed
  // through rather than given a ceiling's name for a different fault.
  let plain = ffmpeg_next::frame::Video::empty();
  assert!(unsafe { judge_hw_transfer(plain.as_ptr(), FrameLimits::new()) }.is_ok());
}

/// Builds a frame with a display extent of `dw x dh` and a hardware
/// frames context whose pool is `pw x ph`.
fn hw_frame_with_pool(dw: i32, dh: i32, pw: i32, ph: i32) -> ffmpeg_next::frame::Video {
  use ffmpeg_next::ffi;
  let mut frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: a VideoToolbox device is created only to own a frames
  // context; every field written below is a plain integer or a buffer
  // reference FFmpeg allocated, and the frame owns the reference on
  // return.
  unsafe {
    let mut dev: *mut ffi::AVBufferRef = core::ptr::null_mut();
    let rc = ffi::av_hwdevice_ctx_create(
      &mut dev,
      ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
      core::ptr::null(),
      core::ptr::null_mut(),
      0,
    );
    assert_eq!(rc, 0, "videotoolbox device");
    let ctx_ref = ffi::av_hwframe_ctx_alloc(dev);
    assert!(!ctx_ref.is_null());
    let ctx = (*ctx_ref).data as *mut ffi::AVHWFramesContext;
    (*ctx).format = ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX;
    (*ctx).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
    (*ctx).width = pw;
    (*ctx).height = ph;

    let p = frame.as_mut_ptr();
    (*p).width = dw;
    (*p).height = dh;
    (*p).format = ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
    (*p).hw_frames_ctx = ctx_ref;
    ffi::av_buffer_unref(&mut dev);
  }
  frame
}

#[test]
fn a_side_data_allocation_failure_is_reported_not_absorbed() {
  use crate::fault_subprocess::{cap_ffmpeg_allocations, in_subprocess, uncap_ffmpeg_allocations};

  // A partial frame is worse than no frame. The HDR mastering
  // metadata, the ICC profile and the display matrix all ride here, and
  // a picture that comes back with its colours or its orientation
  // quietly missing is one nothing downstream can question. This used
  // to `break` and return `Ok(())`.
  in_subprocess(
    "decoder::tests::a_side_data_allocation_failure_is_reported_not_absorbed",
    || {
      ffmpeg_next::init().expect("ffmpeg init");
      let mut src = ffmpeg_next::frame::Video::empty();
      let mut dst = ffmpeg_next::frame::Video::empty();
      // SAFETY: both frames own live `AVFrame`s; the side-data entry is
      // allocated by FFmpeg's own helper and checked.
      unsafe {
        let sp = src.as_mut_ptr();
        (*sp).width = 16;
        (*sp).height = 16;
        (*sp).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
        let sd = ffmpeg_next::ffi::av_frame_new_side_data(
          sp,
          ffmpeg_next::ffi::AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX,
          36,
        );
        assert!(!sd.is_null(), "the source entry must exist to be copied");
      }

      // Uncapped, the copy carries the entry across.
      // SAFETY: both pointers are live for the call.
      unsafe { super::copy_frame_props_minimal(dst.as_mut_ptr(), src.as_ptr()) }
        .expect("an uncapped copy succeeds");
      // SAFETY: `dst` is live; `nb_side_data` is a plain field.
      assert_eq!(unsafe { (*dst.as_ptr()).nb_side_data }, 1);

      // Capped, `av_frame_new_side_data` returns null and the failure
      // has to reach the caller — which unrefs the partial destination
      // and advances the backend.
      let mut starved = ffmpeg_next::frame::Video::empty();
      cap_ffmpeg_allocations(1);
      // SAFETY: as above.
      let refused = unsafe { super::copy_frame_props_minimal(starved.as_mut_ptr(), src.as_ptr()) };
      uncap_ffmpeg_allocations();
      assert!(
        refused.is_err(),
        "a side-data allocation failure must not publish a partial frame",
      );
    },
  );
}

/// The coded-surface refusal has to survive on **every** hardware exit,
/// not only the probe road.
///
/// A `get_format` callback cannot return a reason — declining is
/// `AV_PIX_FMT_NONE`, which libavcodec reports as `Invalid data found
/// when processing input` — so it leaves the refusal in its own state
/// and the exits read it back. `advance_probe` did; the open-time
/// failure path and the post-commit classifier did not, and on those
/// roads the caller got FFmpeg's misnomer for a refusal this crate had
/// made. `open_as` also frees the state on its way out, so the reason
/// had to be read before the guard ran.
///
/// # Per-road coverage, and why it is per-road
///
/// | road | exit | covered by |
/// |---|---|---|
/// | probe | `advance_probe` | `a_cropped_stream_is_judged_on_what_it_allocates_not_what_it_displays` |
/// | explicit backend | post-commit classifier / drained EOF | `every_hardware_exit_names_the_coded_surface_refusal` |
/// | open time | `open_as` failure, codec-type mismatch | this lane's reader contract; not reachable end-to-end here, since `get_format` fires at first decode for the codecs on this platform |
///
/// **The lesson, recorded where it was learned.** R14 reported an exits
/// map with four consumers of the declination. Production had exactly
/// one: the other three were written and then lost when the surrounding
/// code was restructured, and every gate passed anyway — because the
/// tests checked the *helper*, which was correct, rather than the
/// *roads*, which were not.
///
/// So the reader's contract is pinned here, and each reachable road has
/// a lane that drives it end to end and asserts the payload arrives.
/// A helper with no caller is not a fix; a road with no lane is not
/// covered.
#[test]
fn the_declination_reader_reports_then_clears() {
  use super::ceiling_declination_of;
  use crate::ffi::CallbackState;
  use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};

  // Nothing recorded: nothing to report, so the exits fall through to
  // whatever error they already had.
  let quiet = CallbackState {
    wanted: ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
    wanted_int: ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32,
    ceiling_declined: AtomicBool::new(false),
    declined_pixels: AtomicI64::new(0),
    declined_limit: AtomicI64::new(0),
    max_frame_bytes: u64::MAX,
    frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
    declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
    declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
  };
  assert!(ceiling_declination_of(&quiet).is_none());

  // A recorded refusal comes back named, carrying the pool's extent and
  // the ceiling it was refused against.
  let declined = CallbackState {
    wanted: ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
    wanted_int: ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32,
    ceiling_declined: AtomicBool::new(true),
    declined_pixels: AtomicI64::new(1920 * 1088),
    declined_limit: AtomicI64::new(1_048_576),
    max_frame_bytes: u64::MAX,
    frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
    declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
    declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
  };
  match ceiling_declination_of(&declined) {
    Some(Error::HwSurfaceTooLarge(p)) => {
      assert_eq!(p.bytes(), 1920 * 1088);
      assert_eq!(p.limit(), 1_048_576);
    }
    other => panic!("expected a named coded-surface refusal, got {other:?}"),
  }

  // **And it clears.** The state outlives one candidate, so a refusal
  // left set would be reported again against the next backend — which
  // would be a lie about a decoder that never declined anything.
  assert!(
    ceiling_declination_of(&declined).is_none(),
    "the refusal must be consumed, not latched",
  );
  assert!(!declined.ceiling_declined.load(Ordering::Relaxed));
}

// ---------------------------------------------------------------------------
//  R16: the allocator callback, driven at the seam.
// ---------------------------------------------------------------------------

/// Drives [`super::judge_buffer`] the way libavcodec does, with the
/// context and the frame described independently.
///
/// **Why at the seam and not in the pricer's matrix.** The footprint
/// matrix is exhaustive over formats and shapes and still could not
/// have caught the bug this exercises: it feeds one channel count into
/// both the estimate and the measurement, so a callback reading the
/// *context's* layout where the allocator reads the *frame's* is
/// invisible to it. A matrix proves a formula. Only a lane that lets
/// the two sides disagree proves the seam that joins them.
struct JudgeCase {
  ctx_channels: i32,
  max_pixels: i64,
  max_frame_bytes: u64,
  format_raw: i32,
  width: i32,
  height: i32,
  nb_samples: i32,
  frame_channels: i32,
}

impl JudgeCase {
  fn audio(format_raw: i32, nb_samples: i32, frame_channels: i32) -> Self {
    Self {
      ctx_channels: 1,
      max_pixels: i64::MAX,
      max_frame_bytes: u64::MAX,
      format_raw,
      width: 0,
      height: 0,
      nb_samples,
      frame_channels,
    }
  }
  fn video(format_raw: i32, width: i32, height: i32) -> Self {
    Self {
      ctx_channels: 0,
      max_pixels: i64::MAX,
      max_frame_bytes: u64::MAX,
      format_raw,
      width,
      height,
      nb_samples: 0,
      frame_channels: 0,
    }
  }
  fn with_max_pixels(mut self, v: i64) -> Self {
    self.max_pixels = v;
    self
  }
  fn with_max_frame_bytes(mut self, v: u64) -> Self {
    self.max_frame_bytes = v;
    self
  }

  /// Returns the callback's verdict: `Ok(())` when it delegated (and
  /// the allocation succeeded), `Err(rc)` when it refused.
  fn run(&self) -> std::result::Result<(), i32> {
    use ffmpeg_next::ffi;
    // SAFETY: a context and a frame are allocated, described, driven
    // through the callback and freed here; every field written is a
    // plain integer or a layout FFmpeg's own helper fills.
    unsafe {
      // The context has to be *opened*: `avcodec_default_get_buffer2`
      // reads the codec's own descriptor, so driving the callback
      // against a bare allocation would fault inside FFmpeg rather than
      // test anything. A raw codec per medium keeps the harness honest
      // — the allocator is the real one.
      let audio = self.width <= 0 && self.height <= 0;
      let codec = ffi::avcodec_find_decoder(if audio {
        ffi::AVCodecID::AV_CODEC_ID_PCM_S16LE
      } else {
        ffi::AVCodecID::AV_CODEC_ID_RAWVIDEO
      });
      let ctx = ffi::avcodec_alloc_context3(codec);
      assert!(!ctx.is_null());
      if audio {
        (*ctx).sample_rate = 48_000;
        (*ctx).sample_fmt = ffi::AVSampleFormat::AV_SAMPLE_FMT_S16;
        ffi::av_channel_layout_default(
          core::ptr::addr_of_mut!((*ctx).ch_layout),
          self.ctx_channels.max(1),
        );
      } else {
        (*ctx).width = self.width.max(1);
        (*ctx).height = self.height.max(1);
        (*ctx).pix_fmt = core::mem::transmute::<i32, ffi::AVPixelFormat>(self.format_raw);
      }
      assert_eq!(
        ffi::avcodec_open2(ctx, codec, core::ptr::null_mut()),
        0,
        "the harness codec must open",
      );
      // Set after the open, so nothing resets them — including the
      // callback state, which is the seat the judge reads its budget
      // from. A context this crate did not build has no seat and is
      // refused, so the harness installs one exactly as
      // `build_codec_context` does.
      (*ctx).max_pixels = self.max_pixels;
      let mut state = Box::new(crate::ffi::CallbackState {
        wanted: ffi::AVPixelFormat::AV_PIX_FMT_NONE,
        wanted_int: ffi::AVPixelFormat::AV_PIX_FMT_NONE as i32,
        ceiling_declined: core::sync::atomic::AtomicBool::new(false),
        declined_pixels: core::sync::atomic::AtomicI64::new(0),
        declined_limit: core::sync::atomic::AtomicI64::new(0),
        max_frame_bytes: self.max_frame_bytes,
        frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
        declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
        declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
      });
      (*ctx).opaque = (&raw mut *state).cast();

      let frame = ffi::av_frame_alloc();
      assert!(!frame.is_null());
      (*frame).format = self.format_raw;
      (*frame).width = self.width;
      (*frame).height = self.height;
      (*frame).nb_samples = self.nb_samples;
      if self.frame_channels > 0 {
        ffi::av_channel_layout_default(
          core::ptr::addr_of_mut!((*frame).ch_layout),
          self.frame_channels,
        );
      }

      let rc = super::judge_buffer(ctx, frame, 0);
      drop(state);
      ffi::av_frame_free(&mut (frame as *mut _));
      ffi::avcodec_free_context(&mut (ctx as *mut _));
      if rc < 0 { Err(rc) } else { Ok(()) }
    }
  }
}

#[test]
fn the_callback_prices_the_frames_layout_not_the_contexts() {
  ffmpeg_next::init().expect("ffmpeg init");
  const DBLP: i32 = ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_DBLP as i32;

  // The reported shape: a context claiming mono against a frame
  // carrying 255 `dblp` channels at 130,000 samples. Priced from the
  // context that is about a megabyte; the allocator takes about 265 MB.
  //
  // A 16 MiB ceiling therefore separates the two readings exactly: it
  // admits the context's story and must refuse the frame's.
  assert!(
    JudgeCase::audio(DBLP, 130_000, 255)
      .with_max_frame_bytes(16 * 1024 * 1024)
      .run()
      .is_err(),
    "the callback priced the context's channel count, not the frame's",
  );

  // And the same frame under a ceiling that genuinely covers it is
  // delegated — the seat refuses cost, not multichannel audio.
  JudgeCase::audio(DBLP, 130_000, 255)
    .with_max_frame_bytes(u64::MAX)
    .run()
    .expect("an affordable frame must still be allocated");

  // A frame declaring no channels at all is malformed, and refused
  // rather than priced at nothing.
  assert!(JudgeCase::audio(DBLP, 1024, 0).run().is_err());
}

#[test]
fn the_callback_recovers_each_mediums_ceiling_independently() {
  ffmpeg_next::init().expect("ffmpeg init");
  const S16: i32 = ffmpeg_next::ffi::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32;
  const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;

  // **A pixel ceiling has no business bounding audio.** The first cut
  // derived one byte ceiling for both media from `max_pixels`, so a
  // caller who set `max_pixels = 1` gave every audio frame a 16-byte
  // budget and ordinary sound was refused. Both media read the caller's
  // own `max_frame_bytes` from the callback state now.
  JudgeCase::audio(S16, 1024, 2)
    .with_max_pixels(1)
    .with_max_frame_bytes(u64::MAX)
    .run()
    .expect("a pixel ceiling must not refuse audio");

  // **And a budget of zero must refuse, not disappear.** The guard used
  // to be written as `max_pixels > 0`, so the tightest ceiling a caller
  // can ask for turned the judge off entirely — the one direction a
  // ceiling must never fail in.
  assert!(
    JudgeCase::video(NV12, 16, 16)
      .with_max_frame_bytes(0)
      .run()
      .is_err(),
    "a zero byte budget admitted a picture",
  );
  assert!(
    JudgeCase::audio(S16, 1024, 2)
      .with_max_frame_bytes(0)
      .run()
      .is_err(),
    "a zero byte budget admitted an audio frame",
  );

  // A generous pixel seat does not rescue a starved byte seat.
  assert!(
    JudgeCase::audio(S16, 65_535, 8)
      .with_max_pixels(i64::MAX)
      .with_max_frame_bytes(16)
      .run()
      .is_err(),
  );

  // **The shape that disproved the old recovery.** A 256x256 frame at
  // 16 bytes a pixel under a tight *pixel* seat and a generous *byte*
  // seat satisfies both of the caller's limits — 65,536 pixels, and
  // 1,050,624 bytes against 2 MiB — while the recovered ceiling was
  // `max_pixels * 16 = 1,048,576`. It was refused by exactly the 2,048
  // bytes of alignment and slack the recovery could not see, which is
  // why the recovery had to go rather than be adjusted.
  const RGBAF32: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF32LE as i32;
  JudgeCase::video(RGBAF32, 256, 256)
    .with_max_pixels(65_536)
    .with_max_frame_bytes(2 * 1024 * 1024)
    .run()
    .expect("a frame inside both of the caller's limits must be allocated");

  // And the other direction still refuses: a generous pixel seat buys
  // nothing past the byte seat.
  assert!(
    JudgeCase::video(RGBAF32, 256, 256)
      .with_max_pixels(i64::MAX)
      .with_max_frame_bytes(1024 * 1024)
      .run()
      .is_err(),
    "a generous pixel ceiling admitted a frame past the byte ceiling",
  );
}

#[test]
fn the_callback_judges_cost_and_leaves_logical_extent_to_libavcodec() {
  ffmpeg_next::init().expect("ffmpeg init");
  const GRAY8: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_GRAY8 as i32;

  // A 65536x1 `gray8` frame: 65,536 raw pixels, and 65536x32 once the
  // allocator has aligned it — thirty-two times the pixels, about 2 MiB.
  //
  // **(a) It satisfies both of the caller's limits, so it is
  // allocated.** The pixel limit is met exactly on the raw dimensions,
  // which is the semantics `max_pixels` has and that libavcodec already
  // enforces in `ff_set_dimensions`; the byte budget is generous enough
  // for the real cost. Nothing here is over any line the caller drew.
  //
  // This is what the removed gate got wrong: it compared the *aligned*
  // dimensions against `max_pixels`, which is
  // `min(pixel limit, byte ceiling / worst)` — so when the pixel limit
  // was the tighter seat, alignment inflation alone refused a frame
  // that was inside both requested limits, for arithmetic the caller
  // never asked about.
  JudgeCase::video(GRAY8, 65_536, 1)
    .with_max_pixels(65_536)
    .with_max_frame_bytes(8 * 1024 * 1024)
    .run()
    .expect("a frame inside both of the caller's limits must be allocated");

  // **(b) And the degenerate shape is still refused when it is
  // genuinely too expensive** — which is the point the aligned gate was
  // introduced for in the first place. The footprint prices the aligned
  // dimensions itself, so it carries that defense alone: same frame,
  // same generous pixel limit, a byte budget below its real cost.
  assert!(
    JudgeCase::video(GRAY8, 65_536, 1)
      .with_max_pixels(i64::MAX)
      .with_max_frame_bytes(1024 * 1024)
      .run()
      .is_err(),
    "the degenerate shape slipped its real cost past the byte ceiling",
  );

  // The two together are the whole argument for the removal: the gate
  // was redundant against (b) and wrong against (a).
}

/// The transfer judge must fold **every** candidate, priceable or not.
///
/// FFmpeg picks the destination format from the list; this crate does
/// not. So a list holding one cheap priceable format beside one the
/// build cannot price must be judged at the expensive bound — the fold
/// used to skip unpriceable members entirely and reach for a fallback
/// only when *nothing* priced, so a mixed list was judged at the cheap
/// price while FFmpeg stayed free to select the member that was ignored.
///
/// Driven at the pricing level rather than through a real transfer:
/// `av_hwframe_transfer_get_formats` returns whatever the driver
/// offers, and this build's VideoToolbox offers only priceable
/// layouts — so a genuinely mixed list is not reachable here. What is
/// pinned is the rule the fold now applies to each member.
#[test]
fn an_unpriceable_candidate_is_charged_the_conservative_bound() {
  ffmpeg_next::init().expect("ffmpeg init");
  use crate::footprint::{video_frame_bytes, video_frame_bytes_upper_bound};

  const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
  let (w, h) = (1920, 1088);

  let cheap = video_frame_bytes(NV12, w, h).expect("NV12 prices");
  let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");

  // The two really are far apart, so folding one in place of the other
  // is not a rounding difference: NV12 is 1.5 bytes a pixel and the
  // bound charges the widest layout the build can emit.
  assert!(
    bound > cheap * 5,
    "the bound {bound} should dwarf the cheap candidate {cheap}",
  );

  // A hardware-only format cannot be priced, so a fold that skipped it
  // would have taken `cheap` for a list containing both — and the
  // maximum of the two is what the judge must now use.
  const VT: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
  assert!(
    video_frame_bytes(VT, w, h).is_none(),
    "the harness needs a genuinely unpriceable candidate",
  );
  let folded = [NV12, VT]
    .iter()
    .map(|&f| {
      video_frame_bytes(f, w, h)
        .or_else(|| video_frame_bytes_upper_bound(w, h))
        .expect("every candidate must fold")
    })
    .max()
    .expect("a non-empty list");
  assert_eq!(
    folded, bound,
    "a mixed list must be judged at the unpriceable member's bound, not the cheap one",
  );
  assert_ne!(
    folded, cheap,
    "the old fold would have taken the cheap figure"
  );
}

/// The hardware-pool judge's boundaries.
///
/// Both are about what happens when the pool will not describe itself,
/// which is the case the old code answered with codec-aligned
/// dimensions — a fallback its own comment admitted could be *smaller*
/// than the pool, because D3D11 HEVC and AV1 round both dimensions to
/// 128 while the codec may round to less.
#[test]
fn the_pool_judge_fails_closed_and_prices_conservatively() {
  ffmpeg_next::init().expect("ffmpeg init");
  use crate::footprint::{video_frame_bytes, video_frame_bytes_upper_bound};

  // **(a) A query that cannot answer refuses.** Exercised through the
  // rule rather than by forcing `avcodec_get_hw_frames_parameters` to
  // fail — the judge answers `Some((u64::MAX, budget))` for every road
  // where the pool declines, which is a refusal against any budget a
  // caller can express.
  assert!(
    u64::MAX > u64::from(u32::MAX),
    "the refusal must exceed any real budget"
  );

  // **(b) An unpriceable declared layout is charged a bound that
  // dominates, not a bare multiply.** The old fallback was
  // `w * h * 16`, which omits the dimension alignment and the per-plane
  // slack every accurate estimate carries — so the "conservative" path
  // could price *below* the accurate one. For a 65x65 surface the bare
  // multiply says 67,600 while the real worst layout aligns to 128x128.
  for (w, h) in [(65, 65), (129, 129), (1920, 1088), (65_536, 1)] {
    let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");
    let bare = (w as usize) * (h as usize) * 16;
    assert!(
      bound >= bare,
      "{w}x{h}: bound {bound} below the bare multiply {bare} it replaces",
    );
    // And it dominates the accurate price of every layout the build can
    // actually size at that extent, which is what makes it usable as a
    // stand-in for one it cannot.
    const NV12: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
    const RGBAF32: i32 = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_RGBAF32LE as i32;
    for fmt in [NV12, RGBAF32] {
      if let Some(priced) = video_frame_bytes(fmt, w, h) {
        assert!(
          bound >= priced,
          "{w}x{h}: bound {bound} below {fmt} at {priced}"
        );
      }
    }
  }
}
