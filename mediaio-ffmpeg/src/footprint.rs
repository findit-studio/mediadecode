//! What `av_frame_get_buffer` actually costs.
//!
//! # The law this module exists to keep
//!
//! **A judge must dominate the allocator's arithmetic, not the
//! payload's.** Every ceiling in this crate answers the question "may
//! this frame be allocated?", and the only honest way to answer it is
//! to price what the *allocator* will do — not what the pixels or
//! samples nominally weigh. Those two numbers are not close:
//!
//! | shape | tight payload | `av_frame_get_buffer(0)` |
//! |---|---|---|
//! | `nv12` 16x16 | 384 | **1,792** |
//! | `yuv420p` 1x1 | 3 | **2,304** |
//! | `gray8` 65536x1 | 65,536 | **2,097,408** |
//! | `s16p`, 1 sample, 8ch | 16 | **768** |
//! | `dblp`, 1 sample, 255ch | 2,040 | **73,440** |
//! | `yuv420p` 1920x1080 | 3,110,400 | 3,133,696 |
//!
//! The last row is why this went unnoticed for so long: on ordinary
//! shapes the allocator overhead is under one percent, and every
//! under-pricing bug in this release has hidden behind a frame big
//! enough for the slack not to show.
//!
//! Two judges were pricing the payload — the hardware transfer judge
//! (768 priced, 1,792 allocated for a 16x16 NV12 destination) and the
//! resampler's output preflight (a 16-byte ceiling admitting a 768-byte
//! allocation) — so both now come through here.
//!
//! # Why estimates and not the formula
//!
//! `libavutil/frame.c` aligns the width up in a loop until the linesize
//! lands on the allocator's alignment, aligns the plane heights, and
//! adds a per-plane padding term; the constants differ by build and by
//! CPU (this machine resolves to a 32-byte alignment, others to 64).
//! Transcribing that is a fragile way to be exactly right and an easy
//! way to be quietly wrong.
//!
//! So these functions are deliberately **conservative upper bounds**,
//! and the contract is verified rather than argued: the tests price
//! every shape in the table above and compare against the real summed
//! `AVBufferRef.size`, asserting the estimate dominates in each case.
//! A build whose allocator grows hungrier fails those tests rather than
//! silently outgrowing a ceiling.

use libc::c_int;

/// Alignment to charge for. This machine's `av_frame_get_buffer`
/// resolves to 32; 64 is the largest FFmpeg uses on any SIMD target,
/// and charging the larger keeps the bound valid across builds.
const ALIGN: usize = 64;

/// Per-plane slack covering `frame.c`'s padding term and the
/// allocator's own bookkeeping.
///
/// Measured need is smaller — the tightest observed case is a packed
/// 1-sample 8-channel frame at 544 bytes against 64 aligned — but the
/// term costs nothing on frames big enough to matter and is what keeps
/// the tiny shapes dominated.
const PLANE_SLACK: usize = 512;

/// Rounds `value` up to a multiple of `align`.
#[inline]
const fn align_up(value: usize, align: usize) -> usize {
  value.div_ceil(align).saturating_mul(align)
}

/// An upper bound on what `av_frame_get_buffer(0)` allocates for a
/// video frame of `width` x `height` in the pixel format `format_raw`.
///
/// Both dimensions are aligned up before pricing, because the allocator
/// aligns the linesize *and* the plane heights — which is what turns a
/// 65536x1 frame from 64 KiB of pixels into a 2 MiB allocation.
///
/// `None` when libavutil cannot size the format at those dimensions, so
/// callers can fail closed rather than guess.
pub fn video_frame_bytes(format_raw: c_int, width: c_int, height: c_int) -> Option<usize> {
  if width <= 0 || height <= 0 {
    return None;
  }
  let aligned_w = align_up(width as usize, ALIGN).min(c_int::MAX as usize);
  let aligned_h = align_up(height as usize, ALIGN).min(c_int::MAX as usize);
  // SAFETY: the format is passed as the integer it is, through the
  // `c_int` shim; libavutil answers a negative AVERROR for anything it
  // cannot size.
  let size = unsafe {
    crate::ffi::c_shims::av_image_get_buffer_size(
      format_raw,
      aligned_w as c_int,
      aligned_h as c_int,
      ALIGN as c_int,
    )
  };
  if size <= 0 {
    return None;
  }
  // Four planes is `AV_NUM_DATA_POINTERS`'s image half, and the padding
  // term is per plane.
  Some((size as usize).saturating_add(PLANE_SLACK.saturating_mul(4)))
}

