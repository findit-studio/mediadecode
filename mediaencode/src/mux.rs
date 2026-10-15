//! The muxer face — compressed packets in, one container out.
//!
//! A [`Muxer`] is the write side's mirror of the `Demuxer` face in
//! `mediadecode`: an opened container session that takes packets, one
//! write per kind, and a [`finish`](Muxer::finish) that completes the
//! file. It takes the very envelopes a demuxer delivers — a
//! [`VideoTrackPacket`] carries its track coordinate with it — and
//! [`write_packet`](Muxer::write_packet) routes a [`DemuxedPacket`] to
//! the write for its kind. A refused packet comes back inside the
//! error, untouched.
//!
//! # A remux: collect, then open, then write
//!
//! A container writes its attachments with its header, so a muxer is
//! given them when it is opened, in its track table, and never as a
//! packet. A demuxer hands out every attachment before its first timed
//! packet, so a remux collects them first — each into its track's
//! description — opens the muxer with the whole table, and then writes
//! the timed packets:
//!
//! ```
//! use core::ops::Deref;
//! use mediaencode::{MuxError, Muxer};
//! use mediaencode::mediaspine::{
//!   demuxer::{DemuxAdapter, DemuxedPacket, TrackInfo},
//!   track::{TrackDescription, TrackMismatch},
//! };
//!
//! /// The table a muxer is opened with: one description per row, each
//! /// attachment track carrying the payload the demuxer delivered for it
//! /// before its first timed packet — which comes back too, for the
//! /// write loop.
//! ///
//! /// `rows` is what a demuxer's `tracks()` answers: handles that
//! /// dereference to the rows its session keeps. Each row is described
//! /// through a borrow, so nothing is taken from the session.
//! fn table<A: DemuxAdapter, B, H: Deref<Target = TrackInfo<A>>>(
//!   rows: &[H],
//!   packets: &mut impl Iterator<Item = DemuxedPacket<A, B>>,
//! ) -> Result<
//!   (Vec<TrackDescription<A, B, ()>>, Option<DemuxedPacket<A, B>>),
//!   TrackMismatch<A, B, ()>,
//! > {
//!   let mut files: Vec<Option<_>> = rows.iter().map(|_| None).collect();
//!   let mut first = None;
//!   for packet in packets {
//!     match packet {
//!       DemuxedPacket::Attachment(p) => {
//!         let (track, payload) = p.into_parts();
//!         if let Some(file) = files.get_mut(track.get()) {
//!           *file = Some(payload);
//!         }
//!       }
//!       timed => {
//!         first = Some(timed);
//!         break;
//!       }
//!     }
//!   }
//!   let mut table = Vec::with_capacity(rows.len());
//!   for (row, file) in rows.iter().zip(files) {
//!     // `&**row`: the handle, dereferenced to the row it shares.
//!     table.push(TrackDescription::from_track_info(&**row, file, ())?);
//!   }
//!   Ok((table, first))
//! }
//!
//! /// Writes the timed packets once the muxer is open. An attachment here
//! /// would break the demuxer's promise: `write_packet` refuses it by
//! /// name and hands it back, as it does every refused packet.
//! fn write_all<M: Muxer>(
//!   muxer: &mut M,
//!   packets: impl IntoIterator<Item = DemuxedPacket<M::Adapter, M::Buffer>>,
//! ) -> Result<(), MuxError<M::Error, DemuxedPacket<M::Adapter, M::Buffer>>> {
//!   for packet in packets {
//!     muxer.write_packet(packet)?;
//!   }
//!   Ok(())
//! }
//! ```
//!
//! # Construction is not on the trait
//!
//! As with the `Demuxer`, opening is each backend's own business: a
//! backend opens a muxer from one
//! [`TrackDescription`](mediaspine::track::TrackDescription) per output
//! track — the track at [`TrackIndex::new(i)`](TrackIndex::new) is the
//! `i`-th description, an attachment track's carrying its one file —
//! and from wherever the bytes go.
//!
//! # Finishing ends the session
//!
//! [`finish`](Muxer::finish) takes the muxer by value, so nothing can be
//! written after the container is complete — that is the compiler's
//! rule, not a runtime check:
//!
//! ```compile_fail,E0382
//! # use mediaencode::Muxer;
//! # use mediaencode::mediaspine::demuxer::VideoTrackPacket;
//! fn write_after_finish<M: Muxer>(
//!   mut muxer: M,
//!   first: VideoTrackPacket<M::Adapter, M::Buffer>,
//!   second: VideoTrackPacket<M::Adapter, M::Buffer>,
//! ) {
//!   let _ = muxer.write_video(first);
//!   let _ = muxer.finish();
//!   let _ = muxer.write_video(second);
//! }
//! ```

