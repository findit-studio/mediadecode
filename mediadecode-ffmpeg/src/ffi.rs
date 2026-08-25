//! FFI shims used by the decoder. Kept in one place so the unsafe surface is
//! easy to audit.
//!
//! All reads of `AVPixelFormat` / `AVHWDeviceType` values returned by FFmpeg
//! at runtime go through `ptr::read::<i32>` after a pointer cast, never
//! through the bindgen-generated Rust enum. The enums are `#[repr(i32)]`
//! and constructing them from a value not in the listed discriminants is
//! undefined behavior — exactly the situation header/library skew creates.
//! See the doc comments on individual functions for what is read as raw
//! integer vs. constructed from a known constant.

use std::{
  ffi::{c_char, c_int, c_uint},
  ptr,
};

use ffmpeg_next::ffi::{
  AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX, AVCodec, AVCodecContext, AVHWDeviceType, AVPacket,
  AVPacketSideDataType, AVPixelFormat, avcodec_get_hw_config,
};
use smol_str::SmolStr;

unsafe extern "C" {
  /// `av_get_pix_fmt_name`, redeclared with a plain `c_int` parameter
  /// instead of `AVPixelFormat`.
  ///
  /// The binding `ffmpeg-sys-next` generates takes the bindgen enum,
  /// and the whole point of calling this function is to name an integer
  /// we could *not* map — i.e. one that may well not be in our build's
  /// discriminant set. Constructing `AVPixelFormat` from such a value to
  /// pass it in would be immediate UB, the exact hazard this module
  /// exists to keep out of the crate. `AVPixelFormat` is `#[repr(i32)]`
  /// and C passes the enum as an `int`, so the redeclared signature is
  /// ABI-identical; only the Rust-side validity obligation is dropped.
  ///
  /// libavutil answers *every* integer, in range or not, with either a
  /// pointer into its static descriptor table or null — it bounds-checks
  /// before indexing. That is the property [`pix_fmt_name`] relies on,
  /// and `pix_fmt_name_refuses_what_ffmpeg_cannot_name` pins it against
  /// the linked library rather than assuming it.
  fn av_get_pix_fmt_name(pix_fmt: c_int) -> *const c_char;
}

unsafe extern "C" {
  /// `av_packet_new_side_data`, redeclared with a plain integer type
  /// parameter instead of `AVPacketSideDataType`.
  ///
  /// Same reason as [`av_get_pix_fmt_name`] above: the value being
  /// passed is a side-data type this crate carries as the raw integer
  /// it is on the wire, and constructing the bindgen enum from it would
  /// be undefined behaviour for any value outside this build's
  /// discriminant set. `AVPacketSideDataType` is `#[repr(u32)]` and C
  /// passes the enum in a register as an `unsigned int`, so the
  /// redeclared signature is ABI-identical; only the Rust-side validity
  /// obligation is dropped.
  ///
  /// Callers must still hand it a type this build names —
  /// [`packet_new_side_data`] enforces that — because FFmpeg's own code
  /// compares the field against its named constants.
  #[link_name = "av_packet_new_side_data"]
  fn av_packet_new_side_data_raw(pkt: *mut AVPacket, kind: c_uint, size: usize) -> *mut u8;
}

/// Attaches a side-data buffer of `size` bytes to `pkt`, returning a
/// pointer to it, or `None` when the type is not one this build of
/// FFmpeg names or the allocation failed.
///
/// The range check is what keeps the raw type integer honest:
/// `AVPacketSideDataType` is a contiguous enum whose last member,
/// `AV_PKT_DATA_NB`, is its count, so `0 <= kind < AV_PKT_DATA_NB` is
/// exactly the set this build understands. Nothing outside it is handed
/// to C.
///
/// # Safety
///
/// `pkt` must be a live `*mut AVPacket` for the duration of the call.
pub(crate) unsafe fn packet_new_side_data(
  pkt: *mut AVPacket,
  kind: i32,
  size: usize,
) -> Option<*mut u8> {
  if kind < 0 || kind >= side_data_type_count() {
    return None;
  }
  // SAFETY: the caller keeps `pkt` live, and `kind` was just proved to
  // be a type this build names.
  let ptr = unsafe { av_packet_new_side_data_raw(pkt, kind as c_uint, size) };
  (!ptr.is_null()).then_some(ptr)
}

