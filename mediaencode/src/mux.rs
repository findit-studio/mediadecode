//! The muxer face — compressed packets in, one container out.
//!
//! A [`Muxer`] is the write side's mirror of the `Demuxer` face in
//! `mediadecode`: an opened container session that takes packets, one
//! write per kind, and a [`finish`](Muxer::finish) that completes the
//! file. It takes the very envelopes a demuxer delivers — a
//! [`VideoTrackPacket`] carries its track coordinate with it — so a
//! remux is a match:
//!
//! ```
//! use mediaencode::{MuxError, Muxer};
//! use mediaencode::mediaspine::demuxer::DemuxedPacket;
//!
//! /// Copies one demuxed packet into the output. Attachments travel with
//! /// the track table the muxer was opened with, not as packets.
//! fn remux<M: Muxer>(
//!   muxer: &mut M,
//!   packet: DemuxedPacket<M::Adapter, M::Buffer>,
//! ) -> Result<(), MuxError<M::Error>> {
//!   match packet {
//!     DemuxedPacket::Video(p) => muxer.write_video(p),
//!     DemuxedPacket::Audio(p) => muxer.write_audio(p),
//!     DemuxedPacket::Subtitle(p) => muxer.write_subtitle(p),
//!     DemuxedPacket::Data(p) => muxer.write_data(p),
//!     DemuxedPacket::Attachment(_) => Ok(()),
//!   }
//! }
//! ```
//!
//! # Construction is not on the trait
//!
//! As with the `Demuxer`, opening is each backend's own business: a
//! backend opens a muxer from one
//! [`TrackDescription`](mediaspine::track::TrackDescription) per output
//! track — the track at [`TrackIndex::new(i)`](TrackIndex::new) is the
//! `i`-th description — and from wherever the bytes go.
//!
//! # Finishing ends the session
//!
//! [`finish`](Muxer::finish) takes the muxer by value, so nothing can be
//! written after the container is complete — that is the compiler's
//! rule, not a runtime check:
//!
//! ```compile_fail,E0382
//! # use mediaencode::{MuxError, Muxer};
//! # use mediaencode::mediaspine::demuxer::VideoTrackPacket;
//! fn write_after_finish<M: Muxer>(
//!   mut muxer: M,
//!   first: VideoTrackPacket<M::Adapter, M::Buffer>,
//!   second: VideoTrackPacket<M::Adapter, M::Buffer>,
//! ) -> Result<(), MuxError<M::Error>> {
//!   muxer.write_video(first)?;
//!   muxer.finish()?;
//!   muxer.write_video(second)
//! }
//! ```

use mediaspine::{
  Timestamp,
  demuxer::{
    AudioTrackPacket, DataTrackPacket, DemuxAdapter, SubtitleTrackPacket, TrackIndex, TrackKind,
    VideoTrackPacket,
  },
};

/// A refusal every muxer names the same way.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MuxRefusal {
  /// The packet names a track the muxer was not opened with.
  #[error("track {} is not in the muxer's table", .track.get())]
  UnknownTrack {
    /// The track the packet named.
    track: TrackIndex,
  },
  /// The packet's kind is not the track's.
  #[error("track {} carries {expected:?} packets, not {offered:?}", .track.get())]
  KindMismatch {
    /// The track the packet named.
    track: TrackIndex,
    /// The track's kind.
    expected: TrackKind,
    /// The packet's kind.
    offered: TrackKind,
  },
  /// The packet carries no decode timestamp — for a subtitle or data
  /// packet, whose decode time is its presentation time, no
  /// presentation timestamp — and a muxer never invents one.
  #[error("a packet on track {} carries no decode timestamp", .track.get())]
  MissingDts {
    /// The track the packet named.
    track: TrackIndex,
  },
  /// The packet's decode timestamp does not advance past the last one
  /// written on its track. Within a track, decode order is the order
  /// packets are written in, and the container records it as such.
  #[error(
    "decode timestamps on track {} must increase: {} after {}",
    .track.get(), .offered.pts(), .last.pts()
  )]
  NonMonotonicDts {
    /// The track the packet named.
    track: TrackIndex,
    /// The last decode timestamp written on the track.
    last: Timestamp,
    /// The decode timestamp the packet carries.
    offered: Timestamp,
  },
}

