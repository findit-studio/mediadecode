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
pub(crate) fn video_frame_bytes(format_raw: c_int, width: c_int, height: c_int) -> Option<usize> {
  if width <= 0 || height <= 0 {
    return None;
  }
  let aligned_w = align_up(width as usize, ALIGN).min(c_int::MAX as usize);
  let aligned_h = align_up(height as usize, ALIGN).min(c_int::MAX as usize);
  // SAFETY: the format is passed as the integer it is, through the
  // `c_int` shim; libavutil answers a negative AVERROR for anything it
  // cannot size.
  let size = unsafe {
    crate::decoder::c_shims::av_image_get_buffer_size(
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
pub(crate) fn video_frame_bytes_upper_bound(width: c_int, height: c_int) -> Option<usize> {
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
    .checked_mul(crate::decoder::worst_bytes_per_probe())?
    .checked_div(crate::decoder::PROBE_PIXELS)?
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
/// The per-plane term is charged at [`PLANE_SLACK`] rather than the
/// measured 32, which costs nothing and leaves room for a build whose
/// buffer header is larger.
///
/// `None` when libavutil will not size the request — an unnamed format,
/// a count it rejects — so callers fail closed rather than guess.
pub(crate) fn audio_frame_bytes(
  format_raw: c_int,
  nb_samples: usize,
  channels: usize,
) -> Option<usize> {
  let nb = c_int::try_from(nb_samples).ok()?;
  let ch = c_int::try_from(channels.max(1)).ok()?;
  // SAFETY: every argument is passed as the integer it is, through the
  // `c_int` shim; libavutil answers a negative AVERROR for anything it
  // will not size.
  let base = unsafe {
    crate::decoder::c_shims::av_samples_get_buffer_size(
      core::ptr::null_mut(),
      ch,
      nb,
      format_raw,
      0,
    )
  };
  if base < 0 {
    return None;
  }
  let planar = crate::sample_format::SampleFormat::from_raw(format_raw).is_planar();
  let planes = if planar { channels.max(1) } else { 1 };
  (base as usize).checked_add(PLANE_SLACK.checked_mul(planes)?)
}

#[cfg(test)]
mod tests;
