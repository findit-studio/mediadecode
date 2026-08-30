#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
// The core crate carries this same allow, for this same reason, and 0.9
// is where this crate joined it: a signature here reads
// `VideoPacket<VideoPacketExtra, FfmpegBytes>` because every one of those
// three names is load-bearing — the household, the backend's extras,
// and the owned carrier the D-seat amputation contract requires.
// `clippy::type_complexity` counts nesting, and the fix it asks for is
// an alias that hides exactly the word this release exists to make
// visible. 0.8 had that alias; it was called `FfmpegBuffer`.
#![allow(clippy::type_complexity)]

mod adapter;
mod audio;
mod backend;
pub mod boundary;
mod buffer;
mod carrier;
pub mod channel_layout;
mod codec_id;
pub mod convert;
mod decoder;
mod demuxer;
mod error;
pub mod extras;
#[cfg(test)]
mod fault_subprocess;
mod ffi;
mod footprint;
mod frame;
mod image;
pub mod limits;
mod pixdesc;
mod reader_guard;
#[cfg(feature = "resample")]
mod resampler;
mod sample_format;
mod subtitle;
pub mod ticket;
mod video;
mod view;

pub use adapter::Ffmpeg;
pub use audio::{AudioDecodeError, CarrierAudioStreamDecoder};
pub use backend::Backend;
pub use boundary::{
  MediaKind, PacketBuildError, SendPayloadTooLarge, SendSideDataTooLarge,
  attachment_packet_from_ffmpeg, audio_packet_from_ffmpeg, audio_packet_from_ffmpeg_in,
  data_packet_from_ffmpeg_in, empty_audio_frame, empty_owned_audio_frame,
  empty_owned_subtitle_frame, empty_owned_video_frame, empty_subtitle_frame, empty_video_frame,
  ffmpeg_packet_from_audio_packet, ffmpeg_packet_from_owned_audio_packet,
  ffmpeg_packet_from_owned_subtitle_packet, ffmpeg_packet_from_owned_video_packet,
  ffmpeg_packet_from_subtitle_packet, ffmpeg_packet_from_video_packet, from_av_pixel_format,
  is_hardware_pix_fmt, owned_attachment_packet_from_ffmpeg, owned_audio_packet_from_ffmpeg_in,
  owned_data_packet_from_ffmpeg_in, owned_subtitle_packet_from_ffmpeg_in,
  owned_video_packet_from_ffmpeg_in, subtitle_packet_from_ffmpeg, subtitle_packet_from_ffmpeg_in,
  video_packet_from_ffmpeg, video_packet_from_ffmpeg_in,
};

pub use buffer::{FfmpegBytes, PacketBufferError, TrustedPayload};
pub(crate) use carrier::CarrierOps;
pub use carrier::{FfmpegCarrier, Owned, View};
pub use view::FfmpegBuffer;

/// The demuxer, on the **view** lane — the ordinary road.
///
/// Packets carry [`FfmpegBuffer`] views onto libavformat's own
/// allocations: nothing is copied, and a consumer that reads a packet
/// and drops it never pays for bytes it did not keep. This is what a
/// direct consumer wants, which is why it is what the bare name means.
///
/// Reach for [`FfmpegOwnedDemuxer`] when a packet has to **travel** —
/// across a graph, between threads that both read it, into a cache
/// that outlives the decoder. See [the carrier lanes][lanes] for the
/// tradeoff table.
///
/// [lanes]: mediadecode::adapter#the-two-carrier-lanes
pub type FfmpegDemuxer = demuxer::CarrierDemuxer<View>;

/// The demuxer on the **owned** lane: every byte copied once at the
/// boundary into memory Rust owns.
///
/// The same type as [`FfmpegDemuxer`] on the other carrier, with the
/// same constructors — `FfmpegOwnedDemuxer::open(&path)`. Packets are
/// `Send + Sync + 'static` and owe nothing to the session that produced
/// them, which is what a graph needs and what a view cannot give.
pub type FfmpegOwnedDemuxer = demuxer::CarrierDemuxer<Owned>;

/// The audio decoder on the **view** lane.
pub type FfmpegAudioStreamDecoder = audio::CarrierAudioStreamDecoder<View>;
/// The audio decoder on the **owned** lane.
pub type FfmpegOwnedAudioStreamDecoder = audio::CarrierAudioStreamDecoder<Owned>;

/// The subtitle decoder on the **view** lane.
pub type FfmpegSubtitleStreamDecoder = subtitle::CarrierSubtitleStreamDecoder<View>;
/// The subtitle decoder on the **owned** lane.
pub type FfmpegOwnedSubtitleStreamDecoder = subtitle::CarrierSubtitleStreamDecoder<Owned>;

/// The still-image decoder on the **view** lane.
pub type FfmpegImageDecoder = image::CarrierImageDecoder<View>;
/// The still-image decoder on the **owned** lane.
pub type FfmpegOwnedImageDecoder = image::CarrierImageDecoder<Owned>;