/// How a muxer call failed: a refusal every muxer names the same way,
/// or the backend's own fault.
#[derive(Debug, thiserror::Error)]
pub enum MuxError<E> {
  /// A refusal every muxer names the same way.
  #[error(transparent)]
  Refused(#[from] MuxRefusal),
  /// The backend's own fault.
  #[error(transparent)]
  Fault(E),
}

/// An opened container session that writes packets.
///
/// # Interleaving
///
/// Packets may arrive with their tracks interleaved however the caller
/// produces them; putting them in the order the container stores them
/// is the muxer's job. Within one track, decode timestamps must
/// increase ([`MuxRefusal::NonMonotonicDts`]).
///
/// # The refusals
///
/// A packet for a track the muxer does not have, of the wrong kind for
/// its track, or without the timestamp its track is ordered by is
/// refused by name — [`MuxRefusal`], inside [`MuxError`] — and nothing
/// is written.
pub trait Muxer {
  /// Backend-specific vocabulary — the container tier's, as the
  /// `Demuxer`'s is.
  type Adapter: DemuxAdapter;
  /// Buffer type held by the packets this session takes.
  type Buffer: AsRef<[u8]>;
  /// Muxer-specific fault type.
  type Error;

  /// Writes one video packet to the track it names.
  fn write_video(
    &mut self,
    packet: VideoTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error>>;

  /// Writes one audio packet to the track it names.
  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error>>;

  /// Writes one subtitle packet to the track it names.
  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error>>;

  /// Writes one timed-data packet to the track it names.
  fn write_data(
    &mut self,
    packet: DataTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error>>;

  /// Completes the container — the packets still held for
  /// interleaving, then whatever the format writes last — and ends the
  /// session.
  fn finish(self) -> Result<(), MuxError<Self::Error>>;
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use mediaspine::{
    Timebase,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    packet::{AudioPacket, VideoPacket},
  };

  use super::*;

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

  type Bytes = &'static [u8];

  #[derive(Debug, PartialEq, Eq)]
  struct Fault;

  fn ms(pts: i64) -> Timestamp {
    Timestamp::new(
      pts,
      Timebase::new(1, NonZeroI32::new(1000).expect("non-zero")),
    )
  }

  /// A two-track muxer (video, then audio) that checks what every muxer
  /// checks and writes nothing anywhere.
  struct Table {
    kinds: [TrackKind; 2],
    last: [Option<Timestamp>; 2],
    written: u32,
  }

  impl Table {
    fn new() -> Self {
      Self {
        kinds: [TrackKind::Video, TrackKind::Audio],
        last: [None, None],
        written: 0,
      }
    }

    fn admit(
      &mut self,
      track: TrackIndex,
      offered: TrackKind,
      dts: Option<Timestamp>,
    ) -> Result<(), MuxError<Fault>> {
      let slot = track.get();
      let expected = *self
        .kinds
        .get(slot)
        .ok_or(MuxRefusal::UnknownTrack { track })?;
      if expected != offered {
        return Err(
          MuxRefusal::KindMismatch {
            track,
            expected,
            offered,
          }
          .into(),
        );
      }
      let dts = dts.ok_or(MuxRefusal::MissingDts { track })?;
      if let Some(last) = self.last[slot]
        && dts.pts() <= last.pts()
      {
        return Err(
          MuxRefusal::NonMonotonicDts {
            track,
            last,
            offered: dts,
          }
          .into(),
        );
      }
      self.last[slot] = Some(dts);
      self.written += 1;
      Ok(())
    }
  }

  impl Muxer for Table {
    type Adapter = Loopback;
    type Buffer = Bytes;
    type Error = Fault;

    fn write_video(&mut self, p: VideoTrackPacket<Loopback, Bytes>) -> Result<(), MuxError<Fault>> {
      let (track, packet) = p.into_parts();
      self.admit(track, TrackKind::Video, packet.dts())
    }

    fn write_audio(&mut self, p: AudioTrackPacket<Loopback, Bytes>) -> Result<(), MuxError<Fault>> {
      let (track, packet) = p.into_parts();
      self.admit(track, TrackKind::Audio, packet.dts())
    }

    fn write_subtitle(
      &mut self,
      p: SubtitleTrackPacket<Loopback, Bytes>,
    ) -> Result<(), MuxError<Fault>> {
      let (track, packet) = p.into_parts();
      self.admit(track, TrackKind::Subtitle, packet.pts())
    }

    fn write_data(&mut self, p: DataTrackPacket<Loopback, Bytes>) -> Result<(), MuxError<Fault>> {
      let (track, packet) = p.into_parts();
      self.admit(track, TrackKind::Data, packet.pts())
    }

    fn finish(self) -> Result<(), MuxError<Fault>> {
      Ok(())
    }
  }

  fn video(track: usize, dts: Option<i64>) -> VideoTrackPacket<Loopback, Bytes> {
    VideoTrackPacket::new(
      TrackIndex::new(track),
      VideoPacket::new(&[][..], ()).with_dts(dts.map(ms)),
    )
  }

  fn audio(track: usize, dts: i64) -> AudioTrackPacket<Loopback, Bytes> {
    AudioTrackPacket::new(
      TrackIndex::new(track),
      AudioPacket::new(&[][..], ()).with_dts(Some(ms(dts))),
    )
  }

  #[test]
  fn a_packet_for_a_track_the_muxer_lacks_is_refused_by_name() {
    let mut muxer = Table::new();
    assert!(matches!(
      muxer.write_video(video(5, Some(0))),
      Err(MuxError::Refused(MuxRefusal::UnknownTrack { track })) if track.get() == 5
    ));
    assert_eq!(muxer.written, 0, "nothing is written for a refused packet");
  }

  #[test]
  fn a_packet_of_the_wrong_kind_is_refused_by_name() {
    let mut muxer = Table::new();
    assert!(matches!(
      muxer.write_audio(audio(0, 0)),
      Err(MuxError::Refused(MuxRefusal::KindMismatch {
        expected: TrackKind::Video,
        offered: TrackKind::Audio,
        ..
      }))
    ));
  }

  #[test]
  fn a_packet_without_a_decode_timestamp_is_refused_by_name() {
    let mut muxer = Table::new();
    assert!(matches!(
      muxer.write_video(video(0, None)),
      Err(MuxError::Refused(MuxRefusal::MissingDts { .. }))
    ));
  }

  /// **Within a track, decode timestamps increase; across tracks the
  /// packets interleave freely.** An audio packet stamped before the
  /// last video packet is written; a video packet that does not advance
  /// its own track is refused, naming both timestamps.
  #[test]
  fn a_decode_timestamp_that_does_not_advance_its_track_is_refused_by_name() {
    let mut muxer = Table::new();
    muxer
      .write_video(video(0, Some(40)))
      .expect("first video packet");
    muxer
      .write_audio(audio(1, 0))
      .expect("another track, interleaved");
    match muxer.write_video(video(0, Some(40))) {
      Err(MuxError::Refused(MuxRefusal::NonMonotonicDts {
        track,
        last,
        offered,
      })) => {
        assert_eq!(track.get(), 0);
        assert_eq!((last.pts(), offered.pts()), (40, 40));
      }
      other => panic!("expected the monotonicity refusal, got {other:?}"),
    }
    muxer
      .write_video(video(0, Some(80)))
      .expect("an advancing packet");
    assert_eq!(muxer.written, 3);
    muxer.finish().expect("the session ends cleanly");
  }
}