/// How many side-data types this build of FFmpeg names.
#[cfg_attr(not(tarpaulin), inline(always))]
pub(crate) const fn side_data_type_count() -> i32 {
  AVPacketSideDataType::AV_PKT_DATA_NB as i32
}

/// Upper bound on the NUL search in [`pix_fmt_name`]. FFmpeg's longest
/// pixel-format name is around fourteen bytes, so this is generous; the
/// cap exists only so that a corrupt or version-skewed descriptor table
/// cannot turn the walk into an unbounded read, matching the discipline
/// the rest of the crate's FFI text handling follows.
const PIX_FMT_NAME_MAX_BYTES: usize = 64;

/// FFmpeg's own name for a raw `AVFrame.format` integer — `"yuv420p"`,
/// `"vaapi"` — or `None` when libavutil has no descriptor for it.
///
/// Diagnostic only. It never feeds a mapping decision: the raw integer
/// is turned into a [`mediadecode::PixelFormat`] by
/// [`crate::boundary::from_av_pixel_format`], which compares against
/// compile-time constants and is the authority. This function exists so
/// an error can say *which* format was refused when the vocabulary's
/// answer for it is `None`.
pub(crate) fn pix_fmt_name(raw: i32) -> Option<SmolStr> {
  // SAFETY: the redeclaration above takes a plain `c_int`, so no
  // `AVPixelFormat` is constructed from `raw` and no invalid enum value
  // is ever formed. libavutil bounds-checks the index itself and
  // returns null for anything outside its table, so every `i32` —
  // negative, `AV_PIX_FMT_NONE`, or past the end — is a defined call.
  let ptr = unsafe { av_get_pix_fmt_name(raw) };
  if ptr.is_null() {
    return None;
  }

  // A bounded NUL search rather than `CStr::from_ptr`: a missing
  // terminator violates that function's precondition outright, and
  // this crate does not hand FFmpeg's word on string lengths to a
  // function that cannot survive being wrong.
  for i in 0..PIX_FMT_NAME_MAX_BYTES {
    // SAFETY: `ptr` is non-null, and libavutil's format names are
    // string literals in its static `av_pix_fmt_descriptors` table —
    // NUL-terminated and valid for the process lifetime. We read at
    // most one byte past the last name byte.
    let byte = unsafe { *(ptr.add(i) as *const u8) };
    if byte == 0 {
      // SAFETY: the `i` bytes below the NUL were just walked, so the
      // slice is in bounds and initialized.
      let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, i) };
      return std::str::from_utf8(bytes).ok().map(SmolStr::new);
    }
  }
  None
}

