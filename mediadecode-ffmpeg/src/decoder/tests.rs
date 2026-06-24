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