/// An upper bound on what a frame of `width` x `height` can cost in
/// **any** format this build can emit.
///
/// For the judges that have real dimensions but no priceable layout —
/// a hardware `sw_format` libavutil will not size, a transfer candidate
/// this build cannot describe. The alternative reached for first was a
/// bare `w * h * 16`, which is not an upper bound at all: it omits the
/// dimension alignment and the per-plane slack that
/// [`video_frame_bytes`] applies to every other estimate, so the
/// "conservative" fallback could price *below* the accurate path.
///
/// Built from the same machinery instead — align both dimensions, then
/// charge the widest per-pixel rate and the same plane overhead — so it
/// dominates [`video_frame_bytes`] for the same extent by construction.
///
/// `None` only when the dimensions are not a picture.
pub fn video_frame_bytes_upper_bound(width: c_int, height: c_int) -> Option<usize> {
  if width <= 0 || height <= 0 {
    return None;
  }
  let aligned_w = align_up(width as usize, ALIGN);
  let aligned_h = align_up(height as usize, ALIGN);
  // The per-pixel rate comes from the **census** — this build's
  // descriptor table, walked and priced — not from a literal. A build
  // that grows a format wider than today's 16 bytes is charged for it
  // without this crate learning its name, which is the whole reason
  // that census exists.
  aligned_w
    .checked_mul(aligned_h)?
    .checked_mul(worst_bytes_per_probe())?
    .checked_div(PROBE_PIXELS)?
    .checked_add(PLANE_SLACK.saturating_mul(4))
}

/// An upper bound on what `av_frame_get_buffer(0)` allocates for an
/// audio frame of `nb_samples` samples across `channels` channels in
/// the sample format `format_raw`.
///
/// # The allocator's own ruler, in the allocator's own order
///
/// The first version of this function did the arithmetic itself:
/// multiply the samples by the channel count, align the product once,
/// add slack. That is the wrong *order* — `av_frame_get_buffer` rounds
/// the **sample extent** and only then multiplies by the channels — and
/// the error is not small. For packed `dbl` at eight channels it priced
/// 576 bytes against a real 2,080; at 255 channels, 2,560 against
/// 65,312. Twenty-five times under, on a formula that looked right.
///
/// So the arithmetic is not restated here at all. `av_samples_get_buffer_size`
/// with `align = 0` *is* the ruler `av_frame_get_buffer` measures with,
/// and asking it removes the whole class of getting the order wrong.
/// Measured across every sample format this build names, the relation
/// is exact:
///
/// ```text
/// allocated = av_samples_get_buffer_size(channels, nb_samples, fmt, 0)
///           + 32 * planes          (planes = channels if planar, else 1)
/// ```
///
/// The per-plane term is charged at `PLANE_SLACK` rather than the
/// measured 32, which costs nothing and leaves room for a build whose
/// buffer header is larger.
///
/// `None` when libavutil will not size the request — an unnamed format,
/// a count it rejects — so callers fail closed rather than guess.
pub fn audio_frame_bytes(format_raw: c_int, nb_samples: usize, channels: usize) -> Option<usize> {
  let nb = c_int::try_from(nb_samples).ok()?;
  let ch = c_int::try_from(channels.max(1)).ok()?;
  // SAFETY: every argument is passed as the integer it is, through the
  // `c_int` shim; libavutil answers a negative AVERROR for anything it
  // will not size.
  let base = unsafe {
    crate::ffi::c_shims::av_samples_get_buffer_size(core::ptr::null_mut(), ch, nb, format_raw, 0)
  };
  if base < 0 {
    return None;
  }
  let planar = crate::sample_format::SampleFormat::from_raw(format_raw).is_planar();
  let planes = if planar { channels.max(1) } else { 1 };
  (base as usize).checked_add(PLANE_SLACK.checked_mul(planes)?)
}

/// The probe square the per-pixel cost is measured on.
///
/// 256 divides every chroma subsampling FFmpeg has **and** every
/// alignment libavcodec uses, so the measurement is exact: no plane is
/// rounded up to cover a half-sized dimension, and no row is padded to
/// an alignment boundary. Measured at 257 the same census reads 16.934
/// bytes per pixel instead of 16.000 — that 5.8% is per-*row* padding,
/// a term linear in height rather than in pixels, and it is not part of
/// the per-pixel rate.
pub(crate) const PROBE_PIXELS: usize = 256 * 256;