/// The video stream decoder on the **view** lane.
pub type FfmpegVideoStreamDecoder = video::CarrierVideoStreamDecoder<View>;
/// The video stream decoder on the **owned** lane.
pub type FfmpegOwnedVideoStreamDecoder = video::CarrierVideoStreamDecoder<Owned>;
pub use channel_layout::{
  channel_layout_description_from_ffmpeg, channel_layout_from_ffmpeg, channel_order_from_ffmpeg,
};
pub use codec_id::CodecId;
pub use decoder::VideoDecoder;
pub use demuxer::{CarrierDemuxer, DemuxError, ProbeBudgetExhausted};
pub use error::{
  Error, FrameBudgetExceeded, FrameMedium, HwSurfaceTooLarge, HwTransferTooLarge, Result,
};
pub use frame::Frame;
pub use image::{CarrierImageDecoder, Corrupt, CorruptSource, ImageDecodeError, InputTooLarge};
pub use limits::{
  DEFAULT_MAX_ATTACHMENT_BYTES, DEFAULT_MAX_CODEC_PARAMETER_BYTES, DEFAULT_MAX_FRAME_BYTES,
  DEFAULT_MAX_IMAGE_INPUT_BYTES, DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES, DEFAULT_MAX_PACKET_BYTES,
  DEFAULT_MAX_PIXELS, DEFAULT_MAX_PROBE_BYTES, DEFAULT_MAX_STREAMS,
  DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES, DEFAULT_MAX_TOTAL_CODEC_PARAMETER_BYTES, DecoderLimits,
  DemuxLimits, FrameLimits, PacketLimits,
};
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub use resampler::{
  CarrierResampler, OutputTooLarge, ResampleError, ResampleSpec, SpecEnd,
  UnsupportedChannelCount as UnsupportedSpecChannelCount,
};
/// The resampler, on the **view** lane — planes shared out of its own
/// output frame.
///
/// Its output frames are the resampler's, not a decoder's: it allocates
/// one per conversion and never writes into a buffer it has handed out,
/// so the pool-hostage warning that applies to decoder frames does not
/// apply here. What does apply is `!Sync` and the amputation contract —
/// use [`FfmpegOwnedResampler`] for a frame that has to travel.
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub type FfmpegResampler = resampler::CarrierResampler<View>;

/// The resampler, on the **owned** lane — every produced plane copied
/// out of the output frame.
#[cfg(feature = "resample")]
#[cfg_attr(docsrs, doc(cfg(feature = "resample")))]
pub type FfmpegOwnedResampler = resampler::CarrierResampler<Owned>;

pub use sample_format::SampleFormat;
pub use subtitle::{CarrierSubtitleStreamDecoder, SubtitleDecodeError};
pub use ticket::{ChannelLayoutTicket, CodecTicket, CustomChannel, Ratio};
pub use video::{CarrierVideoStreamDecoder, VideoDecodeError};

// Every bare alias below binds [`FfmpegBuffer`] in the `D` seat — the
// view lane, the ordinary road: a decoder's output read where it lands
// and dropped. The `Owned*` family binds [`FfmpegBytes`] and is what a
// payload takes when it has to **travel** — outlive the decoder, cross
// into a graph, be shared across threads.
//
// Each spells its carrier out rather than hiding it behind a neutral
// name. That is deliberate, and it is the one lesson 0.8's version of
// this block failed to teach: 0.8 also called this type `FfmpegBuffer`,
// but the D seat was *only* ever that type, so a consumer reading
// `VideoFrame` could not tell that holding one held libavcodec's memory
// open. It did. Naming both carriers in both families is what makes the
// question answerable at the use site — and [`FfmpegBytes`] answers it
// by being nothing of ours: owned, `Send + Sync`, no FFmpeg lifetime
// attached, the core's D-seat amputation contract satisfied by a type
// out of `alloc`.

/// Compressed video packet pre-parameterized with this crate's extras
/// and view carrier — the type [`FfmpegVideoStreamDecoder`] consumes
/// via [`mediadecode::decoder::VideoStreamDecoder::send_packet`].
pub type VideoPacket = mediadecode::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBuffer>;

/// Compressed audio packet pre-parameterized with this crate's extras
/// and view carrier.
pub type AudioPacket = mediadecode::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBuffer>;

/// Compressed subtitle packet pre-parameterized with this crate's
/// extras and view carrier.
pub type SubtitlePacket =
  mediadecode::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBuffer>;

/// Decoded video frame pre-parameterized with this crate's pixel
/// format / extras / view carrier.
///
/// Its planes are windows into the decoder's own frame buffer wherever
/// the geometry proves they may be — see the frame row of the lane
/// table in [`mediadecode::adapter`]. **A frame held is a pool slot
/// held**: on a hardware or fixed-pool decoder, retaining these past
/// the next `receive_frame` starves the decoder. Use [`OwnedVideoFrame`]
/// when a frame has to outlive the decode loop.
pub type VideoFrame =
  mediadecode::frame::VideoFrame<mediadecode::PixelFormat, extras::VideoFrameExtra, FfmpegBuffer>;

