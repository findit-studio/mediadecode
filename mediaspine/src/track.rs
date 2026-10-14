//! What a muxer needs to open one output track.
//!
//! A demuxer describes the tracks it reads with [`TrackInfo`]; a muxer
//! has to be told the same facts about each track it writes, and a few
//! more that a decoder never needed. [`TrackDescription`] is that
//! handoff: the codec and its per-kind parameters, the timebase the
//! track's packets are stamped in, the codec's configuration bytes
//! (*extradata* — an `avcC`, `hvcC` or `esds` record, a Vorbis or Opus
//! header), and the side data that travels with the stream.
//!
//! It is filled from either end of a pipeline: from a demuxed track,
//! for a remux that copies packets, or from an encoder's output, for a
//! track that is encoded fresh.
//!
//! [`TrackInfo`]: crate::demuxer::TrackInfo

use core::fmt::{self, Debug};

use crate::{
  Timebase,
  demuxer::{DemuxAdapter, TrackKind, TrackParams},
};

/// The description of one track a muxer writes.
///
/// Generic over the adapter `E` (whose vocabulary names the codec and
/// the per-kind parameters), the byte carrier `D` of the extradata, and
/// the side-data carrier `S`, which is the backend's own choice — a
/// list of typed entries for one backend, a borrowed table for another.
/// Nothing here allocates.
///
/// The kind and the codec are read off [`params`](Self::params), never
/// stored beside it, so a description cannot disagree with itself.
///
/// No `Clone`, for the reason [`TrackInfo`](crate::demuxer::TrackInfo)
/// has none: the parameters are not cheap to duplicate, and a consumer
/// that needs to share a description shares a handle on it.
pub struct TrackDescription<E: DemuxAdapter, D, S> {
  timebase: Timebase,
  params: TrackParams<E>,
  extradata: Option<D>,
  side_data: S,
}

impl<E: DemuxAdapter, D, S> TrackDescription<E, D, S> {
  /// Constructs a description with no extradata.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(timebase: Timebase, params: TrackParams<E>, side_data: S) -> Self {
    Self {
      timebase,
      params,
      extradata: None,
      side_data,
    }
  }

  /// Returns the track's kind, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    self.params.kind()
  }

  /// Returns the codec identifier, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.params.codec()
  }

  /// Returns the timebase every timestamp on this track is expressed in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }

  /// Returns the codec and its per-kind parameters.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn params(&self) -> &TrackParams<E> {
    &self.params
  }

  /// Returns the codec's configuration bytes, or `None` where the codec
  /// carries none (a stream that configures itself in band).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extradata(&self) -> Option<&D> {
    self.extradata.as_ref()
  }

  /// Returns the side data that travels with the stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn side_data(&self) -> &S {
    &self.side_data
  }

  /// Returns the side data mutably, for a caller that adds an entry.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data_mut(&mut self) -> &mut S {
    &mut self.side_data
  }

  /// Sets the extradata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_extradata(mut self, value: Option<D>) -> Self {
    self.extradata = value;
    self
  }

  /// Sets the extradata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_extradata(&mut self, value: Option<D>) -> &mut Self {
    self.extradata = value;
    self
  }

  /// Splits the description into its timebase, parameters, extradata
  /// and side data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (Timebase, TrackParams<E>, Option<D>, S) {
    (self.timebase, self.params, self.extradata, self.side_data)
  }
}

// `Debug` is hand-written for the reason `TrackInfo`'s is: a derive
// would demand `E: Debug`, which no field needs — the parameters carry
// their own bounds through the adapter's associated types.
impl<E: DemuxAdapter, D: Debug, S: Debug> Debug for TrackDescription<E, D, S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TrackDescription")
      .field("timebase", &self.timebase)
      .field("params", &self.params)
      .field("extradata", &self.extradata)
      .field("side_data", &self.side_data)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use super::*;
  use crate::{
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    demuxer::{AudioTrackParams, VideoTrackParams},
  };

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct Loopback;
  impl DemuxAdapter for Loopback {
    type CodecId = u32;
    type Video = VLoop;
    type Audio = ALoop;
    type Subtitle = SLoop;
    type DataExtra = ();
    type AttachmentExtra = ();
    type TrackExtra = ();
    type Text = &'static str;
  }

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
  }

  /// The kind and the codec are the parameters' own, so there is no
  /// way to build a description that disagrees with itself.
  #[test]
  fn the_kind_and_the_codec_are_read_off_the_params() {
    let video = TrackDescription::<Loopback, &[u8], ()>::new(
      ms_tb(),
      TrackParams::Video(VideoTrackParams::new(27, 1920, 1080, 0, None)),
      (),
    );
    assert_eq!(video.kind(), TrackKind::Video);
    assert_eq!(video.codec(), 27);
    assert_eq!(video.timebase(), ms_tb());

    let audio = TrackDescription::<Loopback, &[u8], ()>::new(
      ms_tb(),
      TrackParams::Audio(AudioTrackParams::new(86018, 48_000, 2, 8, 3)),
      (),
    );
    assert_eq!(audio.kind(), TrackKind::Audio);
    assert_eq!(audio.codec(), 86018);
  }

  /// Extradata is absent until it is given, and what goes in comes back
  /// out of `into_parts` unchanged, with the side data beside it.
  #[test]
  fn extradata_is_absent_until_given_and_every_part_comes_back() {
    const AVCC: &[u8] = &[0x01, 0x64, 0x00, 0x28];
    let description = TrackDescription::<Loopback, &[u8], [u8; 2]>::new(
      ms_tb(),
      TrackParams::Video(VideoTrackParams::new(27, 1280, 720, 0, None)),
      [7, 9],
    );
    assert_eq!(description.extradata(), None);

    let mut description = description.with_extradata(Some(AVCC));
    assert_eq!(description.extradata(), Some(&AVCC));
    description.side_data_mut()[1] = 8;
    assert_eq!(description.side_data(), &[7, 8]);

    let (timebase, params, extradata, side_data) = description.into_parts();
    assert_eq!(timebase, ms_tb());
    assert_eq!(params.kind(), TrackKind::Video);
    assert_eq!(extradata, Some(AVCC));
    assert_eq!(side_data, [7, 8]);
  }
}
