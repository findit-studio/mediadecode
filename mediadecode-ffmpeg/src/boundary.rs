//! Boundary conversions between FFmpeg's bindgen integers and the
//! unified [`mediadecode`] vocabulary.
//!
//! Centralised so the rest of the crate never compares raw
//! `AVPixelFormat` integers against literals or transmutes back into
//! the bindgen enum (UB hazard when the value isn't in the enum's
//! discriminant set).

use core::ffi::c_int;

use ffmpeg_next::{Packet, ffi::AVPixelFormat};
use mediadecode::{
  PixelFormat, Timestamp,
  channel::AudioChannelLayout,
  frame::{AudioFrame, Dimensions, Plane, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, PacketFlags as MdPacketFlags, SubtitlePacket, VideoPacket},
  subtitle::SubtitlePayload,
};

use crate::{
  FfmpegBuffer,
  extras::{
    AudioFrameExtra, AudioPacketExtra, SubtitleFrameExtra, SubtitlePacketExtra, VideoFrameExtra,
    VideoPacketExtra,
  },
  sample_format::SampleFormat,
};

/// Maps a raw `AVFrame.format` integer (i.e. the value of an
/// `AVPixelFormat` enum variant) onto [`mediadecode::PixelFormat`].
///
/// Returns [`PixelFormat::Unknown`] for raw integers we don't have a
/// mapping for — including hardware-frame markers
/// (`AV_PIX_FMT_VIDEOTOOLBOX` / `_VAAPI` / `_CUDA` / `_D3D11` / …)
/// since those never describe CPU-side pixel data and the unified
/// enum intentionally doesn't carry them. Use [`is_hardware_pix_fmt`]
/// to identify HW frames before transferring to a CPU format.
///
/// The match never constructs an `AVPixelFormat` from a runtime
/// value; it compares the input against `AVPixelFormat::AV_PIX_FMT_X
/// as i32` constants. Sound regardless of which discriminant set the
/// linked FFmpeg version exposes.
pub const fn from_av_pixel_format(raw: i32) -> PixelFormat {
  match raw {
    // Semi-planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_NV12 as i32 => PixelFormat::Nv12,
    x if x == AVPixelFormat::AV_PIX_FMT_NV21 as i32 => PixelFormat::Nv21,
    x if x == AVPixelFormat::AV_PIX_FMT_NV16 as i32 => PixelFormat::Nv16,
    x if x == AVPixelFormat::AV_PIX_FMT_NV24 as i32 => PixelFormat::Nv24,
    x if x == AVPixelFormat::AV_PIX_FMT_NV42 as i32 => PixelFormat::Nv42,
    // Semi-planar YUV high-bit-depth.
    x if x == AVPixelFormat::AV_PIX_FMT_P010LE as i32 => PixelFormat::P010Le,
    // BE-tagged FFmpeg formats map to mediadecode's distinct BE
    // variants. Folding BE onto the LE canonical enum was a previous
    // shortcut that silently corrupted pixel data: each 16-bit sample
    // is byte-swapped between BE and LE, and the convert path
    // exports the AVBufferRef bytes verbatim without endian
    // conversion. Consumers reading the planes as LE samples on a
    // BE-tagged frame would interpret every Y/UV sample with its
    // bytes reversed. By mapping to the BE variant we let
    // `is_supported_cpu_pix_fmt` correctly reject the format until
    // proper BE support (or a byte-swap) is wired in.
    x if x == AVPixelFormat::AV_PIX_FMT_P010BE as i32 => PixelFormat::P010Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P012LE as i32 => PixelFormat::P012Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P016LE as i32 => PixelFormat::P016Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P210LE as i32 => PixelFormat::P210Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P212LE as i32 => PixelFormat::P212Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P216LE as i32 => PixelFormat::P216Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P410LE as i32 => PixelFormat::P410Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P412LE as i32 => PixelFormat::P412Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P416LE as i32 => PixelFormat::P416Le,
    // Planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 => PixelFormat::Yuv420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P as i32 => PixelFormat::Yuv422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P as i32 => PixelFormat::Yuv440p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P as i32 => PixelFormat::Yuv444p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV411P as i32 => PixelFormat::Yuv411p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV410P as i32 => PixelFormat::Yuv410p,
    // Planar YUV 4:2:0 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P9LE as i32 => PixelFormat::Yuv420p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32 => PixelFormat::Yuv420p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P12LE as i32 => PixelFormat::Yuv420p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P14LE as i32 => PixelFormat::Yuv420p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P16LE as i32 => PixelFormat::Yuv420p16Le,
    // Planar YUV 4:2:2 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P9LE as i32 => PixelFormat::Yuv422p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32 => PixelFormat::Yuv422p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P12LE as i32 => PixelFormat::Yuv422p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P14LE as i32 => PixelFormat::Yuv422p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P16LE as i32 => PixelFormat::Yuv422p16Le,
    // Planar YUV 4:4:4 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P9LE as i32 => PixelFormat::Yuv444p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 => PixelFormat::Yuv444p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12LE as i32 => PixelFormat::Yuv444p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P14LE as i32 => PixelFormat::Yuv444p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P16LE as i32 => PixelFormat::Yuv444p16Le,
    // Planar YUVA 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P as i32 => PixelFormat::Yuva420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P as i32 => PixelFormat::Yuva422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P as i32 => PixelFormat::Yuva444p,
    // Packed YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUYV422 as i32 => PixelFormat::Yuyv422,
    x if x == AVPixelFormat::AV_PIX_FMT_UYVY422 as i32 => PixelFormat::Uyvy422,
    x if x == AVPixelFormat::AV_PIX_FMT_YVYU422 as i32 => PixelFormat::Yvyu422,
    // Packed RGB 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB24 as i32 => PixelFormat::Rgb24,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR24 as i32 => PixelFormat::Bgr24,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA as i32 => PixelFormat::Rgba,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA as i32 => PixelFormat::Bgra,
    x if x == AVPixelFormat::AV_PIX_FMT_ARGB as i32 => PixelFormat::Argb,
    x if x == AVPixelFormat::AV_PIX_FMT_ABGR as i32 => PixelFormat::Abgr,
    // Packed RGB high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB48LE as i32 => PixelFormat::Rgb48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR48LE as i32 => PixelFormat::Bgr48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA64LE as i32 => PixelFormat::Rgba64Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA64LE as i32 => PixelFormat::Bgra64Le,
    // Greyscale.
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY8 as i32 => PixelFormat::Gray8,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY16LE as i32 => PixelFormat::Gray16Le,
    // Bayer.
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR8 as i32 => PixelFormat::BayerBggr8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB8 as i32 => PixelFormat::BayerRggb8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG8 as i32 => PixelFormat::BayerGbrg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG8 as i32 => PixelFormat::BayerGrbg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR16LE as i32 => PixelFormat::BayerBggr16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB16LE as i32 => PixelFormat::BayerRggb16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG16LE as i32 => PixelFormat::BayerGbrg16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG16LE as i32 => PixelFormat::BayerGrbg16Le,
    _ => PixelFormat::Unknown(raw as u32),
  }
}