/// Decoded audio frame pre-parameterized with this crate's sample
/// format / channel layout / extras / view carrier.
///
/// Each plane is a window over exactly the samples the decoder wrote —
/// never the allocator's alignment padding past them.
pub type AudioFrame = mediadecode::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBuffer,
>;

/// Decoded subtitle frame pre-parameterized with this crate's
/// extras / view carrier.
pub type SubtitleFrame =
  mediadecode::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBuffer>;

/// Decoded still image pre-parameterized with this crate's pixel
/// format / extras / view carrier — what [`FfmpegImageDecoder`]
/// produces.
pub type ImageFrame =
  mediadecode::frame::ImageFrame<mediadecode::PixelFormat, extras::ImageFrameExtra, FfmpegBuffer>;

/// Timed opaque-data packet pre-parameterized with this crate's extras
/// and view carrier.
pub type DataPacket = mediadecode::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBuffer>;

/// Attachment payload pre-parameterized with this crate's extras and
/// view carrier — a font, or the cover art [`FfmpegImageDecoder`]
/// decodes.
pub type AttachmentPacket =
  mediadecode::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBuffer>;

/// The five-arm demux envelope [`FfmpegDemuxer`] delivers.
pub type DemuxedPacket = mediadecode::demuxer::DemuxedPacket<Ffmpeg, FfmpegBuffer>;

// --- The owned lane's alias family -----------------------------------
//
// The same shapes on the copying carrier, named explicitly because the
// bare names mean the view lane — the ordinary road for a direct
// consumer. Reach for these when a packet has to travel.
//
// Note what is **not** doubled: the `*Extra` types and `SideDataEntry`
// stay monomorphic on both lanes. Side data has no `AVBufferRef` to
// share — `AVPacketSideData` and `AVFrameSideData` payloads are plain
// allocations — so both lanes copy it, and a second family of extras
// would have been two names for one representation. The carrier
// parameter reaches the **payload**, not the annotations.

/// [`VideoPacket`] on the owned lane.
pub type OwnedVideoPacket = mediadecode::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBytes>;

/// [`AudioPacket`] on the owned lane.
pub type OwnedAudioPacket = mediadecode::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBytes>;

/// [`SubtitlePacket`] on the owned lane.
pub type OwnedSubtitlePacket =
  mediadecode::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBytes>;

/// [`DataPacket`] on the owned lane.
pub type OwnedDataPacket = mediadecode::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBytes>;

/// [`AttachmentPacket`] on the owned lane.
pub type OwnedAttachmentPacket =
  mediadecode::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBytes>;

/// [`DemuxedPacket`] on the owned lane.
pub type OwnedDemuxedPacket = mediadecode::demuxer::DemuxedPacket<Ffmpeg, FfmpegBytes>;

/// [`VideoFrame`] on the owned lane — planes copied out of the
/// decoder's buffer, so the frame outlives it and the pool slot goes
/// straight back.
pub type OwnedVideoFrame =
  mediadecode::frame::VideoFrame<mediadecode::PixelFormat, extras::VideoFrameExtra, FfmpegBytes>;

/// [`AudioFrame`] on the owned lane.
pub type OwnedAudioFrame = mediadecode::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBytes,
>;

/// [`SubtitleFrame`] on the owned lane.
pub type OwnedSubtitleFrame =
  mediadecode::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBytes>;

/// [`ImageFrame`] on the owned lane — what [`FfmpegOwnedImageDecoder`]
/// produces.
pub type OwnedImageFrame =
  mediadecode::frame::ImageFrame<mediadecode::PixelFormat, extras::ImageFrameExtra, FfmpegBytes>;

/// One row of the track table [`FfmpegDemuxer::tracks`] returns.
///
/// [`FfmpegDemuxer::tracks`]: mediadecode::demuxer::Demuxer::tracks
pub type TrackInfo = mediadecode::demuxer::TrackInfo<Ffmpeg>;

/// A track's per-kind codec parameters, as [`TrackInfo`] carries them.
pub type TrackParams = mediadecode::demuxer::TrackParams<Ffmpeg>;

/// Asserts a submission was taken, and answers nothing.
///
/// The `#[must_use]` on [`mediadecode::Sent`] is deliberate teeth: a
/// test that submits and ignores the answer is a test that would not
/// notice a decoder quietly asking to be drained. Most of this crate's
/// tests submit into a session they have just emptied, where
/// [`Sent::MustDrain`](mediadecode::Sent::MustDrain) is a real
/// surprise — so they say so here rather than dropping it.
#[cfg(test)]
#[track_caller]
pub(crate) fn accepted<E: core::fmt::Debug>(
  status: core::result::Result<mediadecode::Sent, E>,
  what: &str,
) {
  assert_eq!(
    status.unwrap_or_else(|e| panic!("{what}: {e:?}")),
    mediadecode::Sent::Accepted,
    "{what}: the session asked to be drained where the test expected room",
  );
}
