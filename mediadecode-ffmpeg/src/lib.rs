#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

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
mod ffi;
mod frame;
mod pixdesc;
mod reader_guard;
mod resampler;
mod sample_format;
mod subtitle;
mod video;

pub use adapter::Ffmpeg;
pub use audio::{AudioDecodeError, FfmpegAudioStreamDecoder};
pub use backend::Backend;
pub use boundary::{
  PacketBuildError, attachment_packet_from_ffmpeg, audio_packet_from_ffmpeg,
  audio_packet_from_ffmpeg_in, data_packet_from_ffmpeg_in, empty_audio_frame, empty_subtitle_frame,
  empty_video_frame, from_av_pixel_format, is_hardware_pix_fmt, subtitle_packet_from_ffmpeg,
  subtitle_packet_from_ffmpeg_in, try_empty_audio_frame, try_empty_subtitle_frame,
  try_empty_video_frame, video_packet_from_ffmpeg, video_packet_from_ffmpeg_in,
};
pub use buffer::{FfmpegBuffer, PacketBufferError};
pub use channel_layout::{
  channel_layout_description_from_ffmpeg, channel_layout_from_ffmpeg, channel_order_from_ffmpeg,
};
pub use codec_id::CodecId;
pub use decoder::VideoDecoder;
pub use demuxer::{DemuxError, FfmpegDemuxer};
pub use error::{Error, Result};
pub use frame::Frame;
pub use resampler::{FfmpegResampler, ResampleError, ResampleSpec, SpecEnd};
pub use sample_format::SampleFormat;
pub use subtitle::{FfmpegSubtitleStreamDecoder, SubtitleDecodeError};
pub use video::{FfmpegVideoStreamDecoder, VideoDecodeError};

/// Compressed video packet pre-parameterized with this crate's
/// extras and refcounted buffer — the type
/// [`FfmpegVideoStreamDecoder`] consumes via
/// [`mediadecode::decoder::VideoStreamDecoder::send_packet`].
pub type VideoPacket = mediadecode::packet::VideoPacket<extras::VideoPacketExtra, FfmpegBuffer>;

/// Compressed audio packet pre-parameterized with this crate's extras
/// and refcounted buffer.
pub type AudioPacket = mediadecode::packet::AudioPacket<extras::AudioPacketExtra, FfmpegBuffer>;

/// Compressed subtitle packet pre-parameterized with this crate's
/// extras and refcounted buffer.
pub type SubtitlePacket =
  mediadecode::packet::SubtitlePacket<extras::SubtitlePacketExtra, FfmpegBuffer>;

/// Decoded video frame pre-parameterized with this crate's pixel
/// format / extras / refcounted buffer.
pub type VideoFrame =
  mediadecode::frame::VideoFrame<mediadecode::PixelFormat, extras::VideoFrameExtra, FfmpegBuffer>;

/// Decoded audio frame pre-parameterized with this crate's sample
/// format / channel layout / extras / refcounted buffer.
pub type AudioFrame = mediadecode::frame::AudioFrame<
  SampleFormat,
  mediaframe::audio::ChannelLayoutDescription,
  extras::AudioFrameExtra,
  FfmpegBuffer,
>;

/// Decoded subtitle frame pre-parameterized with this crate's
/// extras / refcounted buffer.
pub type SubtitleFrame =
  mediadecode::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBuffer>;

/// Timed opaque-data packet pre-parameterized with this crate's extras
/// and refcounted buffer.
pub type DataPacket = mediadecode::demuxer::DataPacket<extras::DataPacketExtra, FfmpegBuffer>;

/// Attachment payload pre-parameterized with this crate's extras and
/// refcounted buffer.
pub type AttachmentPacket =
  mediadecode::demuxer::AttachmentPacket<extras::AttachmentPacketExtra, FfmpegBuffer>;

/// The five-arm demux envelope [`FfmpegDemuxer`] delivers.
pub type DemuxedPacket = mediadecode::demuxer::DemuxedPacket<Ffmpeg, FfmpegBuffer>;

/// One row of the track table [`FfmpegDemuxer::tracks`] returns.
///
/// [`FfmpegDemuxer::tracks`]: mediadecode::demuxer::Demuxer::tracks
pub type TrackInfo = mediadecode::demuxer::TrackInfo<Ffmpeg>;

/// A track's per-kind codec parameters, as [`TrackInfo`] carries them.
pub type TrackParams = mediadecode::demuxer::TrackParams<Ffmpeg>;
