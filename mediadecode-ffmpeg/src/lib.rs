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
mod video;

pub use adapter::Ffmpeg;
pub use audio::{AudioDecodeError, FfmpegAudioStreamDecoder};
pub use backend::Backend;
pub use boundary::MediaKind;
pub use boundary::SendSideDataTooLarge;
pub use boundary::{
  PacketBuildError, SendPayloadTooLarge, attachment_packet_from_ffmpeg, audio_packet_from_ffmpeg,
  audio_packet_from_ffmpeg_in, data_packet_from_ffmpeg_in, empty_audio_frame, empty_subtitle_frame,
  empty_video_frame, from_av_pixel_format, is_hardware_pix_fmt, subtitle_packet_from_ffmpeg,
  subtitle_packet_from_ffmpeg_in, video_packet_from_ffmpeg, video_packet_from_ffmpeg_in,
};
pub use buffer::TrustedPayload;
pub use buffer::{FfmpegBytes, PacketBufferError};
pub use channel_layout::{
  channel_layout_description_from_ffmpeg, channel_layout_from_ffmpeg, channel_order_from_ffmpeg,
};
pub use codec_id::CodecId;
pub use decoder::VideoDecoder;
pub use demuxer::{DemuxError, FfmpegDemuxer, ProbeBudgetExhausted};
pub use error::{Error, Result};
pub use error::{FrameBudgetExceeded, FrameMedium, HwSurfaceTooLarge, HwTransferTooLarge};
pub use frame::Frame;
pub use image::{Corrupt, CorruptSource, FfmpegImageDecoder, ImageDecodeError, InputTooLarge};
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
  FfmpegResampler, OutputTooLarge, ResampleError, ResampleSpec, SpecEnd,
  UnsupportedChannelCount as UnsupportedSpecChannelCount,
};
pub use sample_format::SampleFormat;
pub use subtitle::{FfmpegSubtitleStreamDecoder, SubtitleDecodeError};
pub use video::{FfmpegVideoStreamDecoder, VideoDecodeError};

// Every alias below binds `FfmpegBytes` in the `D` seat, and
// each spells it out rather than hiding it behind an alias of this
// crate's own. That is deliberate: 0.8 had such an alias, it was
// called `FfmpegBuffer`, and the name was the problem — a consumer
// reading `VideoFrame` saw a type belonging to this crate and could
// not tell whether holding one held libavcodec's memory open. It did.
// `FfmpegBytes` answers the question by being nothing of ours: owned,
// `Send + Sync`, clone-is-a-refcount-bump, no FFmpeg lifetime
// attached — the core's D-seat amputation contract, satisfied by a
// type out of `alloc`.

/// Compressed video packet pre-parameterized with this crate's extras
/// and owned carrier — the type [`FfmpegVideoStreamDecoder`] consumes
/// via [`mediadecode::decoder::VideoStreamDecoder::send_packet`].
pub type VideoPacket = mediadecode::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBytes>;

/// Compressed audio packet pre-parameterized with this crate's extras
/// and owned carrier.
pub type AudioPacket = mediadecode::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBytes>;

/// Compressed subtitle packet pre-parameterized with this crate's
/// extras and owned carrier.
pub type SubtitlePacket =
  mediadecode::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBytes>;

/// Decoded video frame pre-parameterized with this crate's pixel
/// format / extras / owned carrier.
pub type VideoFrame =
  mediadecode::frame::VideoFrame<mediadecode::PixelFormat, extras::VideoFrameExtra, FfmpegBytes>;

/// Decoded audio frame pre-parameterized with this crate's sample
/// format / channel layout / extras / owned carrier.
pub type AudioFrame = mediadecode::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBytes,
>;

/// Decoded subtitle frame pre-parameterized with this crate's
/// extras / owned carrier.
pub type SubtitleFrame = mediadecode::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBytes>;

/// Decoded still image pre-parameterized with this crate's pixel
/// format / extras / owned carrier — what [`FfmpegImageDecoder`]
/// produces.
pub type ImageFrame =
  mediadecode::frame::ImageFrame<mediadecode::PixelFormat, extras::ImageFrameExtra, FfmpegBytes>;

/// Timed opaque-data packet pre-parameterized with this crate's extras
/// and owned carrier.
pub type DataPacket = mediadecode::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBytes>;

/// Attachment payload pre-parameterized with this crate's extras and
/// owned carrier — a font, or the cover art [`FfmpegImageDecoder`]
/// decodes.
pub type AttachmentPacket =
  mediadecode::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBytes>;

/// The five-arm demux envelope [`FfmpegDemuxer`] delivers.
pub type DemuxedPacket = mediadecode::demuxer::DemuxedPacket<Ffmpeg, FfmpegBytes>;

/// One row of the track table [`FfmpegDemuxer::tracks`] returns.
///
/// [`FfmpegDemuxer::tracks`]: mediadecode::demuxer::Demuxer::tracks
pub type TrackInfo = mediadecode::demuxer::TrackInfo<Ffmpeg>;

/// A track's per-kind codec parameters, as [`TrackInfo`] carries them.
pub type TrackParams = mediadecode::demuxer::TrackParams<Ffmpeg>;
