//! The path-compatibility law: every `mediadecode` path mediagraph names
//! still resolves, by its old name, now that the vocabulary lives in
//! `mediaspine` and this crate re-exports it beside its read faces.
//!
//! The census is mediagraph's `main` at `a6dc129c` (the merge of
//! `follow05`): its 83 `mediadecode` paths in code, grouped imports
//! expanded. One line here per path, so a path that stops resolving
//! fails this target by name at compile time. A generic path is named
//! through `Loop`, a loopback adapter; a trait's path is named by
//! implementing it or by bounding on it.
//!
//! The last block is the stronger half of the law: a re-exported name is
//! not a look-alike, it is `mediaspine`'s own item, so code that mixes
//! the two crates' paths for one value still type-checks.

#![allow(clippy::type_complexity)]

use core::marker::PhantomData;

#[allow(unused_imports)]
use mediadecode::{demuxer, frame};

/// The loopback adapter the generic paths are named through; its impls
/// name the four adapter traits' paths.
struct Loop;

impl mediadecode::adapter::VideoAdapter for Loop {
  type CodecId = u32;
  type PixelFormat = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

impl mediadecode::adapter::AudioAdapter for Loop {
  type CodecId = u32;
  type SampleFormat = u32;
  type ChannelLayout = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

impl mediadecode::adapter::SubtitleAdapter for Loop {
  type CodecId = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

impl mediadecode::demuxer::DemuxAdapter for Loop {
  type CodecId = u32;
  type Video = Loop;
  type Audio = Loop;
  type Subtitle = Loop;
  type DataExtra = ();
  type AttachmentExtra = ();
  type TrackExtra = ();
  type Text = &'static str;
}

type Bytes = &'static [u8];

/// The read faces' paths, named by bounding on them.
#[allow(dead_code)]
fn read_faces<
  A: mediadecode::decoder::AudioStreamDecoder,
  I: mediadecode::decoder::ImageDecoder,
  S: mediadecode::decoder::SubtitleDecoder,
  V: mediadecode::decoder::VideoStreamDecoder,
  D: mediadecode::demuxer::Demuxer,
  R: mediadecode::resampler::AudioResampler,
>() {
  let _ = <I as mediadecode::decoder::ImageDecoder>::decode;
  let _ = <D as mediadecode::demuxer::Demuxer>::tracks;
}

#[test]
fn every_path_mediagraph_names_still_resolves() {
  // The root.
  let _: Option<mediadecode::PixelFormat> = None;
  let _: Option<mediadecode::Received> = None;
  let _: Option<mediadecode::Sent> = None;
  let _: Option<mediadecode::Timebase> = None;
  let _ = mediadecode::Timebase::new;
  let _: Option<mediadecode::Timestamp> = None;

  // `color`.
  let _: Option<mediadecode::color::ColorInfo> = None;
  let _: Option<mediadecode::color::ColorMatrix> = None;
  let _: Option<mediadecode::color::ColorRange> = None;
  let _: Option<mediadecode::color::ColorTransfer> = None;

  // `decoder`: the bounds in `read_faces` above, and the capability word.
  let _: Option<mediadecode::decoder::ScaledOutputCapability> = None;

  // `demuxer`.
  let _: Option<mediadecode::demuxer::AttachmentPacket<(), Bytes>> = None;
  let _: Option<mediadecode::demuxer::AttachmentTrackPacket<Loop, Bytes>> = None;
  let _: Option<mediadecode::demuxer::AttachmentTrackParams<Loop>> = None;
  let _: Option<mediadecode::demuxer::AudioTrackPacket<Loop, Bytes>> = None;
  let _: Option<mediadecode::demuxer::AudioTrackParams<Loop>> = None;
  let _ = mediadecode::demuxer::AudioTrackParams::<Loop>::new;
  let _: Option<mediadecode::demuxer::Chapter<Loop>> = None;
  let _ = mediadecode::demuxer::Chapter::<Loop>::new;
  let _: Option<mediadecode::demuxer::ContainerDuration> = None;
  let _: Option<mediadecode::demuxer::DataPacket<(), Bytes>> = None;
  let _: Option<mediadecode::demuxer::DataTrackPacket<Loop, Bytes>> = None;
  let _: Option<mediadecode::demuxer::DataTrackParams<Loop>> = None;
  let _: Option<mediadecode::demuxer::DemuxedPacket<Loop, Bytes>> = None;
  let _ = mediadecode::demuxer::DemuxedPacket::<Loop, Bytes>::Video;
  let _: Option<mediadecode::demuxer::DurationSource> = None;
  let _ = mediadecode::demuxer::DurationSource::FromBitrate;
  let _ = mediadecode::demuxer::DurationSource::FromPts;
  let _ = mediadecode::demuxer::DurationSource::FromStream;
  let _ = mediadecode::demuxer::DurationSource::Unknown;
  let _: Option<mediadecode::demuxer::MeasuredEnd> = None;
  let _: Option<mediadecode::demuxer::SubtitleTrackPacket<Loop, Bytes>> = None;
  let _: Option<mediadecode::demuxer::SubtitleTrackParams<Loop>> = None;
  let _: Option<mediadecode::demuxer::TrackIndex> = None;
  let _ = mediadecode::demuxer::TrackIndex::new;
  let _: Option<mediadecode::demuxer::TrackInfo<Loop>> = None;
  let _: Option<mediadecode::demuxer::TrackKind> = None;
  let _ = mediadecode::demuxer::TrackKind::Attachment;
  let _ = mediadecode::demuxer::TrackKind::Audio;
  let _ = mediadecode::demuxer::TrackKind::Data;
  let _ = mediadecode::demuxer::TrackKind::Subtitle;
  let _ = mediadecode::demuxer::TrackKind::Unknown;
  let _ = mediadecode::demuxer::TrackKind::Video;
  let _: Option<mediadecode::demuxer::TrackParams<Loop>> = None;
  let _ = mediadecode::demuxer::TrackParams::<Loop>::Attachment;
  let _ = mediadecode::demuxer::TrackParams::<Loop>::Audio;
  let _ = mediadecode::demuxer::TrackParams::<Loop>::Data;
  let _ = mediadecode::demuxer::TrackParams::<Loop>::Subtitle;
  let _ = mediadecode::demuxer::TrackParams::<Loop>::Video;
  let _: Option<mediadecode::demuxer::UnknownTrackParams<Loop>> = None;
  let _: Option<mediadecode::demuxer::VideoTrackPacket<Loop, Bytes>> = None;
  let _: Option<mediadecode::demuxer::VideoTrackParams<Loop>> = None;

  // `frame`.
  let _: Option<mediadecode::frame::AudioFrame<u32, u32, (), Bytes>> = None;
  let _: Option<mediadecode::frame::Dimensions> = None;
  let _ = mediadecode::frame::Dimensions::new;
  let _: Option<mediadecode::frame::Plane<Bytes>> = None;
  let _ = mediadecode::frame::Plane::<Bytes>::new;
  let _: Option<mediadecode::frame::Rect> = None;
  let _: Option<mediadecode::frame::SubtitleFrame<(), Bytes>> = None;
  let _: Option<mediadecode::frame::VideoFrame<u32, (), Bytes>> = None;

  // `packet`.
  let _: Option<mediadecode::packet::AudioPacket<(), Bytes>> = None;
  let _: Option<mediadecode::packet::PacketFlags> = None;
  let _ = mediadecode::packet::PacketFlags::KEY;
  let _: Option<mediadecode::packet::SubtitlePacket<(), Bytes>> = None;
  let _: Option<mediadecode::packet::VideoPacket<(), Bytes>> = None;

  // `resampler`: the face is bounded on in `read_faces` above.
  let _: Option<mediadecode::resampler::AdapterAudioFrame<Loop, Bytes>> = None;

  // `subtitle`.
  let _: Option<mediadecode::subtitle::Bitmap<Bytes>> = None;
  let _: Option<mediadecode::subtitle::BitmapRegion<Bytes>> = None;
  let _: Option<mediadecode::subtitle::SubtitlePayload<Bytes>> = None;
  let _: Option<mediadecode::subtitle::Text<Bytes>> = None;
}

/// Compiles only where both arguments are one type.
fn same<T>(_: PhantomData<T>, _: PhantomData<T>) {}

fn spine_video_adapter<T: mediaspine::adapter::VideoAdapter>() {}
fn spine_demux_adapter<T: mediaspine::demuxer::DemuxAdapter>() {}

#[test]
fn a_reexported_name_is_the_vocabularys_own_item() {
  same(
    PhantomData::<mediadecode::Received>,
    PhantomData::<mediaspine::Received>,
  );
  same(
    PhantomData::<mediadecode::Sent>,
    PhantomData::<mediaspine::Sent>,
  );
  same(
    PhantomData::<mediadecode::PixelFormat>,
    PhantomData::<mediaspine::PixelFormat>,
  );
  same(
    PhantomData::<mediadecode::Timebase>,
    PhantomData::<mediaspine::Timebase>,
  );
  same(
    PhantomData::<mediadecode::color::ColorInfo>,
    PhantomData::<mediaspine::color::ColorInfo>,
  );
  same(
    PhantomData::<mediadecode::frame::VideoFrame<u32, (), Bytes>>,
    PhantomData::<mediaspine::frame::VideoFrame<u32, (), Bytes>>,
  );
  same(
    PhantomData::<mediadecode::packet::PacketFlags>,
    PhantomData::<mediaspine::packet::PacketFlags>,
  );
  same(
    PhantomData::<mediadecode::demuxer::TrackInfo<Loop>>,
    PhantomData::<mediaspine::demuxer::TrackInfo<Loop>>,
  );
  same(
    PhantomData::<mediadecode::subtitle::SubtitlePayload<Bytes>>,
    PhantomData::<mediaspine::subtitle::SubtitlePayload<Bytes>>,
  );
  same(
    PhantomData::<mediadecode::resampler::AdapterAudioFrame<Loop, Bytes>>,
    PhantomData::<mediaspine::resampler::AdapterAudioFrame<Loop, Bytes>>,
  );
  // The traits too: `Loop` implements them through `mediadecode` paths,
  // and satisfies bounds spelled through `mediaspine`.
  spine_video_adapter::<Loop>();
  spine_demux_adapter::<Loop>();
}