/// Returns `true` when `raw` is one of FFmpeg's hardware-frame markers
/// (`AV_PIX_FMT_VIDEOTOOLBOX` / `_VAAPI` / `_CUDA` / `_D3D11` /
/// `_DRM_PRIME` / `_MEDIACODEC` / `_VULKAN`). Used by the HW probe to
/// identify GPU-resident frames before triggering
/// `av_hwframe_transfer_data`.
pub const fn is_hardware_pix_fmt(raw: i32) -> bool {
  matches!(
    raw,
    x if x == AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32
      || x == AVPixelFormat::AV_PIX_FMT_VAAPI as i32
      || x == AVPixelFormat::AV_PIX_FMT_CUDA as i32
      || x == AVPixelFormat::AV_PIX_FMT_D3D11 as i32
      || x == AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32
      || x == AVPixelFormat::AV_PIX_FMT_MEDIACODEC as i32
      || x == AVPixelFormat::AV_PIX_FMT_VULKAN as i32
  )
}

/// Fallible counterpart to ffmpeg-next's `Packet::copy`.
///
/// The upstream helper calls `Packet::new(size)` (which silently
/// truncates `size` to `c_int` and ignores `av_new_packet`'s return
/// code) and then panics via `data_mut().unwrap().write_all(...).unwrap()`
/// if the allocation failed. From a safe public decoder API we want
/// the OOM / oversized-payload paths to surface as
/// `ffmpeg_next::Error` rather than aborting the process — every
/// `send_packet` path goes through this helper.
///
/// Failure modes:
/// * payload larger than `c_int::MAX` (would overflow `AVPacket.size`)
///   → `ffmpeg_next::Error::Other { errno: libc::EINVAL }`.
/// * `av_new_packet` allocation failure (signalled by `data_mut()`
///   returning `None`) → `ffmpeg_next::Error::Other { errno:
///   libc::ENOMEM }`.
fn try_packet_copy(data: &[u8]) -> std::result::Result<Packet, ffmpeg_next::Error> {
  // FFmpeg's `AVPacket.size` is `c_int`. A payload larger than that
  // can't fit in a single packet — refuse rather than truncate via
  // `as c_int` inside `Packet::new`.
  if data.len() > c_int::MAX as usize {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::EINVAL,
    });
  }
  // `Packet::new(size)` calls `av_new_packet(&mut pkt, size as
  // c_int)` and ignores the return code; on OOM it returns a
  // `Packet` whose `.data` is null. We detect that via
  // `data_mut()` (returns `None` on null) and copy via
  // `copy_nonoverlapping` so we never go through `data_mut()
  // .unwrap().write_all().unwrap()` — the upstream `Packet::copy`'s
  // double panic.
  let mut pkt = Packet::new(data.len());
  match pkt.data_mut() {
    Some(slot) if slot.len() == data.len() => {
      // SAFETY: `slot` is a `&mut [u8]` of `data.len()` bytes;
      // `data` is a `&[u8]` of the same length. Non-overlapping
      // because `slot` is a fresh allocation.
      if !data.is_empty() {
        unsafe {
          core::ptr::copy_nonoverlapping(data.as_ptr(), slot.as_mut_ptr(), data.len());
        }
      }
      Ok(pkt)
    }
    _ => Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }),
  }
}