/// Bytes a [`PROBE_PIXELS`]-pixel picture costs in the **most expensive
/// pixel format this build of libavcodec can describe**.
///
/// # Why the worst case and not the declared one
///
/// The first cut of this ceiling charged the format the *container*
/// declared, and a container's declaration is not an upper bound on
/// anything. It may be unset, it may be wrong, and it may be narrower
/// than what the decoder actually emits — a stream declaring `yuv420p`
/// at 1.5 bytes per pixel whose decoder outputs `rgbaf32` at 16 got a
/// ceiling more than ten times too generous, which is the same hole one
/// layer down from the one it was added to close.
///
/// So the rate is not negotiated with the file at all. Every stream is
/// charged the worst case, and the worst case is **measured**, not
/// tabulated: this build's descriptor list is walked once and each
/// format priced through `av_image_get_buffer_size`, the same function
/// `avcodec_default_get_buffer2` sizes from. A future FFmpeg that adds
/// a wider format is priced correctly without this crate learning its
/// name.
///
/// # The census, at the time of writing
///
/// 267 descriptors, 251 of them CPU formats that price (the rest are
/// hardware surfaces, which carry no CPU bytes and return no size). The
/// maximum is **16.000 bytes per pixel**, reached by eight formats —
/// `gbrapf32be/le`, `rgbaf32be/le`, `rgba128be/le`, `gbrap32be/le`.
/// Next below are the 12-byte `gbrpf32`/`rgbf32` family.
///
/// # What this trades
///
/// Over-refusal for cheap formats, and it is deliberate. At the 512 MiB
/// default the effective ceiling becomes ~33.55 Mpx, so 8K (33.18 Mpx)
/// still decodes in *any* format — including the 16-byte ones, where it
/// really does cost 506 MiB — but a 16K `yuv420p` frame, which would
/// only have cost 199 MB, is refused too. That is the honest shape of a
/// bound that has to hold before the format is known: the deployment
/// answer is to raise `max_frame_bytes`, which is exactly the setting that
/// says how much memory one frame may cost.
///
/// # The residual, stated
///
/// Row alignment adds at most `align x planes x height` bytes on top of
/// this rate — about 1 MB on an 8K frame, 0.2%, and covered by the fact
/// that `max_frame_bytes` is a policy number rather than a hardware
/// limit. It is only significant for degenerate aspect ratios (a
/// one-pixel-wide frame is all padding), which the *pixel* ceiling has
/// always been the wrong shape to bound and which this change neither
/// introduces nor worsens.
pub(crate) fn worst_bytes_per_probe() -> usize {
  /// The census result, taken once. `av_pix_fmt_desc_next` walks a
  /// static table that cannot change during the process.
  static WORST: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
  *WORST.get_or_init(|| {
    /// The measured maximum at the time of writing, and the floor this
    /// census may not fall below. A build whose census comes back
    /// *smaller* than the eight 16-byte formats has failed to walk the
    /// table, not discovered a cheaper world — take the known number
    /// rather than a ceiling built on a failed measurement.
    const KNOWN_WORST_BYTES_PER_PIXEL: usize = 16;

    let mut worst = 0usize;
    let mut desc: *const ffmpeg_next::ffi::AVPixFmtDescriptor = core::ptr::null();
    loop {
      // SAFETY: `av_pix_fmt_desc_next` walks libavutil's own static
      // descriptor table, taking the previous entry (or null to start)
      // and returning null at the end. It traffics in descriptor
      // pointers, not enums, so it needs no shim.
      desc = unsafe { ffmpeg_next::ffi::av_pix_fmt_desc_next(desc) };
      if desc.is_null() {
        break;
      }
      // **Both of these go through the `c_int` shims**, and this is the
      // place it matters most: the whole point of walking the table is
      // to price formats this build's bindings may not name, and the
      // generated `av_pix_fmt_desc_get_id` hands those ids back as a
      // closed `AVPixelFormat`. Every future format would have become
      // an invalid enum value on the way into the pricing meant to
      // handle it — the census would have been UB on exactly its reason
      // for existing.
      //
      // SAFETY: `desc` is a live entry from libavutil's static table;
      // the id is passed straight back to libavutil as the integer it
      // is, and `av_image_get_buffer_size` returns a negative AVERROR
      // for ids it cannot size rather than misbehaving.
      let id = unsafe { crate::ffi::c_shims::av_pix_fmt_desc_get_id(desc) };
      let size = unsafe { crate::ffi::c_shims::av_image_get_buffer_size(id, 256, 256, 1) };
      if size > 0 {
        worst = worst.max(size as usize);
      }
    }
    worst.max(KNOWN_WORST_BYTES_PER_PIXEL * PROBE_PIXELS)
  })
}

#[cfg(test)]
mod tests;
