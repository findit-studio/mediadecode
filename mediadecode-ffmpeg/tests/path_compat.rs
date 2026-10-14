//! The path-compatibility law: every `mediadecode_ffmpeg` path mediagraph
//! names still resolves, by its old name, now that the crossings live in
//! `mediaio-ffmpeg` and this crate re-exports them.
//!
//! The census is mediagraph's `main` at `a6dc129c` (the merge of
//! `follow05`): its 39 paths in code — grouped imports expanded — and the
//! three more its documentation links name. One line here per path, so a
//! path that stops resolving fails this target by name at compile time;
//! the test body only gives the target something to run.
//!
//! What this law does not cover is a path whose *signature* changed:
//! `TrackExtra::clone_parameters`, `CodecTicket::mirror` and
//! `CodecTicket::rebuild` now answer the base's `ParametersError`,
//! `Frame::empty` and `ResampleError::Resample` the base's
//! `CrossingError`. Those sites are the dependants' follow-up, named in
//! the pull request.

#[test]
fn every_path_mediagraph_names_still_resolves() {
  // Types and type aliases, in census order.
  let _: Option<mediadecode_ffmpeg::AudioDecodeError> = None;
  let _: Option<mediadecode_ffmpeg::Backend> = None;
  let _: Option<mediadecode_ffmpeg::CodecId> = None;
  let _: Option<mediadecode_ffmpeg::DecodePath> = None;
  let _: Option<mediadecode_ffmpeg::DecoderLimits> = None;
  let _: Option<mediadecode_ffmpeg::DemuxError> = None;
  let _: Option<mediadecode_ffmpeg::Error> = None;
  let _: Option<mediadecode_ffmpeg::Ffmpeg> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegBuffer> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegBytes> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegOwnedAudioStreamDecoder> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegOwnedDemuxer> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegOwnedImageDecoder> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegOwnedSubtitleStreamDecoder> = None;
  let _: Option<mediadecode_ffmpeg::FfmpegOwnedVideoStreamDecoder> = None;
  let _: Option<mediadecode_ffmpeg::FrameLimits> = None;
  let _: Option<mediadecode_ffmpeg::ImageDecodeError> = None;
  let _: Option<mediadecode_ffmpeg::OwnedAttachmentPacket> = None;
  let _: Option<mediadecode_ffmpeg::OwnedAudioFrame> = None;
  let _: Option<mediadecode_ffmpeg::OwnedAudioPacket> = None;
  let _: Option<mediadecode_ffmpeg::OwnedImageFrame> = None;
  let _: Option<mediadecode_ffmpeg::OwnedSubtitleFrame> = None;
  let _: Option<mediadecode_ffmpeg::OwnedSubtitlePacket> = None;
  let _: Option<mediadecode_ffmpeg::OwnedVideoFrame> = None;
  let _: Option<mediadecode_ffmpeg::OwnedVideoPacket> = None;
  let _: Option<mediadecode_ffmpeg::SampleFormat> = None;
  let _: Option<mediadecode_ffmpeg::SubtitleDecodeError> = None;

  // Functions, associated functions and constants.
  let _ = mediadecode_ffmpeg::channel_layout_description_from_ffmpeg;
  let _ = mediadecode_ffmpeg::empty_owned_audio_frame;
  let _ = mediadecode_ffmpeg::empty_owned_subtitle_frame;
  let _ = mediadecode_ffmpeg::empty_owned_video_frame;
  let _ = mediadecode_ffmpeg::extras::AudioFrameExtra::default;
  let _ = mediadecode_ffmpeg::FfmpegOwnedDemuxer::open_reader::<std::io::Cursor<Vec<u8>>>;
  let _ = mediadecode_ffmpeg::SampleFormat::FLT;

  // The three its documentation links name.
  let _: Option<mediadecode_ffmpeg::CodecTicket> = None;
  let _ = mediadecode_ffmpeg::ticket::CodecTicket::extradata;
  let _: Option<mediadecode_ffmpeg::VideoDecoder> = None;

  // The resampler's four, behind the feature that links `swresample`.
  #[cfg(feature = "resample")]
  {
    let _: Option<mediadecode_ffmpeg::FfmpegOwnedResampler> = None;
    let _ = mediadecode_ffmpeg::FfmpegOwnedResampler::new;
    let _: Option<mediadecode_ffmpeg::ResampleError> = None;
    let _: Option<mediadecode_ffmpeg::ResampleSpec> = None;
    let _: Option<mediadecode_ffmpeg::SpecEnd> = None;
  }
}