/// State pointed to by `AVCodecContext::opaque` so [`get_hw_format`] can pick
/// the correct hardware pixel format without globals. One instance per
/// decoder; freed by [`crate::VideoDecoder`] after the codec context is
/// dropped.
///
/// `wanted` is set from a hardcoded `AVPixelFormat` constant in our bindings
/// (via `Backend::hw_pixel_format`), so it is always a valid enum value. We
/// also store its raw `i32` so the callback can compare against the offered
/// list without going through enum reads.
#[repr(C)]
pub(crate) struct CallbackState {
  /// Hardware pixel format we want the decoder to produce. Constructed
  /// from a known constant; safe to use as the callback's return value.
  pub(crate) wanted: AVPixelFormat,
  /// Same value as `wanted` cast to `i32`, cached so the callback's
  /// pix_fmts walk doesn't have to convert per iteration.
  pub(crate) wanted_int: i32,
  /// Set by [`get_hw_format`] when it declines this backend because the
  /// **coded** surface is over the frame ceiling.
  ///
  /// A `get_format` callback has no way to return a reason: declining
  /// means returning `AV_PIX_FMT_NONE`, which libavcodec reports as
  /// `Invalid data found when processing input`. That is a true
  /// statement about what libavcodec saw and a false one about what
  /// happened — the data was fine and this crate declined it — so the
  /// reason is left here instead, where the probe machinery can pick it
  /// up and give the caller the name the callback could not.
  pub(crate) ceiling_declined: core::sync::atomic::AtomicBool,
  /// The coded pixel count that was refused, for the message.
  pub(crate) declined_pixels: core::sync::atomic::AtomicI64,
  /// The ceiling it was refused against.
  pub(crate) declined_limit: core::sync::atomic::AtomicI64,
  /// The caller's [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// verbatim.
  ///
  /// **The single source of truth for the allocator judge.** It used to
  /// be recovered from `AVCodecContext.max_pixels`, which is set to
  /// `min(pixel ceiling, byte ceiling / worst-bytes-per-pixel)` — so
  /// when the *pixel* seat was the tighter of the two, `max_pixels`
  /// stopped encoding the byte ceiling at all and the recovery invented
  /// a smaller one. A 256x256 frame at 16 bytes a pixel under
  /// `max_pixels = 65536` and a 2 MiB byte budget satisfies both of the
  /// caller's limits and costs 1,050,624 bytes — and was judged against
  /// 1,048,576 and refused. The recovery also omitted the footprint's
  /// own alignment and slack, which is where those extra 2,048 bytes
  /// come from.
  ///
  /// Carried here instead, so the judge reads the number the caller
  /// actually set.
  pub(crate) max_frame_bytes: u64,
  /// Set by `judge_buffer` when it refuses a **software** allocation
  /// over [`Self::max_frame_bytes`].
  ///
  /// A `get_buffer2` callback can only answer with an errno, and
  /// `AVERROR(EINVAL)` is what libavcodec also reports for corrupt
  /// input — so a caller could not tell a budget refusal this crate
  /// made from a broken file. The reason is left here and collected by
  /// the decoder funnels, exactly as the `get_format` declination is.
  pub(crate) frame_budget_declined: core::sync::atomic::AtomicBool,
  /// What the refused frame would have cost.
  pub(crate) declined_frame_bytes: core::sync::atomic::AtomicU64,
  /// Whether the refused frame was audio (`true`) or a picture.
  pub(crate) declined_frame_audio: core::sync::atomic::AtomicBool,
}

/// Reads and clears a software frame-budget refusal, if one was left.
///
/// Clear-on-read for the same reason the `get_format` declination is: a
/// refusal that latched would be reported again against the next frame,
/// which never declined anything.
pub(crate) fn take_frame_budget_declination(
  state: *const CallbackState,
) -> Option<(u64, u64, bool)> {
  use core::sync::atomic::Ordering;
  if state.is_null() {
    return None;
  }
  // SAFETY: `state` is the live `CallbackState` the caller owns; it is
  // freed only after the codec context it belongs to.
  let (declined, bytes, limit, audio) = unsafe {
    (
      (*state)
        .frame_budget_declined
        .swap(false, Ordering::Acquire),
      (*state).declined_frame_bytes.load(Ordering::Relaxed),
      (*state).max_frame_bytes,
      (*state).declined_frame_audio.load(Ordering::Relaxed),
    )
  };
  declined.then_some((bytes, limit, audio))
}

