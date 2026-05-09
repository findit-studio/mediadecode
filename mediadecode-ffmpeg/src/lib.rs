//! FFmpeg adapter for the [`mediadecode`] abstraction layer.
//!
//! Implements [`mediadecode::adapter::VideoAdapter`],
//! [`mediadecode::adapter::AudioAdapter`], and
//! [`mediadecode::adapter::SubtitleAdapter`] for the [`Ffmpeg`] zero-
//! sized type, plus the matching push-style decoder traits. Frame
//! payloads are zero-copy refcounted views of FFmpeg's `AVBufferRef`
//! via the [`FfmpegBuffer`] type.
//!
//! # Quick start
//!
//! For most callers this crate is used through three pieces:
//!
//! 1. **A decoder type** — [`FfmpegVideoStreamDecoder`],
//!    [`FfmpegAudioStreamDecoder`], or [`FfmpegSubtitleStreamDecoder`]
//!    — each implementing the matching `mediadecode` decoder trait.
//! 2. **Safe packet wrappers** — [`video_packet_from_ffmpeg`],
//!    [`audio_packet_from_ffmpeg`], [`subtitle_packet_from_ffmpeg`]
//!    — convert a borrowed `ffmpeg::Packet` (the demuxer output)
//!    into the matching mediadecode packet without copying the
//!    compressed payload.
//! 3. **Empty-frame builders** — [`empty_video_frame`],
//!    [`empty_audio_frame`], [`empty_subtitle_frame`] — produce a
//!    well-formed destination for a decoder's `receive_frame` call.
//!
//! ```no_run
//! use mediadecode::{Timebase, decoder::VideoStreamDecoder};
//! use mediadecode_ffmpeg::{
//!   FfmpegVideoStreamDecoder, empty_video_frame, video_packet_from_ffmpeg,
//! };
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! # let parameters: ffmpeg_next::codec::Parameters = unimplemented!();
//! # let time_base: Timebase = unimplemented!();
//! # let av_packet: ffmpeg_next::Packet = unimplemented!();
//! let mut decoder = FfmpegVideoStreamDecoder::open(parameters, time_base)?;
//! let mut dst = empty_video_frame();
//! if let Some(packet) = video_packet_from_ffmpeg(&av_packet) {
//!   decoder.send_packet(&packet)?;
//!   if decoder.receive_frame(&mut dst).is_ok() {
//!     println!("{}x{} {}", dst.width(), dst.height(), dst.pixel_format());
//!   }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Public surface map
//!
//! - **Decoders**: [`FfmpegVideoStreamDecoder`],
//!   [`FfmpegAudioStreamDecoder`], [`FfmpegSubtitleStreamDecoder`].
//!   Plus their error types: [`VideoDecodeError`], [`AudioDecodeError`],
//!   [`SubtitleDecodeError`].
//! - **Type aliases**: [`VideoPacket`], [`AudioPacket`],
//!   [`SubtitlePacket`], [`VideoFrame`], [`AudioFrame`], [`SubtitleFrame`]
//!   — the mediadecode generic types pre-parameterized with this
//!   crate's adapter / buffer / extras, so you don't have to spell
//!   them out.
//! - **Buffer**: [`FfmpegBuffer`] (refcounted view over an
//!   `AVBufferRef`) with safe constructors
//!   ([`FfmpegBuffer::empty`], [`FfmpegBuffer::from_packet`],
//!   [`FfmpegBuffer::from_frame_plane`], [`FfmpegBuffer::copy_from_slice`])
//!   plus low-level unsafe constructors for advanced use.
//! - **Boundary helpers** ([`boundary`] module): packet wrappers
//!   ([`video_packet_from_ffmpeg`], [`audio_packet_from_ffmpeg`],
//!   [`subtitle_packet_from_ffmpeg`]), empty-frame builders
//!   ([`empty_video_frame`], [`empty_audio_frame`],
//!   [`empty_subtitle_frame`]), pixel-format mapping
//!   ([`from_av_pixel_format`], [`is_hardware_pix_fmt`]).
//! - **Frame conversion** ([`convert`] module): safe entry points
//!   [`convert::video_frame_from`], [`convert::audio_frame_from`],
//!   [`convert::subtitle_frame_from`] (taking borrowed
//!   `ffmpeg::Frame` / `Subtitle`); plus unsafe pointer-based
//!   variants for callers driving FFmpeg directly.
//! - **Channel-layout mapping** ([`channel_layout`] module).
//! - **Format identifiers**: [`CodecId`], [`SampleFormat`].
//!   `mediadecode::PixelFormat` is the unified pixel format and is
//!   re-exported through [`VideoFrame`].
//! - **Adapter binding**: [`Ffmpeg`] (zero-sized type satisfying the
//!   three `mediadecode` adapter traits).
//! - **Backend probe**: [`Backend`], [`VideoDecoder`] (the lower-level
//!   HW-probe wrapper underneath [`FfmpegVideoStreamDecoder`]).
//! - **Per-packet / per-frame extras**: [`extras`] module.
//!
//! # Safety stance
//!
//! The crate distinguishes:
//!
//! - **Safe API** (default): typed, refcount-tracked, no raw
//!   pointers in or out. Examples: [`FfmpegVideoStreamDecoder::open`],
//!   [`video_packet_from_ffmpeg`], [`convert::video_frame_from`],
//!   [`FfmpegBuffer::from_packet`].
//! - **Unsafe API** (advanced): raw `*const AVFrame` /
//!   `*const AVSubtitle` / `*mut AVBufferRef` ins and outs. Use these
//!   only when you're driving FFmpeg directly and need to bridge
//!   without copying. Examples: [`convert::av_frame_to_video_frame`],
//!   [`FfmpegBuffer::take`].
//!
//! Both surfaces compose: every safe entry point is implemented in
//! terms of the corresponding unsafe one.
//!
//! # Panic-free entry points
//!
//! A handful of safe constructors panic on FFmpeg-side OOM (the
//! 1-byte placeholder allocations behind frame slots, and `Clone`'s
//! refcount bump). Each has a `try_*` counterpart that returns
//! `Option<T>` for callers running in OOM-recoverable contexts:
//!
//! | Panicking                  | Fallible                       |
//! | -------------------------- | ------------------------------ |
//! | [`FfmpegBuffer::empty`]    | [`FfmpegBuffer::try_empty`]    |
//! | `<FfmpegBuffer as Clone>::clone` | [`FfmpegBuffer::try_clone`] |
//! | [`empty_video_frame`]      | [`try_empty_video_frame`]      |
//! | [`empty_audio_frame`]      | [`try_empty_audio_frame`]      |
//! | [`empty_subtitle_frame`]   | [`try_empty_subtitle_frame`]   |
//! | [`Frame::stride`]          | [`Frame::try_stride`]          |
//!
//! All other public APIs already return `Result` for the failure
//! modes that can come from input data, FFmpeg state, or the OS
//! (decoder open / send_packet / receive_frame / convert::*).
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
mod error;
pub mod extras;
mod ffi;
mod frame;
mod sample_format;
mod subtitle;
mod video;

pub use adapter::Ffmpeg;
pub use audio::{AudioDecodeError, FfmpegAudioStreamDecoder};
pub use backend::Backend;
pub use boundary::{
  audio_packet_from_ffmpeg, empty_audio_frame, empty_subtitle_frame, empty_video_frame,
  from_av_pixel_format, is_hardware_pix_fmt, subtitle_packet_from_ffmpeg, try_empty_audio_frame,
  try_empty_subtitle_frame, try_empty_video_frame, video_packet_from_ffmpeg,
};
pub use buffer::FfmpegBuffer;
pub use channel_layout::{
  audio_channel_layout_from_ffmpeg, audio_channel_order_kind_from_ffmpeg,
  channel_layout_kind_from_ffmpeg,
};
pub use codec_id::CodecId;
pub use decoder::VideoDecoder;
pub use error::{Error, Result};
pub use frame::Frame;
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
  mediadecode::channel::AudioChannelLayout,
  extras::AudioFrameExtra,
  FfmpegBuffer,
>;

/// Decoded subtitle frame pre-parameterized with this crate's
/// extras / refcounted buffer.
pub type SubtitleFrame =
  mediadecode::frame::SubtitleFrame<extras::SubtitleFrameExtra, FfmpegBuffer>;
