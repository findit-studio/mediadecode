#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod catalogue;
pub mod mux;

pub use catalogue::{Admitted, CATALOGUE, EncoderRefusal, Entry, License, encoder_by_name, lookup};
pub use mux::{
  CarrierMuxer, ContainerName, FfmpegMuxRefusal, FfmpegMuxer, FfmpegOwnedMuxer, MuxFault,
  MuxOpenError, MuxReport, MuxStage, Uncarried,
};
// What a fault and a refusal name, from the base's muxing context.
pub use mediaio_ffmpeg::output::{OutputCall, OutputError, OutputPhase, OutputText};

// The faces this adapter implements, and the vocabulary they speak.
pub use mediaencode;
