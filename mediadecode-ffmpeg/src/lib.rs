//! FFmpeg adapter for the [`mediadecode`] abstraction layer.
//!
//! Implements [`mediadecode::adapter::VideoAdapter`],
//! [`mediadecode::adapter::AudioAdapter`], and
//! [`mediadecode::adapter::SubtitleAdapter`] for the [`Ffmpeg`] zero-
//! sized type, plus the matching push-style decoder traits. Frame
//! payloads are zero-copy refcounted views of FFmpeg's `AVBufferRef`
//! via the [`FfmpegBuffer`] type.
//!
//! ## Video path
//!
//! [`FfmpegVideoStreamDecoder`] implements
//! [`mediadecode::decoder::VideoStreamDecoder`] over the internal
//! [`VideoDecoder`] (the HW-probe wrapper carried over from this
//! crate's `hwdecode` ancestry). The probe walks
//! VideoToolbox / VAAPI / NVDEC / D3D11VA in platform-appropriate
//! order; [`Error::AllBackendsFailed`] surfaces when every candidate
//! is exhausted. Software fallback (opening
//! `ffmpeg::decoder::Video` directly) is the next follow-up — the
//! caller can use the rescued `unconsumed_packets` to drive their
//! own software decoder today.
//!
//! Frames are converted to
//! `mediadecode::frame::VideoFrame<Ffmpeg, FfmpegBuffer>` via
//! [`crate::convert::av_frame_to_video_frame`], which packs each
//! plane as an `FfmpegBuffer` view into the source `AVFrame`'s
//! `AVBufferRef`s — no copying, refcount-tracked.
//!
//! ## Audio and subtitle paths
//!
//! [`FfmpegAudioStreamDecoder`] and [`FfmpegSubtitleDecoder`] are
//! stubbed in this revision — trait impls compile, methods return
//! a structured `NotImplemented` error. The decode loops will land
//! in follow-up commits.
//!
//! ## Format identifiers
//!
//! The newtypes [`CodecId`], [`PixelFormat`], [`SampleFormat`], and
//! [`ChannelLayout`] wrap the relevant FFmpeg integers (the same
//! safety stance as the original `pix_fmt` module: never transmute
//! back into a bindgen enum). Each carries a `pub const` set of
//! constants for the most common values.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]

mod adapter;
mod audio;
mod backend;
mod buffer;
mod channel_layout;
mod codec_id;
pub mod convert;
mod decoder;
mod error;
pub mod extras;
mod ffi;
mod frame;
pub mod pix_fmt;
mod sample_format;
mod subtitle;
mod video;

pub use adapter::Ffmpeg;
pub use audio::{AudioDecodeError, FfmpegAudioStreamDecoder};
pub use backend::Backend;
pub use buffer::FfmpegBuffer;
pub use channel_layout::ChannelLayout;
pub use codec_id::CodecId;
pub use decoder::VideoDecoder;
pub use error::{Error, Result};
pub use frame::Frame;
pub use pix_fmt::PixelFormat;
pub use sample_format::SampleFormat;
pub use subtitle::{FfmpegSubtitleDecoder, SubtitleDecodeError};
pub use video::{FfmpegVideoStreamDecoder, VideoDecodeError};
