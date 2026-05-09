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

use std::ptr;

use ffmpeg_next::ffi::{
  AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX, AVCodec, AVCodecContext, AVHWDeviceType, AVPixelFormat,
  avcodec_get_hw_config,
};

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
      return wanted;
    }
    p = unsafe { p.add(1) };
  }
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