/// `AVCodecContext::get_format` callback. FFmpeg invokes it with the list of
/// pixel formats the codec is willing to output for the current stream.
///
/// The offered list is walked as `*const i32` (cast from `*const AVPixelFormat`)
/// to avoid constructing the bindgen enum from values that may not be in our
/// build's discriminant set. The return value is either `wanted` (a known
/// constant) or `AV_PIX_FMT_NONE` (also a known constant) — both safe to
/// produce as `AVPixelFormat`.
pub(crate) unsafe extern "C" fn get_hw_format(
  ctx: *mut AVCodecContext,
  pix_fmts: *const AVPixelFormat,
) -> AVPixelFormat {
  debug_assert!(!ctx.is_null());
  debug_assert!(!pix_fmts.is_null());

  // SAFETY: opaque was set by `try_open` to a valid `Box<CallbackState>`
  // pointer that outlives the codec context (we only free it after the
  // codec context's drop runs). When opaque is null we treat the call as
  // strict — a stray invocation cannot silently downgrade.
  let state = unsafe { (*ctx).opaque as *const CallbackState };
  let (wanted, wanted_int) = if state.is_null() {
    (
      AVPixelFormat::AV_PIX_FMT_NONE,
      AVPixelFormat::AV_PIX_FMT_NONE as i32,
    )
  } else {
    unsafe { ((*state).wanted, (*state).wanted_int) }
  };

  // Walk the offered list as i32. The pointer cast is sound because
  // `AVPixelFormat` is `#[repr(i32)]` (same size and alignment as i32).
  // Reading as i32 cannot be UB regardless of the value FFmpeg wrote.
  let mut p = pix_fmts as *const i32;
  let none_int = AVPixelFormat::AV_PIX_FMT_NONE as i32;
  loop {
    // SAFETY: FFmpeg guarantees the list is terminated by AV_PIX_FMT_NONE.
    // We bail at the sentinel; reads up to and including it are in-bounds.
    let v = unsafe { ptr::read(p) };
    if v == none_int {
      return AVPixelFormat::AV_PIX_FMT_NONE;
    }
    if v == wanted_int {
      // **The coded-dimension seat.** This is the last moment before
      // the hardware frames pool is built, and the first at which the
      // *coded* extent is known — which is the extent that gets
      // allocated, and not the one `max_pixels` was checked against.
      //
      // `ff_set_dimensions` applies `av_image_check_size2` to the dims
      // the decoder passes it, and for a cropped stream those are the
      // **display** dims. Measured on this build with an h264 stream
      // carrying SPS cropping of 32x32 out of a 1920x1088 macroblock
      // grid — a 2040x divergence: `max_pixels = 5000` admits it,
      // because 32x32 is 1024 pixels. The surface behind it is
      // 2,088,960.
      //
      // On the software road that gap is already closed, and this was
      // measured too: `get_buffer2` receives the frame at **coded**
      // dims (1920x1088, aligned to 1920x1090, a 2 MiB allocation), so
      // `judge_buffer` bounds the real extent. The hardware road never
      // reaches `get_buffer2` at all — `ff_get_buffer` goes to
      // `hwaccel->alloc_frame` — which leaves this callback as the only
      // place the same number can be applied to the same extent.
      //
      // Refused by returning `AV_PIX_FMT_NONE`: that is already this
      // callback's vocabulary for "not this backend", and the machinery
      // around it already answers by falling back to software — where
      // `judge_buffer` applies the very same ceiling to the very same
      // coded dimensions. One number, both roads, and no refusal mode
      // invented for the occasion.
      // SAFETY: `state` is the live `CallbackState` this crate put in
      // `opaque`; it outlives the codec context. A null one means a
      // context this crate did not build, and gets no budget to judge
      // against.
      let budget = if state.is_null() {
        u64::MAX
      } else {
        unsafe { (*state).max_frame_bytes }
      };
      if let Some((cost, limit)) = unsafe { coded_extent_over_ceiling(ctx, wanted, budget) } {
        if !state.is_null() {
          use core::sync::atomic::Ordering;
          // SAFETY: as above.
          unsafe {
            (*state)
              .declined_pixels
              .store(i64::try_from(cost).unwrap_or(i64::MAX), Ordering::Relaxed);
            (*state)
              .declined_limit
              .store(i64::try_from(limit).unwrap_or(i64::MAX), Ordering::Relaxed);
            (*state).ceiling_declined.store(true, Ordering::Release);
          }
        }
        return AVPixelFormat::AV_PIX_FMT_NONE;
      }
      return wanted;
    }
    p = unsafe { p.add(1) };
  }
}