use mediaspine::{
  Timestamp,
  demuxer::{
    AudioTrackPacket, DataTrackPacket, DemuxAdapter, DemuxedPacket, SubtitleTrackPacket,
    TrackIndex, TrackKind, VideoTrackPacket,
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
  /// An attachment was offered as a packet. A container writes its
  /// attachments with its header, so a muxer is given each one when it
  /// is opened, in its track table, and never takes one as a packet.
  #[error(
    "the attachment on track {} is not written as a packet; \
     a muxer is given its attachments in the track table it is opened with",
    .track.get()
  )]
  Attachment {
    /// The track the packet named.
    track: TrackIndex,
  },
}

/// How a muxer write failed: a refusal every muxer names the same way,
/// with the packet handed back, or the backend's own fault.
///
/// `P` is the packet the write took — each write method names its own —
/// so a refused packet can be retried, rerouted or dropped whatever its
/// buffer and extras are: it is moved back, never cloned.
#[derive(Debug, thiserror::Error)]
pub enum MuxError<E, P> {
  /// A refusal every muxer names the same way. Nothing was written.
  #[error("{refusal}")]
  Refused {
    /// What was refused, and why.
    refusal: MuxRefusal,
    /// The packet offered, handed back untouched.
    packet: P,
  },
  /// The backend's own fault. The packet is not handed back: a fault can
  /// come after the backend has taken it, so it may have been consumed.
  #[error(transparent)]
  Fault(E),
}