/// Centralised mediadecode→AV packet flag mapping so the three
/// packet-conversion helpers stay aligned.
fn map_md_flags_to_av(flags: MdPacketFlags) -> ffmpeg_next::packet::Flags {
  let mut av_flags = ffmpeg_next::packet::Flags::empty();
  if flags.contains(MdPacketFlags::KEY) {
    av_flags |= ffmpeg_next::packet::Flags::KEY;
  }
  if flags.contains(MdPacketFlags::CORRUPT) {
    av_flags |= ffmpeg_next::packet::Flags::CORRUPT;
  }
  // ffmpeg-next 8.x doesn't expose a DISCARD flag constant on
  // `packet::Flags`; the upstream `AV_PKT_FLAG_DISCARD` bit is
  // documented as a demuxer hint and rarely set on packets passed
  // to a decoder. We forward KEY and CORRUPT (the meaningful subset)
  // and silently drop DISCARD until ffmpeg-next adds it.
  av_flags
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::VideoPacket`]
/// parameterized by [`crate::extras::VideoPacketExtra`] and
/// [`crate::FfmpegBuffer`].
///
/// The compressed bytes are **copied** into a new packet allocation —
/// zero-copy passthrough of the FfmpegBuffer's underlying AVBufferRef
/// is a future optimization (would need to wire an `AVBufferRef` into
/// `AVPacket.buf` directly via `av_packet_alloc` + manual buffer set).
/// PTS / DTS / duration / flags / stream_index are propagated.
///
/// Returns `Err(ffmpeg_next::Error)` on:
/// * payload larger than `c_int::MAX` (would overflow `AVPacket.size`);
/// * `av_new_packet` allocation failure (OOM).
pub fn ffmpeg_packet_from_video_packet(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::AudioPacket`].
/// Same shape as [`ffmpeg_packet_from_video_packet`] — bytes are
/// copied; pts/dts/duration/flags/stream_index are forwarded. Same
/// failure modes.
pub fn ffmpeg_packet_from_audio_packet(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::SubtitlePacket`].
/// Bytes copied; pts/duration/flags/stream_index forwarded. Subtitle
/// packets have no `dts` in the mediadecode model. Same failure
/// modes as [`ffmpeg_packet_from_video_packet`].
pub fn ffmpeg_packet_from_subtitle_packet(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

// ---------------------------------------------------------------------------
//  Safe wrappers — `&ffmpeg::Packet` → `mediadecode::*Packet`.
// ---------------------------------------------------------------------------

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::VideoPacket`]. The compressed payload is
/// shared with the source `AVPacket` via refcount bump (no copy).
/// Timestamps, duration, key/corrupt flags, and the source stream
/// index are forwarded to the produced packet.
///
/// Returns `None` when the source packet has no buffer attached
/// (empty packet — typical after EOF). Caller can also fill in
/// [`VideoPacketExtra::byte_pos`] / `side_data` post-construction
/// if they need those.
pub fn video_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<VideoPacket<VideoPacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = VideoPacket::new(buf, VideoPacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  if let Some(d) = packet.dts() {
    out = out.with_dts(Some(Timestamp::new(d, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::AudioPacket`]. Same shape as
/// [`video_packet_from_ffmpeg`] — refcounted payload, forwarded
/// metadata.
pub fn audio_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<AudioPacket<AudioPacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = AudioPacket::new(buf, AudioPacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  if let Some(d) = packet.dts() {
    out = out.with_dts(Some(Timestamp::new(d, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::SubtitlePacket`]. Subtitle packets have no
/// `dts` in the mediadecode model; everything else mirrors
/// [`video_packet_from_ffmpeg`].
pub fn subtitle_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<SubtitlePacket<SubtitlePacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = SubtitlePacket::new(buf, SubtitlePacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

fn md_flags_from_av(flags: ffmpeg_next::packet::Flags) -> MdPacketFlags {
  let mut out = MdPacketFlags::empty();
  if flags.contains(ffmpeg_next::packet::Flags::KEY) {
    out |= MdPacketFlags::KEY;
  }
  if flags.contains(ffmpeg_next::packet::Flags::CORRUPT) {
    out |= MdPacketFlags::CORRUPT;
  }
  out
}

// ---------------------------------------------------------------------------
//  Empty-frame placeholders for `receive_frame` destinations.
// ---------------------------------------------------------------------------

/// Constructs an empty [`mediadecode::frame::VideoFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::VideoStreamDecoder::receive_frame`]. The
/// decoder overwrites the frame on success; this just provides a
/// well-formed slot.
///
/// All four plane slots get a 1-byte `FfmpegBuffer` placeholder
/// (the array shape requires a buffer in every slot, but
/// `plane_count = 0` reports them as inactive).
///
/// # Panics
///
/// Panics on FFmpeg-side OOM (the per-plane 1-byte allocation
/// failed). Callers who need to recover from OOM should use
/// [`try_empty_video_frame`].
pub fn empty_video_frame() -> VideoFrame<PixelFormat, VideoFrameExtra, FfmpegBuffer> {
  try_empty_video_frame().expect("empty_video_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_video_frame`]. Returns `None` if
/// any of the four placeholder allocations fails.
pub fn try_empty_video_frame() -> Option<VideoFrame<PixelFormat, VideoFrameExtra, FfmpegBuffer>> {
  let planes = [
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
  ];
  Some(VideoFrame::new(
    Dimensions::new(0, 0),
    PixelFormat::Unknown(0),
    planes,
    0,
    VideoFrameExtra::default(),
  ))
}

/// Constructs an empty [`mediadecode::frame::AudioFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::AudioStreamDecoder::receive_frame`]. Same
/// behaviour as [`empty_video_frame`] — eight 1-byte plane
/// placeholders, `plane_count = 0`.
///
/// # Panics
///
/// Panics on FFmpeg-side OOM. See [`try_empty_audio_frame`] for the
/// fallible variant.
pub fn empty_audio_frame()
-> AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer> {
  try_empty_audio_frame().expect("empty_audio_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_audio_frame`]. Returns `None` if
/// any of the eight placeholder allocations fails.
pub fn try_empty_audio_frame()
-> Option<AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer>> {
  let planes = [
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
  ];
  Some(AudioFrame::new(
    0,
    0,
    0,
    SampleFormat::NONE,
    AudioChannelLayout::default(),
    planes,
    0,
    AudioFrameExtra::default(),
  ))
}

/// Constructs an empty [`mediadecode::frame::SubtitleFrame`] suitable
/// as the destination argument to
/// [`mediadecode::decoder::SubtitleDecoder::receive_frame`]. The
/// payload is an empty `Text` placeholder; the decoder overwrites
/// it on success.
///
/// # Panics
///
/// Panics on FFmpeg-side OOM. See [`try_empty_subtitle_frame`] for
/// the fallible variant.
pub fn empty_subtitle_frame() -> SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer> {
  try_empty_subtitle_frame().expect("empty_subtitle_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_subtitle_frame`]. Returns `None`
/// if the placeholder allocation fails.
pub fn try_empty_subtitle_frame() -> Option<SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::copy_from_slice(&[]).or_else(FfmpegBuffer::try_empty)?;
  Some(SubtitleFrame::new(
    SubtitlePayload::Text {
      text: buf,
      language: None,
    },
    SubtitleFrameExtra::default(),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nv12_round_trips() {
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_NV12 as i32),
      PixelFormat::Nv12,
    );
  }

  #[test]
  fn p010be_maps_to_p010be() {
    // BE must map to the BE variant — the previous "fold to LE"
    // mapping silently corrupted P010BE pixel data via the safe
    // export path. The unsupported-format gate in `convert::av_frame_to_video_frame`
    // is the right place to reject BE today.
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_P010BE as i32),
      PixelFormat::P010Be,
    );
  }

  #[test]
  fn unknown_for_garbage_value() {
    assert!(matches!(
      from_av_pixel_format(-99_999),
      PixelFormat::Unknown(_)
    ));
  }

  #[test]
  fn hw_formats_detected() {
    assert!(is_hardware_pix_fmt(
      AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32
    ));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_VAAPI as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_CUDA as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_D3D11 as i32));
  }

  #[test]
  fn cpu_formats_not_detected_as_hw() {
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NV12 as i32));
    assert!(!is_hardware_pix_fmt(
      AVPixelFormat::AV_PIX_FMT_YUV420P as i32
    ));
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NONE as i32));
  }

  #[test]
  fn hw_formats_map_to_unknown_in_pixel_format() {
    // HW sentinels intentionally don't have a mediadecode::PixelFormat
    // representation — they're not CPU pixel data.
    assert!(matches!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32),
      PixelFormat::Unknown(_)
    ));
    assert!(matches!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VAAPI as i32),
      PixelFormat::Unknown(_)
    ));
  }
}