/// Whether this context's **coded** extent, aligned as the allocator
/// will align it, exceeds the pixel ceiling the context carries.
///
/// Reads `max_pixels` off the context — the field the crate set itself,
/// already carrying the byte ceiling converted at the worst per-pixel
/// rate — so there is no state to thread and no second number to keep
/// in step. The same read `judge_buffer` does on the software road.
///
/// Conservative when the context declares no coded extent: a zero or
/// negative dimension is nothing to judge, and refusing it here would
/// reject streams whose dimensions arrive later.
///
/// # Safety
///
/// `ctx` must be the live `AVCodecContext` FFmpeg passed to the
/// callback.
unsafe fn coded_extent_over_ceiling(
  ctx: *mut AVCodecContext,
  wanted: AVPixelFormat,
  budget: u64,
) -> Option<(u64, u64)> {
  // **Ask the pool what it will be, and refuse it if it will not say.**
  // `avcodec_get_hw_frames_parameters` builds the `AVHWFramesContext`
  // the decoder is about to initialise — fully populated and *not yet*
  // allocated — so its `width`/`height` are the pool's own declaration
  // of its extent and its `sw_format` is the layout that extent is
  // stored in.
  //
  // There used to be a fallback here: when the query failed, the judge
  // computed the codec's own aligned dimensions instead. That fallback
  // was unsound on its face, and the comment beside it said so — a
  // hardware pool aligns by its own API's rules, and D3D11 HEVC and AV1
  // round both dimensions to 128, so codec alignment can answer
  // *smaller* than the pool. A conservative fallback that can
  // under-state is not conservative; it is a hole with a comment on it.
  //
  // So this seat takes the rule the transfer judge already keeps: **a
  // pool that will not declare itself is a pool that cannot be judged,
  // and an unprovable extent is not a small one.** Refusing here
  // declines the hardware format, which falls the decode back to
  // software — where `judge_buffer` applies the same budget to the
  // frame it can see.
  //
  // SAFETY: `ctx` is the live context; the out-parameter is a fresh
  // null pointer FFmpeg fills with a reference we unref before
  // returning, and the device reference is borrowed, not consumed.
  let device_ref = unsafe { (*ctx).hw_device_ctx };
  if device_ref.is_null() {
    // No device means no hardware frames pool will be built at all, so
    // there is nothing for this seat to guard. That is a different
    // thing from a pool declining to describe itself, which is refused
    // below — this crate's hardware road always attaches a device
    // before opening, so reaching here means the caller is not on it.
    return None;
  }
  let mut frames_ref: *mut ffmpeg_next::ffi::AVBufferRef = core::ptr::null_mut();
  // **The format being negotiated, not `ctx->pix_fmt`.** At
  // `get_format` time the context still carries the software format;
  // asking about that one answers `ENOENT`. Measured: with the
  // hardware format the query returns the pool's own 32x32
  // declaration; with `ctx->pix_fmt` it returns -2.
  let rc = unsafe {
    ffmpeg_next::ffi::avcodec_get_hw_frames_parameters(ctx, device_ref, wanted, &mut frames_ref)
  };
  if rc < 0 || frames_ref.is_null() {
    return Some((u64::MAX, budget));
  }
  // SAFETY: on success `frames_ref` holds a live, uninitialised
  // `AVHWFramesContext`. `width`/`height` are plain `c_int`s;
  // `sw_format` is read as the integer it is, never as an enum.
  let (pool_w, pool_h, sw_format) = unsafe {
    let fc = (*frames_ref).data as *const ffmpeg_next::ffi::AVHWFramesContext;
    (
      (*fc).width,
      (*fc).height,
      core::ptr::read(core::ptr::addr_of!((*fc).sw_format) as *const libc::c_int),
    )
  };
  // SAFETY: the reference is ours to release and is released once.
  unsafe { ffmpeg_next::ffi::av_buffer_unref(&mut frames_ref) };

  let Some(cost) = pool_bytes(sw_format, pool_w, pool_h) else {
    // Dimensions that are not a picture: nothing here can be priced,
    // and nothing here will guess.
    return Some((u64::MAX, budget));
  };
  if cost > budget {
    tracing::warn!(
      pool_width = pool_w,
      pool_height = pool_h,
      cost,
      budget,
      "hwdecode: the hardware surface pool exceeds the frame ceiling; declining the \
       hardware format so the decode falls back to software, where the same ceiling applies"
    );
    return Some((cost, budget));
  }
  None
}