impl<E, P> MuxError<E, P> {
  /// Maps the handed-back packet, leaving the refusal — or the fault —
  /// as it is. A caller that routes several kinds of packet through one
  /// error type gives each refused packet back its envelope this way;
  /// see the module's remux example.
  pub fn map_packet<Q>(self, f: impl FnOnce(P) -> Q) -> MuxError<E, Q> {
    match self {
      Self::Refused { refusal, packet } => MuxError::Refused {
        refusal,
        packet: f(packet),
      },
      Self::Fault(fault) => MuxError::Fault(fault),
    }
  }
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
/// # Attachments
///
/// A container writes its attachments — fonts, cover art, any file it
/// carries whole — with its header, so a muxer is given them when it is
/// opened, in its track table: an attachment track's
/// [`TrackDescription`](mediaspine::track::TrackDescription) carries its
/// one file. No method writes one as a packet, and
/// [`write_packet`](Self::write_packet) refuses a
/// [`DemuxedPacket::Attachment`] by name ([`MuxRefusal::Attachment`]).
///
/// A demuxer hands its attachments out first — the `Demuxer` face in
/// `mediadecode` promises each attachment track's one packet before any
/// timed packet — so a remux collects them all before it opens the
/// muxer. `mediadecode-ffmpeg`'s demuxer keeps that promise by
/// construction: it queues every attachment at open, and each pull
/// drains the queue before it reads the container, after a seek as
/// well.
///
/// # The refusals
///
/// A packet for a track the muxer does not have, of the wrong kind for
/// its track, without the timestamp its track is ordered by, or an
/// attachment offered as a packet is refused by name — [`MuxRefusal`],
/// inside [`MuxError::Refused`] — and nothing is written. The refusal
/// hands the packet back untouched, so the caller still owns it.
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
  ) -> Result<(), MuxError<Self::Error, VideoTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one audio packet to the track it names.
  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, AudioTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one subtitle packet to the track it names.
  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, SubtitleTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one timed-data packet to the track it names.
  fn write_data(
    &mut self,
    packet: DataTrackPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, DataTrackPacket<Self::Adapter, Self::Buffer>>>;

  /// Writes one demuxed packet through the write for its kind — the
  /// router a remux's write loop calls.
  ///
  /// A refused packet comes back in the envelope it arrived in, and a
  /// [`DemuxedPacket::Attachment`] is refused by name
  /// ([`MuxRefusal::Attachment`]) and handed back: a muxer is given its
  /// attachments in its track table, when it is opened.
  fn write_packet(
    &mut self,
    packet: DemuxedPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), MuxError<Self::Error, DemuxedPacket<Self::Adapter, Self::Buffer>>> {
    match packet {
      DemuxedPacket::Video(p) => self
        .write_video(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Video)),
      DemuxedPacket::Audio(p) => self
        .write_audio(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Audio)),
      DemuxedPacket::Subtitle(p) => self
        .write_subtitle(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Subtitle)),
      DemuxedPacket::Data(p) => self
        .write_data(p)
        .map_err(|e| e.map_packet(DemuxedPacket::Data)),
      DemuxedPacket::Attachment(p) => Err(MuxError::Refused {
        refusal: MuxRefusal::Attachment { track: p.track() },
        packet: DemuxedPacket::Attachment(p),
      }),
    }
  }

  /// Completes the container — the packets still held for
  /// interleaving, then whatever the format writes last — and ends the
  /// session. It takes no packet and refuses nothing; an error is the
  /// backend's own fault.
  fn finish(self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use mediaspine::{
    Timebase,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    demuxer::{AttachmentPacket, AttachmentTrackPacket, DataPacket, DemuxedPacket},
    packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
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

  /// A four-track muxer (video, audio, then two attachments, whose files
  /// it was opened with) that checks what every muxer checks and writes
  /// nothing anywhere.
  struct Table {
    kinds: [TrackKind; 4],
    last: [Option<Timestamp>; 4],
    written: u32,
  }

  impl Table {
    fn new() -> Self {
      Self {
        kinds: [
          TrackKind::Video,
          TrackKind::Audio,
          TrackKind::Attachment,
          TrackKind::Attachment,
        ],
        last: [None; 4],
        written: 0,
      }
    }

    /// That `track` is in the table and carries `offered` packets.
    fn kind_of(&self, track: TrackIndex, offered: TrackKind) -> Result<(), MuxRefusal> {
      let expected = *self
        .kinds
        .get(track.get())
        .ok_or(MuxRefusal::UnknownTrack { track })?;
      if expected != offered {
        return Err(MuxRefusal::KindMismatch {
          track,
          expected,
          offered,
        });
      }
      Ok(())
    }

    /// What every timed write checks, from the packet's track, kind and
    /// decode timestamp alone — the packet itself is not touched —
    /// answering the timestamp to record.
    fn check(
      &self,
      track: TrackIndex,
      offered: TrackKind,
      dts: Option<Timestamp>,
    ) -> Result<Timestamp, MuxRefusal> {
      self.kind_of(track, offered)?;
      let dts = dts.ok_or(MuxRefusal::MissingDts { track })?;
      if let Some(last) = self.last[track.get()]
        && dts.pts() <= last.pts()
      {
        return Err(MuxRefusal::NonMonotonicDts {
          track,
          last,
          offered: dts,
        });
      }
      Ok(dts)
    }

    /// Writes `packet`, or hands it back with the refusal.
    fn admit<P>(
      &mut self,
      track: TrackIndex,
      offered: TrackKind,
      dts: Option<Timestamp>,
      packet: P,
    ) -> Result<(), MuxError<Fault, P>> {
      match self.check(track, offered, dts) {
        Ok(dts) => {
          self.last[track.get()] = Some(dts);
          self.written += 1;
          Ok(())
        }
        Err(refusal) => Err(MuxError::Refused { refusal, packet }),
      }
    }
  }

  type VTrack = VideoTrackPacket<Loopback, Bytes>;
  type ATrack = AudioTrackPacket<Loopback, Bytes>;
  type STrack = SubtitleTrackPacket<Loopback, Bytes>;
  type DTrack = DataTrackPacket<Loopback, Bytes>;
  type AtTrack = AttachmentTrackPacket<Loopback, Bytes>;
  type Demuxed = DemuxedPacket<Loopback, Bytes>;

  impl Muxer for Table {
    type Adapter = Loopback;
    type Buffer = Bytes;
    type Error = Fault;

    fn write_video(&mut self, p: VTrack) -> Result<(), MuxError<Fault, VTrack>> {
      let (track, dts) = (p.track(), p.packet().dts());
      self.admit(track, TrackKind::Video, dts, p)
    }

    fn write_audio(&mut self, p: ATrack) -> Result<(), MuxError<Fault, ATrack>> {
      let (track, dts) = (p.track(), p.packet().dts());
      self.admit(track, TrackKind::Audio, dts, p)
    }

    fn write_subtitle(&mut self, p: STrack) -> Result<(), MuxError<Fault, STrack>> {
      let (track, pts) = (p.track(), p.packet().pts());
      self.admit(track, TrackKind::Subtitle, pts, p)
    }

    fn write_data(&mut self, p: DTrack) -> Result<(), MuxError<Fault, DTrack>> {
      let (track, pts) = (p.track(), p.packet().pts());
      self.admit(track, TrackKind::Data, pts, p)
    }

    fn finish(self) -> Result<(), Fault> {
      Ok(())
    }
  }

  /// Every packet the laws offer carries these bytes, so a handed-back
  /// packet can be checked to hold the very slice it was offered with.
  const PAYLOAD: &[u8] = b"payload";

  fn video(track: usize, dts: Option<i64>) -> VTrack {
    VideoTrackPacket::new(
      TrackIndex::new(track),
      VideoPacket::new(PAYLOAD, ())
        .with_dts(dts.map(ms))
        .with_pts(dts.map(ms))
        .with_flags(PacketFlags::KEY),
    )
  }

  fn audio(track: usize, dts: i64) -> ATrack {
    AudioTrackPacket::new(
      TrackIndex::new(track),
      AudioPacket::new(PAYLOAD, ()).with_dts(Some(ms(dts))),
    )
  }

  fn subtitle(track: usize, pts: i64) -> STrack {
    SubtitleTrackPacket::new(
      TrackIndex::new(track),
      SubtitlePacket::new(PAYLOAD, ()).with_pts(Some(ms(pts))),
    )
  }

  fn data(track: usize, pts: i64) -> DTrack {
    DataTrackPacket::new(
      TrackIndex::new(track),
      DataPacket::new(PAYLOAD, ()).with_pts(Some(ms(pts))),
    )
  }

  fn attachment(track: usize) -> AtTrack {
    AttachmentTrackPacket::new(
      TrackIndex::new(track),
      AttachmentPacket::new(PAYLOAD, ()).with_flags(PacketFlags::KEY),
    )
  }

  /// Asserts that `back` is the packet `kept` was cloned from before it
  /// was offered: the same track, the very bytes it was offered with,
  /// and the same timestamps and flags.
  macro_rules! assert_handed_back {
    ($back:expr, $kept:expr; $($field:ident),*) => {{
      let (back, kept) = (&$back, &$kept);
      assert_eq!(back.track(), kept.track(), "the same track");
      let (b, k) = (back.packet(), kept.packet());
      assert!(
        core::ptr::eq(*b.data(), *k.data()),
        "the very bytes it was offered with"
      );
      $(assert_eq!(b.$field(), k.$field(), stringify!($field));)*
    }};
  }

  /// The refusal and the handed-back packet, or a panic naming what came
  /// instead.
  fn refused<P>(result: Result<(), MuxError<Fault, P>>) -> (MuxRefusal, P) {
    match result {
      Err(MuxError::Refused { refusal, packet }) => (refusal, packet),
      Err(MuxError::Fault(fault)) => panic!("expected a refusal, got the fault {fault:?}"),
      Ok(()) => panic!("expected a refusal, the packet was written"),
    }
  }

  #[test]
  fn a_packet_for_a_track_the_muxer_lacks_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = video(5, Some(0));
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 5));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    assert_eq!(muxer.written, 0, "nothing is written for a refused packet");
  }

  #[test]
  fn a_packet_of_the_wrong_kind_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = audio(0, 0);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_audio(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        expected: TrackKind::Video,
        offered: TrackKind::Audio,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
  }

  #[test]
  fn a_packet_without_a_decode_timestamp_is_refused_by_name() {
    let mut muxer = Table::new();
    let offered = video(0, None);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    assert!(matches!(refusal, MuxRefusal::MissingDts { .. }));
    assert_handed_back!(back, kept; pts, dts, duration, flags);
  }

  /// **Within a track, decode timestamps increase; across tracks the
  /// packets interleave freely.** An audio packet stamped before the
  /// last video packet is written; a video packet that does not advance
  /// its own track is refused, naming both timestamps, and handed back.
  #[test]
  fn a_decode_timestamp_that_does_not_advance_its_track_is_refused_by_name() {
    let mut muxer = Table::new();
    muxer
      .write_video(video(0, Some(40)))
      .expect("first video packet");
    muxer
      .write_audio(audio(1, 0))
      .expect("another track, interleaved");
    let offered = video(0, Some(40));
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_video(offered));
    match refusal {
      MuxRefusal::NonMonotonicDts {
        track,
        last,
        offered,
      } => {
        assert_eq!(track.get(), 0);
        assert_eq!((last.pts(), offered.pts()), (40, 40));
      }
      other => panic!("expected the monotonicity refusal, got {other:?}"),
    }
    assert_handed_back!(back, kept; pts, dts, duration, flags);
    // The handed-back packet is the caller's again: restamped, it goes.
    let (track, packet) = back.into_parts();
    muxer
      .write_video(VideoTrackPacket::new(track, packet.with_dts(Some(ms(80)))))
      .expect("the same packet, advancing");
    assert_eq!(muxer.written, 3);
    muxer.finish().expect("the session ends cleanly");
  }

  /// **Every write hands a refused packet back untouched** — the
  /// subtitle and data writes as well as the two above, here refused for
  /// the kind of the track they name.
  #[test]
  fn every_write_hands_a_refused_packet_back_untouched() {
    let mut muxer = Table::new();

    let offered = subtitle(0, 10);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_subtitle(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        offered: TrackKind::Subtitle,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, duration, flags);

    let offered = data(1, 10);
    let kept = offered.clone();
    let (refusal, back) = refused(muxer.write_data(offered));
    assert!(matches!(
      refusal,
      MuxRefusal::KindMismatch {
        offered: TrackKind::Data,
        ..
      }
    ));
    assert_handed_back!(back, kept; pts, duration, flags);
    assert_eq!(muxer.written, 0);
  }

  /// **A remux gets a refused packet back in the envelope it came in.**
  /// [`MuxError::map_packet`] carries the refusal over unchanged and
  /// wraps the packet, as the module's remux example does.
  #[test]
  fn map_packet_puts_a_refused_packet_back_in_its_envelope() {
    let mut muxer = Table::new();
    let offered = video(7, Some(0));
    let kept = offered.clone();
    let routed: Result<(), MuxError<Fault, DemuxedPacket<Loopback, Bytes>>> = muxer
      .write_video(offered)
      .map_err(|e| e.map_packet(DemuxedPacket::Video));
    let (refusal, back) = refused(routed);
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 7));
    match back {
      DemuxedPacket::Video(back) => {
        assert_handed_back!(back, kept; pts, dts, duration, flags);
      }
      other => panic!("expected the video envelope, got {other:?}"),
    }
  }

  /// **`write_packet` routes each kind to its write, and refuses an
  /// attachment by name.** A muxer is given its attachments in its track
  /// table; one offered as a packet comes back untouched, in its
  /// envelope, as does a timed packet a write refused.
  #[test]
  fn write_packet_routes_each_kind_and_refuses_an_attachment_by_name() {
    let mut muxer = Table::new();
    muxer
      .write_packet(DemuxedPacket::Video(video(0, Some(0))))
      .expect("a video packet goes to the video write");
    muxer
      .write_packet(DemuxedPacket::Audio(audio(1, 0)))
      .expect("an audio packet goes to the audio write");
    assert_eq!(muxer.written, 2);

    let offered = attachment(2);
    let kept = offered.clone();
    let (refusal, back) =
      refused::<Demuxed>(muxer.write_packet(DemuxedPacket::Attachment(offered)));
    assert!(matches!(refusal, MuxRefusal::Attachment { track } if track.get() == 2));
    match back {
      DemuxedPacket::Attachment(back) => assert_handed_back!(back, kept; flags),
      other => panic!("expected the attachment envelope, got {other:?}"),
    }

    let offered = video(9, Some(10));
    let kept = offered.clone();
    let (refusal, back) = refused::<Demuxed>(muxer.write_packet(DemuxedPacket::Video(offered)));
    assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 9));
    match back {
      DemuxedPacket::Video(back) => assert_handed_back!(back, kept; pts, dts, duration, flags),
      other => panic!("expected the video envelope, got {other:?}"),
    }
    assert_eq!(muxer.written, 2, "nothing refused was written");
  }
}