/// What a pool of `w` x `h` in `sw_format` costs.
///
/// Priced through the allocator-parity footprint when libavutil can
/// size the layout, and through
/// [`crate::footprint::video_frame_bytes_upper_bound`] when it cannot —
/// which applies the same dimension alignment and per-plane overhead at
/// the widest per-pixel rate, so it dominates every layout the build
/// could have priced. The bare `w * h * 16` this replaced omitted both
/// and could land *below* the accurate path.
///
/// `None` when the dimensions are not a picture, which callers treat as
/// unjudgeable rather than free.
fn pool_bytes(sw_format: libc::c_int, w: libc::c_int, h: libc::c_int) -> Option<u64> {
  crate::footprint::video_frame_bytes(sw_format, w, h)
    .or_else(|| crate::footprint::video_frame_bytes_upper_bound(w, h))
    .map(|bytes| bytes as u64)
}

/// Walk the codec's `AVCodecHWConfig` table and return whether the codec
/// advertises support for `device_type` **with** `wanted_pix_fmt` via the
/// `HW_DEVICE_CTX` setup method.
///
/// FFmpeg's HW config table is keyed per (device_type, pix_fmt) pair: a
/// codec can advertise the same device with several different hardware
/// pixel formats (e.g. VAAPI codecs that offer both `AV_PIX_FMT_VAAPI`
/// and `AV_PIX_FMT_DRM_PRIME`). Matching only on `device_type` would let
/// us proceed to install a strict `get_format` callback for a format the
/// codec never advertises, and the failure would surface deep inside the
/// probe / decode path instead of up front. Requiring the codec to
/// advertise the **exact** pix_fmt our `Backend` uses keeps the strict
/// `get_format` honest and gives `open_with` a clean rejection signal.
///
/// All reads from the FFmpeg-supplied `AVCodecHWConfig` are performed as
/// raw integers via `addr_of!` + `ptr::read::<i32>` to avoid copying or
/// interpreting enum-typed fields whose runtime values might not match
/// our build's discriminant set.
pub(crate) fn codec_supports_hwaccel(
  codec: *const AVCodec,
  device_type: AVHWDeviceType,
  wanted_pix_fmt: i32,
) -> bool {
  debug_assert!(!codec.is_null());
  let device_type_int = device_type as i32;
  let mut i = 0;
  loop {
    // SAFETY: `avcodec_get_hw_config` returns null past the end; we stop then.
    let cfg = unsafe { avcodec_get_hw_config(codec, i) };
    if cfg.is_null() {
      return false;
    }
    // Read each field as raw integer rather than copying the whole struct
    // (which would interpret `pix_fmt` and `device_type` as their enum types).
    // SAFETY: `cfg` is non-null and points to a valid `AVCodecHWConfig` for
    // the lifetime of the call; `addr_of!` projects to a sized field; the
    // `*const i32` cast is sound because `methods` is `c_int` (i32),
    // `device_type` is `AVHWDeviceType` (`#[repr(u32)]`, but FFmpeg's
    // assigned values fit in i32 and the runtime layout is i32-sized),
    // and `pix_fmt` is `AVPixelFormat` (`#[repr(i32)]`).
    let methods: i32 = unsafe { ptr::read(ptr::addr_of!((*cfg).methods)) };
    let cfg_device_type_int: i32 =
      unsafe { ptr::read(ptr::addr_of!((*cfg).device_type) as *const i32) };
    let cfg_pix_fmt_int: i32 = unsafe { ptr::read(ptr::addr_of!((*cfg).pix_fmt) as *const i32) };

    if methods & (AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0
      && cfg_device_type_int == device_type_int
      && cfg_pix_fmt_int == wanted_pix_fmt
    {
      return true;
    }
    i += 1;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pix_fmt_name_reads_the_linked_librarys_own_table() {
    assert_eq!(
      pix_fmt_name(AVPixelFormat::AV_PIX_FMT_YUV420P as i32).as_deref(),
      Some("yuv420p")
    );
    assert_eq!(
      pix_fmt_name(AVPixelFormat::AV_PIX_FMT_NV12 as i32).as_deref(),
      Some("nv12")
    );
    // A hardware surface: no CPU pixel data, so `from_av_pixel_format`
    // answers `PixelFormat::None` — and this is what puts a name on the
    // integer behind that `None` in the error message.
    assert_eq!(
      pix_fmt_name(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32).as_deref(),
      Some("videotoolbox_vld")
    );
  }

  #[test]
  fn pix_fmt_name_refuses_what_ffmpeg_cannot_name() {
    // The property the redeclared signature depends on, checked against
    // the linked library rather than assumed: libavutil bounds-checks
    // the index and answers out-of-range integers with null, so passing
    // a value outside the enum's discriminant set is defined.
    assert_eq!(pix_fmt_name(AVPixelFormat::AV_PIX_FMT_NONE as i32), None);
    assert_eq!(pix_fmt_name(-99_999), None);
    assert_eq!(pix_fmt_name(i32::MIN), None);
    assert_eq!(pix_fmt_name(i32::MAX), None);
    assert_eq!(pix_fmt_name(AVPixelFormat::AV_PIX_FMT_NB as i32), None);
  }

  // The callback derefs `(*ctx).opaque`, so we need a real-looking
  // AVCodecContext. We construct a zeroed one (the callback only reads opaque).
  struct FakeCtx(*mut AVCodecContext);
  impl FakeCtx {
    fn new(state: *mut CallbackState) -> Self {
      let boxed: Box<AVCodecContext> = unsafe { Box::new(std::mem::zeroed()) };
      let raw = Box::into_raw(boxed);
      unsafe { (*raw).opaque = state.cast() };
      Self(raw)
    }
  }
  impl Drop for FakeCtx {
    fn drop(&mut self) {
      unsafe { drop(Box::from_raw(self.0)) };
    }
  }

  fn make_state(wanted: AVPixelFormat) -> CallbackState {
    CallbackState {
      wanted,
      wanted_int: wanted as i32,
      ceiling_declined: core::sync::atomic::AtomicBool::new(false),
      declined_pixels: core::sync::atomic::AtomicI64::new(0),
      declined_limit: core::sync::atomic::AtomicI64::new(0),
      max_frame_bytes: u64::MAX,
      frame_budget_declined: core::sync::atomic::AtomicBool::new(false),
      declined_frame_bytes: core::sync::atomic::AtomicU64::new(0),
      declined_frame_audio: core::sync::atomic::AtomicBool::new(false),
    }
  }

  fn run(state: &CallbackState, mut offered: Vec<i32>) -> AVPixelFormat {
    // Build the offered list as raw i32, terminated by AV_PIX_FMT_NONE.
    offered.push(AVPixelFormat::AV_PIX_FMT_NONE as i32);
    let ctx = FakeCtx::new(state as *const _ as *mut _);
    // SAFETY: we cast the i32 buffer pointer to *const AVPixelFormat
    // because that's the function's declared signature. The callback only
    // ever reads through *const i32 internally, so this transit through
    // *const AVPixelFormat is purely a type system formality.
    unsafe { get_hw_format(ctx.0, offered.as_ptr() as *const AVPixelFormat) }
  }

  #[test]
  fn returns_wanted_hw_format_when_offered() {
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32,
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
  }

  #[test]
  fn returns_none_when_wanted_absent() {
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
        AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
  }

  #[test]
  fn null_opaque_is_treated_as_strict() {
    let boxed: Box<AVCodecContext> = unsafe { Box::new(std::mem::zeroed()) };
    let ctx_raw = Box::into_raw(boxed);
    unsafe { (*ctx_raw).opaque = ptr::null_mut() };
    let offered = [
      AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      AVPixelFormat::AV_PIX_FMT_NONE as i32,
    ];
    let got = unsafe { get_hw_format(ctx_raw, offered.as_ptr() as *const AVPixelFormat) };
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
    unsafe { drop(Box::from_raw(ctx_raw)) };
  }

  #[test]
  fn unknown_offered_value_is_skipped_without_ub() {
    // Simulate a header-skewed FFmpeg that offers a pixel-format value we
    // don't have a binding constant for (e.g. some future format). The
    // callback walks the list as i32 — no enum is constructed from that
    // value, so this read is sound.
    let state = make_state(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX);
    let got = run(
      &state,
      vec![
        99_999_i32, // imaginary unknown
        AVPixelFormat::AV_PIX_FMT_NV12 as i32,
      ],
    );
    assert_eq!(got, AVPixelFormat::AV_PIX_FMT_NONE);
  }

  /// `codec_supports_hwaccel` must reject a (device_type, pix_fmt) pair
  /// that the codec does not advertise — even if the device alone is
  /// listed. Without this check, the strict `get_format` callback would
  /// be wired up for a HW pix_fmt the codec never offers and the failure
  /// would surface deep inside the probe / decode path instead of at
  /// `open_with` / probe-build time.
  ///
  /// macOS-only: the test relies on FFmpeg's H.264 decoder advertising
  /// `(AV_HWDEVICE_TYPE_VIDEOTOOLBOX, AV_PIX_FMT_VIDEOTOOLBOX)`, which is
  /// only present in builds with VideoToolbox compiled in.
  #[cfg(target_os = "macos")]
  #[test]
  fn codec_supports_hwaccel_requires_matching_pix_fmt() {
    use ffmpeg_next::ffi::{AVCodecID, AVHWDeviceType, AVPixelFormat, avcodec_find_decoder};

    // SAFETY: AV_CODEC_ID_H264 is a known constant in our build's
    // `AVCodecID` discriminant set; constructing it does not invoke the
    // bindgen-enum UB we worry about for runtime-derived ids.
    let codec_ptr = unsafe { avcodec_find_decoder(AVCodecID::AV_CODEC_ID_H264) };
    assert!(!codec_ptr.is_null(), "H.264 decoder must be present");

    let device = AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX;
    let videotoolbox = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
    let nv12 = AVPixelFormat::AV_PIX_FMT_NV12 as i32;

    assert!(
      codec_supports_hwaccel(codec_ptr, device, videotoolbox),
      "VideoToolbox + AV_PIX_FMT_VIDEOTOOLBOX must be advertised by FFmpeg's H.264 decoder"
    );
    assert!(
      !codec_supports_hwaccel(codec_ptr, device, nv12),
      "VideoToolbox + AV_PIX_FMT_NV12 must NOT match the codec's HW config — \
       the strict get_format would have no offered HW format to return"
    );
  }
}
