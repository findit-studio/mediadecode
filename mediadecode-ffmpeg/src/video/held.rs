//! **The parameter sets a decoder holds**, as FFmpeg 9's H.264 and HEVC
//! decoders hold them, and **the record a decoder opened fresh is opened
//! on** so that it holds them too (libavcodec 63.1.101, FFmpeg 9.0.1).
//!
//! A decoder of either codec keeps every parameter set it stores, by id, for
//! as long as it lives: the sets of the extradata it was opened on
//! (`h264_decode_init`, h264dec.c:399-412; `hevc_decode_init`,
//! hevc/hevcdec.c:4167-4172), those of a packet's
//! `AV_PKT_DATA_NEW_EXTRADATA` (h264dec.c:1038-1044;
//! hevc/hevcdec.c:3855-3860), and those a packet carries in band, whatever
//! its key flag (`decode_nal_units`, h264dec.c:699-728; `decode_nal_unit`,
//! hevc/hevcdec.c:3609-3625). A set replaces the one held under its id. A
//! record replaces exactly the ids it carries and no other: FFmpeg clears
//! nothing before it applies one (`ff_h264_decode_extradata`,
//! h264_parse.c:466-524; `ff_hevc_decode_extradata`, hevc/parse.c:79-145).
//! An HEVC set that replaces another drops the sets that refer to it
//! (`remove_vps`, `remove_sps`, hevc/ps.c:89-111); an H.264 picture parameter
//! set keeps the sequence parameter set it was read under (`pps->sps`,
//! h264_ps.c:731-738), and a slice is decoded under that one
//! (h264_slice.c:1746-1747), whatever its id holds now. A flush clears none
//! of it.
//!
//! A decoder the session opens fresh — a switch to its threads, a reopen, a
//! fallback's — opens on a record. Opened on the codec parameters' record
//! alone it holds only that record's sets, and on a stream whose sets came
//! in band an IDR carrying none fails, with every picture to the next set.
//! So the session keeps what the decoder serving holds ([`Held`]), and a
//! decoder it opens fresh opens on a record carrying all of it, in the
//! framing the decoder serving reads packets in ([`Held::record`]); where no
//! record can carry it, or whether the decoder holds a set cannot be told,
//! the session refuses by name ([`crate::SetsUnrecordable`]).

use std::sync::Arc;

use super::params::{self, Codec, H264Sets, Unit, Walk};
use crate::{ParameterSet, Unrecordable};

/// What the decoder of a stream's codec holds.
#[derive(Clone, Debug, Default)]
pub(super) enum Held {
  /// A codec whose decoder holds no parameter set this crate reads.
  #[default]
  Other,
  /// FFmpeg's H.264 decoder.
  H264(Box<H264>),
  /// FFmpeg's HEVC decoder.
  Hevc(Box<Hevc>),
}

/// A record a decoder opened fresh is opened on ([`Held::record`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
  /// The record's bytes.
  pub(super) bytes: Vec<u8>,
  /// Whether FFmpeg's own decoder, opened on it strictly, witnesses what it
  /// carries before a decoder opens on it — a second witness of an HEVC
  /// record, whose sets this crate reads as FFmpeg reads them
  /// ([`params::hevc_vps`], [`params::hevc_sps`], [`params::hevc_pps`]): where
  /// no set it carries is one FFmpeg stores with a warning that the strict
  /// open turns into its refusal.
  pub(super) strict: bool,
}

/// The most bytes of a parameter set the table keeps: what an `avcC` or
/// `hvcC` entry, its length 16 bits, can carry. A packet may run to a
/// gigabyte; a set longer than this is held by a fingerprint of its bytes
/// alone, and no record carries it ([`Unrecordable::Oversized`]), so what the
/// table holds stays within what records could.
const MAX_HELD_UNIT: usize = u16::MAX as usize;

/// A parameter set's bytes as the table holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Bytes {
  /// The bytes themselves.
  Kept(Box<[u8]>),
  /// A set too long for a record's entry: its length and a 64-bit
  /// fingerprint (FNV-1a) of its bytes, which tell one such set from
  /// another.
  Fingerprint(usize, u64),
}

impl Bytes {
  fn of(bytes: &[u8]) -> Self {
    if bytes.len() <= MAX_HELD_UNIT {
      return Self::Kept(bytes.into());
    }
    let fingerprint = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
      (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    Self::Fingerprint(bytes.len(), fingerprint)
  }

  /// Whether these are `bytes`'s, as [`Self::of`] would hold them.
  fn is(&self, bytes: &[u8]) -> bool {
    match self {
      Self::Kept(kept) => **kept == *bytes,
      Self::Fingerprint(..) => *self == Self::of(bytes),
    }
  }

  /// The bytes, where they are kept: a record can carry them.
  fn kept(&self) -> Option<&[u8]> {
    match self {
      Self::Kept(bytes) => Some(bytes),
      Self::Fingerprint(..) => None,
    }
  }
}

impl Held {
  /// What a decoder of `codec_id` holds once opened on `extradata` — none
  /// where it is empty.
  pub(super) fn opened_on(codec_id: i32, extradata: &[u8]) -> Self {
    if codec_id == crate::CodecId::H264.raw() {
      let mut held = H264::default();
      let mut write = H264Write::new(&held);
      let _ = write.record(extradata);
      if let Some(next) = write.next {
        held = next;
      }
      Self::H264(Box::new(held))
    } else if codec_id == crate::CodecId::HEVC.raw() {
      let mut held = Hevc::default();
      let mut write = HevcWrite::new(&held);
      let _ = write.record(extradata);
      if let Some(next) = write.next {
        held = next;
      }
      Self::Hevc(Box::new(held))
    } else {
      Self::Other
    }
  }

  /// **What the decoder holds once it has read a packet** carrying `record`
  /// as `AV_PKT_DATA_NEW_EXTRADATA` and `data` as its body — the record
  /// first, as FFmpeg applies it before the packet's units (h264dec.c:
  /// 1038-1044; hevc/hevcdec.c:3855-3860); `None` where nothing changes.
  pub(super) fn after_packet(&self, record: Option<&[u8]>, data: Option<&[u8]>) -> Option<Self> {
    self.after_packet_reading_alpha(record, data).0
  }

  /// [`Self::after_packet`], and whether the packet — its record or its
  /// units — carries an HEVC video parameter set FFmpeg stores as alpha
  /// video, or may: read against the sets the decoder holds, as FFmpeg reads
  /// it, an identical set changing nothing and a set read past its end
  /// refused under an id held (hevc/ps.c:797-802, 944-949); an id held in
  /// doubt read as possibly holding nothing, so the reading errs toward
  /// alpha.
  pub(super) fn after_packet_reading_alpha(
    &self,
    record: Option<&[u8]>,
    data: Option<&[u8]>,
  ) -> (Option<Self>, bool) {
    match self {
      Self::Other => (None, false),
      Self::H264(held) => {
        let mut write = H264Write::new(held);
        if let Some(record) = record {
          // FFmpeg's H.264 decoder drops the record's verdict
          // (h264dec.c:1038-1044).
          let _ = write.record(record);
        }
        if let Some(data) = data {
          write.body(data);
        }
        (write.next.map(|next| Self::H264(Box::new(next))), false)
      }
      Self::Hevc(held) => {
        let mut write = HevcWrite::new(held);
        // A record FFmpeg's HEVC decoder fails to apply fails the packet
        // before its units are read (hevc/hevcdec.c:3855-3860).
        let applied = record.is_none_or(|record| write.record(record));
        if applied && let Some(data) = data {
          write.units(data);
        }
        let alpha = write.alpha;
        (write.next.map(|next| Self::Hevc(Box::new(next))), alpha)
      }
    }
  }

  /// Whether the HEVC decoder holds a video parameter set it reads as alpha
  /// video, or may — one held in doubt among them.
  pub(super) fn declares_alpha(&self) -> bool {
    match self {
      Self::Hevc(held) => held.vps.iter().flatten().any(|vps| vps.read.alpha),
      Self::H264(_) | Self::Other => false,
    }
  }

  /// This, with `record` applied as a packet's new extradata is: what a
  /// decoder opened for that packet must hold.
  pub(super) fn with_record(&self, record: &[u8]) -> Self {
    self
      .after_packet(Some(record), None)
      .unwrap_or_else(|| self.clone())
  }

  /// **Every set that differs from what `proven` holds is in doubt**: the
  /// decoder may have read what changed it, or not — a packet it refused
  /// with an error that does not say, a flush that dropped what it had not
  /// read. Where a set is gone, the one `proven` held comes back in doubt;
  /// what refers to a set in doubt is in doubt with it.
  pub(super) fn doubt_since(&mut self, proven: &Self) {
    match (self, proven) {
      (Self::H264(held), Self::H264(proven)) => held.doubt_since(proven),
      (Self::Hevc(held), Self::Hevc(proven)) => held.doubt_since(proven),
      _ => {}
    }
  }

  /// **The record a decoder opened fresh opens on to hold what this
  /// holds**: `Ok(None)` where `base` — the record it would open on, the
  /// codec parameters' or a packet's new one — gives it exactly that as it
  /// is; a record carrying every set held, in the framing the decoder
  /// serving reads packets in, otherwise, read back as FFmpeg reads it and
  /// found to give exactly that; the reason none can, where none can.
  ///
  /// `next`, the body of the packet the decoder opened reads first, if any:
  /// where it is H.264 and FFmpeg re-guesses its framing (h264dec.c:602-607)
  /// — under a NAL length size of four, which the record must then give —
  /// the framing either held before it does not matter.
  pub(super) fn record(
    &self,
    codec_id: i32,
    base: &[u8],
    next: Option<&[u8]>,
  ) -> Result<Option<Record>, Unrecordable> {
    match (self, Self::opened_on(codec_id, base)) {
      (Self::H264(held), Self::H264(fresh)) => held.record(&fresh, base, next),
      (Self::Hevc(held), Self::Hevc(fresh)) => held.record(&fresh, base),
      _ => Ok(None),
    }
  }

  /// **FFmpeg's H.264 decoder's verdict on `record`**, applied now — as a
  /// packet's new extradata, or as a body it reads as one
  /// ([`params::h264_record`]): read against the sequence parameter sets the
  /// decoder holds, which a picture parameter set the record carries may
  /// refer to — FFmpeg parses it against `ps->sps_list` (h264_ps.c:731-738) —
  /// a set held in doubt read as held by nothing.
  pub(super) fn h264_verdict(&self, record: &[u8]) -> Result<(), crate::ExtradataRejection> {
    let mut facts = [None; 32];
    if let Self::H264(held) = self {
      for (fact, sps) in facts.iter_mut().zip(held.sps.iter()) {
        *fact = sps
          .as_ref()
          .filter(|sps| !sps.doubt)
          .map(|sps| sps.set.facts);
      }
    }
    params::h264_extradata(record, &mut facts).verdict
  }

  /// Whether this holds a picture parameter set under `id`, for certain.
  #[cfg(test)]
  pub(super) fn holds_pps(&self, id: usize) -> bool {
    match self {
      Self::H264(held) => held.pps[id].as_ref().is_some_and(|pps| !pps.doubt),
      Self::Hevc(held) => held.pps[id].as_ref().is_some_and(|pps| !pps.doubt),
      Self::Other => false,
    }
  }

  /// **How FFmpeg's H.264 decoder reads the packet** `data` carrying
  /// `record` as `AV_PKT_DATA_NEW_EXTRADATA`, under the framing it holds once
  /// it applied the record: as an `avcC` record it applies, decoding no
  /// slice (h264dec.c:1045-1050), or as NAL units so framed; `None` where
  /// the framing cannot be told, or the stream is not H.264.
  pub(super) fn h264_reading(&self, record: Option<&[u8]>, data: &[u8]) -> Option<H264Reading> {
    let Self::H264(held) = self else {
      return None;
    };
    let (mut is_avc, mut size) = (held.is_avc, held.nal_length_size);
    if let Some(record) = record.filter(|record| !record.is_empty()) {
      let read = params::h264_extradata(record, &mut [None; 32]);
      is_avc = Some(read.is_avc);
      if let Some(read) = read.nal_length_size {
        size = Some(read);
      }
    }
    Some(match reading(is_avc, size, data)? {
      BodyReading::Record => H264Reading::Record,
      BodyReading::Units { is_avc: false, .. } => H264Reading::Units(None),
      BodyReading::Units { is_avc: true, size } => H264Reading::Units(Some(usize::from(size))),
    })
  }

  /// Whether this holds the same sets as `other`, in the same framing, each
  /// as certain.
  #[cfg(test)]
  pub(super) fn same(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::H264(a), Self::H264(b)) => a.same(b),
      (Self::Hevc(a), Self::Hevc(b)) => a.same(b),
      (Self::Other, Self::Other) => true,
      _ => false,
    }
  }
}

// ---------------------------------------------------------------------------
//  H.264
// ---------------------------------------------------------------------------

/// A sequence parameter set an H.264 decoder stored: the unit it read,
/// which a record carries; what FFmpeg compares to keep an identical set in
/// place; and what it says.
///
/// FFmpeg compares the whole parsed `SPS` (h264_ps.c:578-587): its fields
/// and `data`, the bytes its reader starts at through those its payload bits
/// reach ([`params::h264_identity`], h264_ps.c:296-305). A set's fields are
/// what its reading makes of those bytes — the first two readings never read
/// past them, or they fail the set — so two sets are alike in FFmpeg exactly
/// where their bytes so taken and their reading are: `unit`, the raw bytes,
/// is not compared. The same set before a four-byte start code rather than a
/// three-byte one carries one zero more in its raw bytes, which the splitter
/// drops from its payload (`get_bit_length`, h2645_parse.c:348-376), and is
/// the same set.
#[derive(Debug)]
struct Sequence {
  /// The unit as the splitter took it (`nal->raw_data`).
  unit: Bytes,
  /// `data` as FFmpeg keeps it ([`params::h264_identity`]).
  identity: Bytes,
  /// Which of FFmpeg's three readings stored it ([`params::H264Sets`]).
  reading: u8,
  /// What it says to a picture parameter set read after it.
  facts: params::Sps,
}

impl PartialEq for Sequence {
  fn eq(&self, other: &Self) -> bool {
    self.identity == other.identity && self.reading == other.reading && self.facts == other.facts
  }
}

impl Eq for Sequence {}

impl Sequence {
  /// Whether its reading ran past its payload: the third reading, which
  /// lets a truncated set stand, is the one that does.
  fn past_end(&self) -> bool {
    self.reading == 3
  }
}

/// A sequence parameter set held, and whether it is in doubt.
#[derive(Clone, Debug)]
struct SequenceHeld {
  set: Arc<Sequence>,
  doubt: bool,
}

/// A picture parameter set an H.264 decoder holds.
#[derive(Clone, Debug)]
struct PictureHeld {
  /// The unit as the splitter took it.
  unit: Arc<Bytes>,
  /// `data` as FFmpeg keeps it ([`params::h264_identity`],
  /// h264_ps.c:716-727): what tells it from another, whose raw bytes may
  /// differ by a trailing zero.
  identity: Arc<Bytes>,
  /// The id of the sequence parameter set it refers to.
  sps_id: u8,
  /// The sequence parameter set it was read under (`pps->sps`), which a
  /// slice referring to it is decoded under.
  bound: Arc<Sequence>,
  /// Whether its reading ran past its payload.
  past_end: bool,
  doubt: bool,
}

impl PictureHeld {
  fn same(&self, other: &Self) -> bool {
    self.identity == other.identity
      && self.sps_id == other.sps_id
      && *self.bound == *other.bound
      && self.past_end == other.past_end
      && self.doubt == other.doubt
  }
}

/// How FFmpeg's H.264 decoder reads a packet's body ([`Held::h264_reading`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum H264Reading {
  /// As an `avcC` record it applies as extradata, decoding no slice of it.
  Record,
  /// As NAL units, length-prefixed by so many bytes, or start-coded.
  Units(Option<usize>),
}

/// How an H.264 decoder frames the packets it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum H264Framing {
  /// Length-prefixed (`is_avc`), by NAL length fields of so many bytes.
  Avc(u8),
  /// Start-coded; `reguess` where its NAL length size is 4, which has
  /// FFmpeg re-guess the framing of every packet (h264dec.c:602-607).
  AnnexB { reguess: bool },
}

/// **What FFmpeg's H.264 decoder holds**: its sequence and picture parameter
/// sets (`ps->sps_list`, `ps->pps_list`) and its framing (`is_avc`,
/// `nal_length_size`), `None` where it cannot be told.
#[derive(Clone, Debug)]
pub(super) struct H264 {
  sps: [Option<SequenceHeld>; 32],
  pps: Box<[Option<PictureHeld>; 256]>,
  is_avc: Option<bool>,
  nal_length_size: Option<u8>,
}

impl Default for H264 {
  /// A decoder opened on no extradata: no set, start-coded, its NAL length
  /// size the option's default, 0 (h264dec.c:1093-1094).
  fn default() -> Self {
    Self {
      sps: Default::default(),
      pps: Box::new(core::array::from_fn(|_| None)),
      is_avc: Some(false),
      nal_length_size: Some(0),
    }
  }
}

impl H264 {
  /// How the decoder frames packets; `None` where that cannot be told.
  pub(super) fn framing(&self) -> Option<H264Framing> {
    match (self.is_avc?, self.nal_length_size?) {
      (true, size) => Some(H264Framing::Avc(size)),
      (false, size) => Some(H264Framing::AnnexB { reguess: size == 4 }),
    }
  }

  fn same(&self, other: &Self) -> bool {
    self.framing() == other.framing()
      && self.is_avc.is_some() == other.is_avc.is_some()
      && self
        .sps
        .iter()
        .zip(other.sps.iter())
        .all(|pair| match pair {
          (None, None) => true,
          (Some(a), Some(b)) => *a.set == *b.set && a.doubt == b.doubt,
          _ => false,
        })
      && self
        .pps
        .iter()
        .zip(other.pps.iter())
        .all(|pair| match pair {
          (None, None) => true,
          (Some(a), Some(b)) => a.same(b),
          _ => false,
        })
  }

  fn doubt_since(&mut self, proven: &Self) {
    for id in 0..self.sps.len() {
      let same = match (&self.sps[id], &proven.sps[id]) {
        (None, None) => true,
        (Some(a), Some(b)) => *a.set == *b.set && a.doubt == b.doubt,
        _ => false,
      };
      if !same {
        let entry = self.sps[id].clone().or_else(|| proven.sps[id].clone());
        self.sps[id] = entry.map(|entry| SequenceHeld {
          doubt: true,
          ..entry
        });
      }
    }
    for id in 0..self.pps.len() {
      let same = match (&self.pps[id], &proven.pps[id]) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same(b),
        _ => false,
      };
      let refers_to_doubt = self.pps[id].as_ref().is_some_and(|pps| {
        self.sps[usize::from(pps.sps_id)]
          .as_ref()
          .is_none_or(|sps| sps.doubt)
      });
      if !same || refers_to_doubt {
        let entry = self.pps[id].clone().or_else(|| proven.pps[id].clone());
        self.pps[id] = entry.map(|entry| PictureHeld {
          doubt: true,
          ..entry
        });
      }
    }
    if self.is_avc != proven.is_avc {
      self.is_avc = None;
    }
    if self.nal_length_size != proven.nal_length_size {
      self.nal_length_size = None;
    }
  }

  /// This, as it is once the packet `next` re-guessed its framing, where it
  /// does: FFmpeg's decoder takes start codes or `avcC` for that packet
  /// whatever it held, where its NAL length size is four and the packet is
  /// not read as a record (h264dec.c:602-607, 1045-1050) — `None` otherwise.
  fn decided_by(&self, next: Option<&[u8]>) -> Option<Self> {
    let data = next?;
    if self.nal_length_size != Some(4) || params::avcc_body(data) {
      return None;
    }
    let guess = params::h264_reguess(data)?;
    let mut decided = self.clone();
    decided.is_avc = Some(guess);
    Some(decided)
  }

  /// The record a decoder opened fresh opens on to hold this, against
  /// `fresh`, what one opened on `base` holds ([`Held::record`]): `next`, the
  /// packet it reads first, deciding the framing either holds where it
  /// re-guesses it ([`Self::decided_by`]).
  fn record(
    &self,
    fresh: &Self,
    base: &[u8],
    next: Option<&[u8]>,
  ) -> Result<Option<Record>, Unrecordable> {
    let decided = self.decided_by(next);
    let this = decided.as_ref().unwrap_or(self);
    let fresh_decided = fresh.decided_by(next);
    if this.same(fresh_decided.as_ref().unwrap_or(fresh)) {
      return Ok(None);
    }
    let framing = this.framing().ok_or(Unrecordable::Unknown)?;
    let sequences: Vec<&SequenceHeld> = this.sps.iter().flatten().collect();
    let pictures: Vec<&PictureHeld> = this.pps.iter().flatten().collect();
    if sequences.iter().any(|sps| sps.doubt) || pictures.iter().any(|pps| pps.doubt) {
      return Err(Unrecordable::Unknown);
    }
    if sequences.iter().any(|sps| sps.set.past_end()) {
      return Err(Unrecordable::PastEnd(ParameterSet::Sequence));
    }
    if pictures.iter().any(|pps| pps.past_end) {
      return Err(Unrecordable::PastEnd(ParameterSet::Picture));
    }
    // A picture parameter set read under a sequence parameter set its id no
    // longer holds: a record reads every sequence parameter set first, and
    // binds it to the one held now.
    if pictures.iter().any(|pps| {
      this.sps[usize::from(pps.sps_id)]
        .as_ref()
        .is_none_or(|sps| *sps.set != *pps.bound)
    }) {
      return Err(Unrecordable::Superseded);
    }
    // Start codes under a NAL length size of four, which the next packet
    // re-guesses: an `avcC` record of four-byte fields, whose framing that
    // packet re-guesses alike.
    let framing = match framing {
      H264Framing::AnnexB { reguess: true } if decided.is_some() => H264Framing::Avc(4),
      framing => framing,
    };
    let mut bytes = Vec::new();
    match framing {
      H264Framing::Avc(size @ 1..=4) => {
        if sequences.len() > 31 {
          return Err(Unrecordable::TooMany(ParameterSet::Sequence));
        }
        if pictures.len() > 255 {
          return Err(Unrecordable::TooMany(ParameterSet::Picture));
        }
        // The profile, its constraints and the level, which FFmpeg does not
        // read (h264_parse.c:475-488): the record's, or the first set's.
        let profile = if base.first() == Some(&1) && base.len() >= 4 {
          [base[1], base[2], base[3]]
        } else {
          let unit: &[u8] = sequences
            .first()
            .and_then(|sps| sps.set.unit.kept())
            .unwrap_or_default();
          core::array::from_fn(|at| unit.get(at + 1).copied().unwrap_or(0))
        };
        bytes.extend_from_slice(&[
          1,
          profile[0],
          profile[1],
          profile[2],
          0xfc | (size - 1),
          0xe0 | sequences.len() as u8,
        ]);
        for sps in &sequences {
          entry(&mut bytes, &sps.set.unit, ParameterSet::Sequence)?;
        }
        bytes.push(pictures.len() as u8);
        for pps in &pictures {
          entry(&mut bytes, &pps.unit, ParameterSet::Picture)?;
        }
      }
      H264Framing::AnnexB { reguess: false } => {
        for (unit, set) in sequences
          .iter()
          .map(|sps| (&sps.set.unit, ParameterSet::Sequence))
          .chain(
            pictures
              .iter()
              .map(|pps| (&*pps.unit, ParameterSet::Picture)),
          )
        {
          bytes.extend_from_slice(&[0, 0, 1]);
          bytes.extend_from_slice(unit.kept().ok_or(Unrecordable::Oversized(set))?);
        }
      }
      H264Framing::Avc(_) | H264Framing::AnnexB { reguess: true } => {
        return Err(Unrecordable::Framing);
      }
    }
    let mut read = H264::default();
    let mut write = H264Write::new(&read);
    let _ = write.record(&bytes);
    if let Some(next) = write.next {
      read = next;
    }
    if !read.decided_by(next).as_ref().unwrap_or(&read).same(this) {
      return Err(Unrecordable::Unverified);
    }
    Ok(Some(Record {
      bytes,
      strict: false,
    }))
  }
}

/// `unit` as an `avcC` or `hvcC` entry: its length in two bytes, then the
/// unit — refused where it does not fit them.
fn entry(bytes: &mut Vec<u8>, unit: &Bytes, set: ParameterSet) -> Result<(), Unrecordable> {
  let unit = unit.kept().ok_or(Unrecordable::Oversized(set))?;
  let length = u16::try_from(unit.len()).map_err(|_| Unrecordable::Oversized(set))?;
  bytes.extend_from_slice(&length.to_be_bytes());
  bytes.extend_from_slice(unit);
  Ok(())
}

/// A reading's view of an H.264 decoder's sets: `base` until the reading
/// changes something, then its own copy.
struct H264Write<'a> {
  base: &'a H264,
  next: Option<H264>,
}

impl<'a> H264Write<'a> {
  const fn new(base: &'a H264) -> Self {
    Self { base, next: None }
  }

  fn now(&self) -> &H264 {
    self.next.as_ref().unwrap_or(self.base)
  }

  fn edit(&mut self) -> &mut H264 {
    let base = self.base;
    self.next.get_or_insert_with(|| base.clone())
  }

  /// `ff_h264_decode_extradata` applying `record` (h264_parse.c:466-524):
  /// its sets as FFmpeg stores them and its framing; nothing where it is
  /// empty, which FFmpeg refuses before reading (472-473). Answers the
  /// verdict.
  fn record(&mut self, record: &[u8]) -> Result<(), crate::ExtradataRejection> {
    if record.is_empty() {
      return Ok(());
    }
    let read = params::h264_extradata(record, self);
    if self.now().is_avc != Some(read.is_avc) {
      self.edit().is_avc = Some(read.is_avc);
    }
    if let Some(size) = read.nal_length_size
      && self.now().nal_length_size != Some(size)
    {
      self.edit().nal_length_size = Some(size);
    }
    read.verdict
  }

  /// **What FFmpeg's H.264 decoder reads off a packet's body**, under the
  /// framing it holds ([`reading`], [`Self::read_body`]). Where that cannot
  /// be told, every set any framing it may hold would store is in doubt, and
  /// so is the framing it holds after it.
  fn body(&mut self, data: &[u8]) {
    let now = self.now();
    match reading(now.is_avc, now.nal_length_size, data) {
      Some(reading) => self.read_body(data, reading),
      None => self.body_in_doubt(data),
    }
  }

  /// `h264_decode_frame`'s reading of a packet's body as `reading` says: an
  /// `avcC` record applied as extradata, its sets and its framing, no unit
  /// read as a slice (h264dec.c:1045-1050); or its units, split as the
  /// framing re-guessed for it says, that framing the decoder's from then
  /// on (602-610; [`params::h264_packet`]).
  fn read_body(&mut self, data: &[u8], reading: BodyReading) {
    match reading {
      BodyReading::Record => {
        let _ = self.record(data);
      }
      BodyReading::Units { is_avc, size } => {
        if self.now().is_avc != Some(is_avc) {
          self.edit().is_avc = Some(is_avc);
        }
        params::h264_packet(data, is_avc, usize::from(size), self);
      }
    }
  }

  /// [`Self::body`] where the framing cannot be told: the body read under
  /// every framing the decoder may hold, what any reading changes in doubt
  /// over what is held, and the framing after it known only where every
  /// reading leaves the same.
  fn body_in_doubt(&mut self, data: &[u8]) {
    let before = self.now().clone();
    let avc: &[bool] = match before.is_avc {
      Some(true) => &[true],
      Some(false) => &[false],
      None => &[false, true],
    };
    let sizes = before
      .nal_length_size
      .map_or(vec![1u8, 2, 3, 4], |size| vec![size]);
    let mut framings = Vec::new();
    for &is_avc in avc {
      for &size in &sizes {
        let Some(reading) = reading(Some(is_avc), Some(size), data) else {
          continue;
        };
        let mut probe = H264Write::new(&before);
        probe.read_body(data, reading);
        let mut next = probe.next.unwrap_or_else(|| before.clone());
        framings.push((next.is_avc, next.nal_length_size));
        next.doubt_since(&before);
        // What this framing would change, in doubt over what is held.
        let edit = self.edit();
        for id in 0..edit.sps.len() {
          if next.sps[id].as_ref().is_some_and(|sps| sps.doubt) {
            let entry = edit.sps[id].clone().or_else(|| next.sps[id].clone());
            edit.sps[id] = entry.map(|entry| SequenceHeld {
              doubt: true,
              ..entry
            });
          }
        }
        for id in 0..edit.pps.len() {
          if next.pps[id].as_ref().is_some_and(|pps| pps.doubt) {
            let entry = edit.pps[id].clone().or_else(|| next.pps[id].clone());
            edit.pps[id] = entry.map(|entry| PictureHeld {
              doubt: true,
              ..entry
            });
          }
        }
      }
    }
    let first = framings.first().copied();
    let edit = self.edit();
    edit.is_avc = first
      .and_then(|(is_avc, _)| is_avc)
      .filter(|&is_avc| framings.iter().all(|framing| framing.0 == Some(is_avc)));
    edit.nal_length_size = first
      .and_then(|(_, size)| size)
      .filter(|&size| framings.iter().all(|framing| framing.1 == Some(size)));
  }
}

/// How FFmpeg's H.264 decoder reads a packet's body, its framing told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyReading {
  /// As an `avcC` record it applies as extradata.
  Record,
  /// As NAL units, length-prefixed by `size` bytes where `is_avc`, or
  /// start-coded: the framing it holds once it re-guessed it.
  Units { is_avc: bool, size: u8 },
}

/// **How FFmpeg's H.264 decoder reads the packet `data`** under the framing
/// `is_avc` and `size` it holds (`None` where that cannot be told): as an
/// `avcC` record where its framing is `avcC` and the body reads as one
/// (h264dec.c:1045-1050), tested before the units are; otherwise as units
/// split under the framing it re-guesses for the packet where its NAL length
/// size is four ([`reguessed`]). `None` where that cannot be told.
fn reading(is_avc: Option<bool>, size: Option<u8>, data: &[u8]) -> Option<BodyReading> {
  if params::avcc_body(data) {
    match is_avc {
      Some(true) => return Some(BodyReading::Record),
      None => return None,
      Some(false) => {}
    }
  }
  match (reguessed(is_avc, size, data)?, size) {
    // Start codes split alike whatever the length size.
    (false, size) => Some(BodyReading::Units {
      is_avc: false,
      size: size.unwrap_or(0),
    }),
    (true, Some(size)) => Some(BodyReading::Units { is_avc: true, size }),
    (true, None) => None,
  }
}

/// The `is_avc` FFmpeg's H.264 decoder holds once it re-guessed the framing
/// of the packet `data` (h264dec.c:602-607; [`params::h264_reguess`]) — which
/// it does where its NAL length size is four — from `is_avc` under the NAL
/// length size `size`; `None` where that cannot be told.
fn reguessed(is_avc: Option<bool>, size: Option<u8>, data: &[u8]) -> Option<bool> {
  let guess = params::h264_reguess(data);
  match size {
    Some(4) => guess.or(is_avc),
    Some(_) => is_avc,
    // It may re-guess, or not.
    None => match (guess, is_avc) {
      (None, is_avc) => is_avc,
      (Some(guess), Some(is_avc)) if guess == is_avc => Some(is_avc),
      _ => None,
    },
  }
}

impl H264Sets for H264Write<'_> {
  fn sps(&self, id: usize) -> Option<params::Sps> {
    self.now().sps.get(id)?.as_ref().map(|sps| sps.set.facts)
  }

  fn store_sps(
    &mut self,
    id: usize,
    facts: params::Sps,
    unit: &Unit<'_>,
    memory: &[u8],
    reading: u8,
  ) {
    let set = Sequence {
      unit: Bytes::of(unit.raw()),
      identity: Bytes::of(&params::h264_identity(unit, memory, reading)),
      reading,
      facts,
    };
    // A set identical to the one held leaves that one in place, and what
    // was read under it (h264_ps.c:578-587). One read past its payload is
    // taken as new: what it read past it may differ.
    if let Some(held) = &self.now().sps[id]
      && !set.past_end()
      && *held.set == set
    {
      if held.doubt {
        let set = held.set.clone();
        self.edit().sps[id] = Some(SequenceHeld { set, doubt: false });
      }
      return;
    }
    self.edit().sps[id] = Some(SequenceHeld {
      set: Arc::new(set),
      doubt: false,
    });
  }

  fn store_pps(&mut self, id: usize, sps: usize, unit: &Unit<'_>, memory: &[u8], past_end: bool) {
    // `h264_pps` found it held.
    let Some(held) = self.now().sps[sps].clone() else {
      return;
    };
    let pps = PictureHeld {
      unit: Arc::new(Bytes::of(unit.raw())),
      identity: Arc::new(Bytes::of(&params::h264_identity(unit, memory, 1))),
      sps_id: sps as u8,
      bound: held.set,
      past_end,
      doubt: held.doubt,
    };
    // FFmpeg replaces a picture parameter set whatever it held
    // (h264_ps.c:839-840); an identical one bound alike changes nothing.
    if self.now().pps[id]
      .as_ref()
      .is_some_and(|held| held.same(&pps))
    {
      return;
    }
    self.edit().pps[id] = Some(pps);
  }
}

// ---------------------------------------------------------------------------
//  HEVC
// ---------------------------------------------------------------------------

/// An HEVC parameter set a decoder stored: the unit it read, which a record
/// carries, and the bytes FFmpeg compares to keep an identical set in place
/// — the unit as handed over, to its last payload byte (`get_bits_bytesize`,
/// hevc/ps.c:797-802, 1729-1733, 2219-2223), which alone tell one set from
/// another: the raw bytes are not compared.
#[derive(Debug)]
struct HevcSet {
  unit: Bytes,
  identity: Bytes,
}

impl PartialEq for HevcSet {
  fn eq(&self, other: &Self) -> bool {
    self.identity == other.identity
  }
}

impl Eq for HevcSet {}

/// A video parameter set held.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VideoHeld {
  set: Arc<HevcSet>,
  /// What FFmpeg stored of it ([`params::hevc_vps`]).
  read: params::VpsRead,
  /// Whether its reading ran past its payload: what FFmpeg stored of it
  /// was read off the bytes after it, which a record changes.
  past_end: bool,
  /// Whether a sequence parameter set this table holds no id of may be
  /// stored referring to it: one read while it was in doubt, refused before
  /// its id was read against the set held here, which may not be the one
  /// the decoder holds. Such a set goes where this one is replaced for
  /// certain (`remove_vps`, hevc/ps.c:102-111).
  orphans: bool,
  doubt: bool,
}

/// A sequence parameter set held: the video parameter set it refers to, and
/// what FFmpeg stored of it — `None` for one held in doubt that was read
/// against a video parameter set held in doubt and refused there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HevcSequenceHeld {
  set: Arc<HevcSet>,
  vps_id: u8,
  facts: Option<params::HevcSps>,
  doubt: bool,
}

/// A picture parameter set held: the sequence parameter set it refers to,
/// and whether its reading ran past its payload — FFmpeg stores it so, its
/// last fields read off the bytes after it (hevc/ps.c:2458-2461), which a
/// record changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HevcPictureHeld {
  set: Arc<HevcSet>,
  sps_id: u8,
  past_end: bool,
  doubt: bool,
}

/// **What FFmpeg's HEVC decoder holds**: its video, sequence and picture
/// parameter sets (`ps->vps_list`, `ps->sps_list`, `ps->pps_list`) and its
/// framing (`is_nalff`, `nal_length_size`).
#[derive(Clone, Debug)]
pub(super) struct Hevc {
  vps: [Option<VideoHeld>; 16],
  sps: [Option<HevcSequenceHeld>; 16],
  pps: [Option<HevcPictureHeld>; 64],
  is_nalff: bool,
  nal_length_size: u8,
  /// Whether the framing cannot be told.
  framing_doubt: bool,
}

impl Default for Hevc {
  /// A decoder opened on no extradata: no set, start-coded.
  fn default() -> Self {
    Self {
      vps: Default::default(),
      sps: Default::default(),
      pps: core::array::from_fn(|_| None),
      is_nalff: false,
      nal_length_size: 0,
      framing_doubt: false,
    }
  }
}

impl Hevc {
  /// The NAL length field's width packets are read with: `None` for
  /// start codes.
  fn nal_length(&self) -> Option<usize> {
    self.is_nalff.then_some(usize::from(self.nal_length_size))
  }

  fn same(&self, other: &Self) -> bool {
    self.nal_length() == other.nal_length()
      && self.framing_doubt == other.framing_doubt
      && self.vps == other.vps
      && self.sps == other.sps
      && self.pps == other.pps
  }

  fn doubt_since(&mut self, proven: &Self) {
    for id in 0..self.vps.len() {
      if self.vps[id] != proven.vps[id] {
        let entry = self.vps[id].clone().or_else(|| proven.vps[id].clone());
        self.vps[id] = entry.map(|entry| VideoHeld {
          doubt: true,
          ..entry
        });
      }
    }
    for id in 0..self.sps.len() {
      let refers_to_doubt = self.sps[id].as_ref().is_some_and(|sps| {
        self.vps[usize::from(sps.vps_id)]
          .as_ref()
          .is_none_or(|vps| vps.doubt)
      });
      if self.sps[id] != proven.sps[id] || refers_to_doubt {
        let entry = self.sps[id].clone().or_else(|| proven.sps[id].clone());
        self.sps[id] = entry.map(|entry| HevcSequenceHeld {
          doubt: true,
          ..entry
        });
      }
    }
    for id in 0..self.pps.len() {
      let refers_to_doubt = self.pps[id].as_ref().is_some_and(|pps| {
        self.sps[usize::from(pps.sps_id)]
          .as_ref()
          .is_none_or(|sps| sps.doubt)
      });
      if self.pps[id] != proven.pps[id] || refers_to_doubt {
        let entry = self.pps[id].clone().or_else(|| proven.pps[id].clone());
        self.pps[id] = entry.map(|entry| HevcPictureHeld {
          doubt: true,
          ..entry
        });
      }
    }
    if self.nal_length() != proven.nal_length() || proven.framing_doubt {
      self.framing_doubt = true;
    }
  }

  /// The record a decoder opened fresh opens on to hold this, against
  /// `fresh`, what one opened on `base` holds ([`Held::record`]).
  fn record(&self, fresh: &Self, base: &[u8]) -> Result<Option<Record>, Unrecordable> {
    if self.same(fresh) {
      return Ok(None);
    }
    if self.framing_doubt
      || self
        .vps
        .iter()
        .flatten()
        .any(|vps| vps.doubt || vps.orphans)
      || self.sps.iter().flatten().any(|sps| sps.doubt)
      || self.pps.iter().flatten().any(|pps| pps.doubt)
    {
      return Err(Unrecordable::Unknown);
    }
    // A set FFmpeg read past its payload holds what it read off the bytes
    // after it, which a record changes: a video parameter set stored where
    // its id held nothing (hevc/ps.c:944-952), a picture parameter set
    // stored with a warning (2458-2461). FFmpeg refuses a sequence parameter
    // set read so (1711-1716).
    if self.vps.iter().flatten().any(|vps| vps.past_end) {
      return Err(Unrecordable::PastEnd(ParameterSet::Video));
    }
    if self.pps.iter().flatten().any(|pps| pps.past_end) {
      return Err(Unrecordable::PastEnd(ParameterSet::Picture));
    }
    let arrays: [(u8, ParameterSet, Vec<&HevcSet>); 3] = [
      (
        32,
        ParameterSet::Video,
        self.vps.iter().flatten().map(|vps| &*vps.set).collect(),
      ),
      (
        33,
        ParameterSet::Sequence,
        self.sps.iter().flatten().map(|sps| &*sps.set).collect(),
      ),
      (
        34,
        ParameterSet::Picture,
        self.pps.iter().flatten().map(|pps| &*pps.set).collect(),
      ),
    ];
    let mut bytes = Vec::new();
    if self.is_nalff {
      // What FFmpeg reads of the header is its version, the NAL length size
      // and the count of arrays (hevc/parse.c:93-101): the rest is the
      // record's own where it is an `hvcC` record.
      let mut header = [0u8; 23];
      if hvcc(base) {
        header.copy_from_slice(&base[..23]);
      }
      header[0] = 1;
      header[21] = (header[21] & !3) | (self.nal_length_size.clamp(1, 4) - 1);
      header[22] = arrays
        .iter()
        .filter(|(_, _, units)| !units.is_empty())
        .count() as u8;
      bytes.extend_from_slice(&header);
      for (kind, set, units) in &arrays {
        if units.is_empty() {
          continue;
        }
        bytes.push(*kind);
        let count = u16::try_from(units.len()).map_err(|_| Unrecordable::TooMany(*set))?;
        bytes.extend_from_slice(&count.to_be_bytes());
        for unit in units {
          entry(&mut bytes, &unit.unit, *set)?;
        }
      }
    } else {
      for (_, set, units) in &arrays {
        for unit in units {
          bytes.extend_from_slice(&[0, 0, 1]);
          bytes.extend_from_slice(unit.unit.kept().ok_or(Unrecordable::Oversized(*set))?);
        }
      }
    }
    let mut read = Hevc::default();
    let mut write = HevcWrite::new(&read);
    let _ = write.record(&bytes);
    if let Some(next) = write.next {
      read = next;
    }
    if !read.same(self) {
      return Err(Unrecordable::Unverified);
    }
    // FFmpeg's own decoder opened strictly answers as the decoder serving
    // does only where no set the record carries is one FFmpeg stores with a
    // warning `AV_EF_EXPLODE` turns into its refusal (hevc/ps.c:858-862,
    // 1416-1424): there it is a second witness; elsewhere this reading is the
    // only one, and it holds such a set as FFmpeg stores it.
    let warned = self.vps.iter().flatten().any(|vps| vps.read.warned)
      || self
        .sps
        .iter()
        .flatten()
        .any(|sps| sps.facts.is_some_and(|facts| facts.warned));
    Ok(Some(Record {
      bytes,
      strict: !warned,
    }))
  }
}

/// FFmpeg's own `hvcC` test (`ff_hevc_decode_extradata`, hevc/parse.c:93):
/// 23 bytes or more, its first byte 1, or 0 with a second or third byte no
/// start code has.
fn hvcc(record: &[u8]) -> bool {
  record.len() >= 23 && (record[0] == 1 || (record[0] == 0 && (record[1] != 0 || record[2] > 1)))
}

/// What FFmpeg made of one HEVC parameter set a reading met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
  /// Stored, kept, or dropped with no error: the reading goes on.
  Taken,
  /// Refused; `aborts` where the error is other than invalid data, which
  /// ends a decoder's reading of the packet (`decode_nal_unit`,
  /// hevc/hevcdec.c:3665-3672).
  Refused { aborts: bool },
  /// What FFmpeg does with the set cannot be told: it is read against a set
  /// held in doubt. The set is held in doubt; so is whether the reading goes
  /// on, where a refusal would end it — any refusal in an extradata buffer,
  /// in a packet one other than invalid data, which `may_end` says the set
  /// can be refused with.
  Unknown { may_end: bool },
}

/// A reading's view of an HEVC decoder's sets: `base` until the reading
/// changes something, then its own copy.
pub(super) struct HevcWrite<'a> {
  base: &'a Hevc,
  next: Option<Hevc>,
  /// Whether a video parameter set the reading met is stored as alpha
  /// video, or may be.
  pub(super) alpha: bool,
}

impl<'a> HevcWrite<'a> {
  pub(super) const fn new(base: &'a Hevc) -> Self {
    Self {
      base,
      next: None,
      alpha: false,
    }
  }

  fn now(&self) -> &Hevc {
    self.next.as_ref().unwrap_or(self.base)
  }

  fn edit(&mut self) -> &mut Hevc {
    let base = self.base;
    self.next.get_or_insert_with(|| base.clone())
  }

  /// **`ff_hevc_decode_extradata`** (hevc/parse.c:79-145) applying `record`:
  /// an `hvcC` record entry by entry, each its own buffer read as
  /// `hevc_decode_nal_units` reads one (24-77), a refusal ending its entry,
  /// the framing set to NAL length fields of two bytes while the entries are
  /// read and to the record's own after them; anything else as one
  /// start-coded buffer. An entry running past the record fails it there —
  /// "Invalid NAL unit size in extradata" (115-119) — the sets before it
  /// stored, the framing two-byte length fields: answers `false`, as FFmpeg
  /// fails the open, or the packet, then. An empty record applies nothing.
  pub(super) fn record(&mut self, record: &[u8]) -> bool {
    if record.is_empty() {
      return true;
    }
    if !hvcc(record) {
      self.framing(false, self.now().nal_length_size);
      self.read(record, record.len(), None, true);
      return true;
    }
    self.framing(true, 2);
    let size = record.len();
    let byte = |at: usize| record.get(at).copied().unwrap_or(0);
    let mut at = 23usize;
    for _ in 0..byte(22) {
      at = (at + 1).min(size); // the array's type
      // `bytestream2_get_be16`: 0, at the end, where fewer than two bytes
      // are left.
      let count = if size - at < 2 {
        0
      } else {
        (usize::from(byte(at)) << 8) | usize::from(byte(at + 1))
      };
      at = (at + 2).min(size);
      for _ in 0..count {
        let nalsize = ((usize::from(byte(at)) << 8) | usize::from(byte(at + 1))) + 2;
        if size - at < nalsize {
          return false;
        }
        self.read(&record[at..], nalsize, Some(2), true);
        at += nalsize;
      }
    }
    self.framing(true, (record[21] & 3) + 1);
    true
  }

  fn framing(&mut self, is_nalff: bool, nal_length_size: u8) {
    let now = self.now();
    if now.is_nalff != is_nalff || now.nal_length_size != nal_length_size || now.framing_doubt {
      let edit = self.edit();
      edit.is_nalff = is_nalff;
      edit.nal_length_size = nal_length_size;
      edit.framing_doubt = false;
    }
  }

  /// **The parameter sets FFmpeg's HEVC decoder stores off a packet's
  /// units** (`decode_nal_units`, hevc/hevcdec.c:3681-3776), framed as the
  /// decoder frames packets.
  pub(super) fn units(&mut self, data: &[u8]) {
    let nal_length = self.now().nal_length();
    self.read(data, data.len(), nal_length, false);
  }

  /// The units of the first `length` bytes of `mem`, as one of FFmpeg's
  /// readings splits them (`H2645_FLAG_SMALL_PADDING`), length-prefixed by
  /// `nal_length` bytes or start-coded: an extradata buffer's, where any
  /// refusal ends the reading (`hevc_decode_nal_units`, hevc/parse.c:24-77),
  /// or a packet's, where only one other than invalid data does
  /// (hevc/hevcdec.c:3664-3672, 3770-3775); nothing where the split fails.
  ///
  /// A unit whose parser this crate does not run — an SEI message, in a
  /// packet a slice — may end the reading: the sets after one are taken in
  /// doubt, as are the sets after one whose reading cannot be told
  /// ([`Read::Unknown`]).
  fn read(&mut self, mem: &[u8], length: usize, nal_length: Option<usize>, extradata: bool) {
    let mut units = Walk::new(
      mem,
      length,
      nal_length.unwrap_or(0),
      Codec::Hevc,
      nal_length.is_some(),
      true,
    );
    if !units.accepted() {
      return;
    }
    let mut scratch = Vec::new();
    let mut certain = true;
    while let Some(Ok(unit)) = units.next() {
      let read = match unit.kind {
        32 => self.vps(&units, &unit, &mut scratch, certain),
        33 => self.sps(&units, &unit, &mut scratch, certain),
        34 => self.pps(&units, &unit, &mut scratch, certain),
        39 | 40 => {
          certain = false;
          Read::Taken
        }
        0..=9 | 16..=21 if !extradata => {
          certain = false;
          Read::Taken
        }
        _ => Read::Taken,
      };
      match read {
        Read::Refused { aborts } if extradata || aborts => return,
        Read::Unknown { may_end } if extradata || may_end => certain = false,
        Read::Taken | Read::Refused { .. } | Read::Unknown { .. } => {}
      }
    }
  }

  /// `ff_hevc_decode_nal_vps` (hevc/ps.c:786-959) on the video parameter set
  /// `unit`, the walk standing past it, against the sets held: an identical
  /// set changes nothing (797-802); one read past its payload is refused
  /// where its id holds a set (944-949); one stored drops the sequence
  /// parameter sets that refer to its id (`remove_vps`, 102-111). A set the
  /// decoder `certain`ly read is held as it stands, any other in doubt, and
  /// so is what refers to its id.
  fn vps(
    &mut self,
    walk: &Walk<'_>,
    unit: &Unit<'_>,
    scratch: &mut Vec<u8>,
    certain: bool,
  ) -> Read {
    let memory = walk.memory(unit, scratch);
    let mut reader = unit.reader(memory);
    let id = reader.bits(4) as usize;
    let size = ((unit.size_bits + 7) >> 3) as usize;
    let identity = memory.get(..size).unwrap_or(memory);
    let held = self.now().vps[id].clone();
    if held
      .as_ref()
      .is_some_and(|held| !held.doubt && held.set.identity.is(identity))
    {
      return Read::Taken;
    }
    let read = match params::hevc_vps(&mut reader) {
      Ok(read) => read,
      Err(aborts) => return Read::Refused { aborts },
    };
    let past_end = reader.left() < 0;
    // Held for certain, a set read past its payload is refused; held in
    // doubt, it is stored only where the decoder holds nothing of the id,
    // which cannot be told.
    if past_end && held.as_ref().is_some_and(|held| !held.doubt) {
      return Read::Refused { aborts: false };
    }
    let doubt = !certain || (past_end && held.is_some());
    self.alpha |= read.alpha;
    let set = Arc::new(HevcSet {
      unit: Bytes::of(unit.raw()),
      identity: Bytes::of(identity),
    });
    let replaces = held.as_ref().is_some_and(|held| !held.doubt) && !doubt;
    let edit = self.edit();
    for sps in 0..edit.sps.len() {
      if edit.sps[sps]
        .as_ref()
        .is_some_and(|held| usize::from(held.vps_id) == id)
      {
        if replaces {
          edit.drop_sps(sps);
        } else {
          edit.sps_in_doubt(sps);
        }
      }
    }
    edit.vps[id] = Some(VideoHeld {
      set,
      read,
      past_end,
      // What may refer to the set replaced goes only where it is replaced
      // for certain.
      orphans: !replaces && held.is_some_and(|held| held.orphans),
      doubt,
    });
    Read::Taken
  }

  /// `ff_hevc_decode_nal_sps` (hevc/ps.c:1735-1786) on the sequence parameter
  /// set `unit`, read whole ([`params::hevc_sps`]) against the video
  /// parameter set held under its id, which must be held (1252-1258): an
  /// identical set changes nothing (`compare_sps`, 1729-1733, 1774-1780),
  /// any other drops the picture parameter sets that refer to its id
  /// (`remove_sps`, 89-100). Read against a video parameter set held in
  /// doubt, what FFmpeg does with it cannot be told: the set is held in
  /// doubt under its id, or, where the reading did not reach its id, the
  /// video parameter set is held as one a set of an unknown id may refer to.
  fn sps(
    &mut self,
    walk: &Walk<'_>,
    unit: &Unit<'_>,
    scratch: &mut Vec<u8>,
    certain: bool,
  ) -> Read {
    let memory = walk.memory(unit, scratch);
    let mut reader = unit.reader(memory);
    let vps_id = reader.bits(4) as usize;
    let Some(vps) = self.now().vps[vps_id].clone() else {
      return Read::Refused { aborts: false };
    };
    let parsed = params::hevc_sps(&mut reader, unit.layer, &vps.read);
    let size = ((unit.size_bits + 7) >> 3) as usize;
    let identity = memory.get(..size).unwrap_or(memory);
    let set = || {
      Arc::new(HevcSet {
        unit: Bytes::of(unit.raw()),
        identity: Bytes::of(identity),
      })
    };
    if vps.doubt {
      let id = match parsed {
        Ok(read) => Some(read.id),
        Err(refused) => refused.id,
      };
      let edit = self.edit();
      match id.map(usize::from) {
        Some(id) => {
          edit.sps_in_doubt(id);
          if edit.sps[id].is_none() {
            edit.sps[id] = Some(HevcSequenceHeld {
              set: set(),
              vps_id: vps_id as u8,
              facts: parsed.ok(),
              doubt: true,
            });
          }
        }
        None => {
          if let Some(vps) = &mut edit.vps[vps_id] {
            vps.orphans = true;
          }
        }
      }
      return Read::Unknown { may_end: true };
    }
    let read = match parsed {
      Ok(read) => read,
      Err(refused) => {
        return Read::Refused {
          aborts: refused.aborts,
        };
      }
    };
    let id = usize::from(read.id);
    let doubt = !certain;
    let held = self.now().sps[id].clone();
    let identical = held
      .as_ref()
      .is_some_and(|held| held.set.identity.is(identity));
    if identical && held.as_ref().is_some_and(|held| !held.doubt || doubt) {
      return Read::Taken;
    }
    let replaces = held.as_ref().is_none_or(|held| !held.doubt) && !doubt && !identical;
    let edit = self.edit();
    if replaces {
      edit.drop_sps(id);
    } else {
      edit.sps_in_doubt(id);
    }
    edit.sps[id] = Some(HevcSequenceHeld {
      set: set(),
      vps_id: vps_id as u8,
      facts: Some(read),
      doubt,
    });
    Read::Taken
  }

  /// `ff_hevc_decode_nal_pps` (hevc/ps.c:2201-2471) on the picture parameter
  /// set `unit`: an id under 64, an identical set changing nothing
  /// (2219-2223), a sequence parameter set id under 16 that is held
  /// (2248-2258), then the rest read whole ([`params::hevc_pps`]) against
  /// that set and the video parameter set held under its id — a set read
  /// past its payload stored as such. Read against a set held in doubt, what
  /// FFmpeg does with it cannot be told: it is held in doubt. FFmpeg refuses
  /// a picture parameter set only with invalid data, which ends no packet's
  /// reading.
  fn pps(
    &mut self,
    walk: &Walk<'_>,
    unit: &Unit<'_>,
    scratch: &mut Vec<u8>,
    certain: bool,
  ) -> Read {
    let memory = walk.memory(unit, scratch);
    let mut reader = unit.reader(memory);
    let id = reader.ue_long() as usize;
    if id >= 64 {
      return Read::Refused { aborts: false };
    }
    let size = ((unit.size_bits + 7) >> 3) as usize;
    let identity = memory.get(..size).unwrap_or(memory);
    let held = self.now().pps[id].clone();
    let identical = held
      .as_ref()
      .is_some_and(|held| held.set.identity.is(identity));
    if identical && held.as_ref().is_some_and(|held| !held.doubt || !certain) {
      return Read::Taken;
    }
    let sps_id = reader.ue_long() as usize;
    if sps_id >= 16 {
      return Read::Refused { aborts: false };
    }
    let Some(sps) = self.now().sps[sps_id].clone() else {
      return Read::Refused { aborts: false };
    };
    let vps = self.now().vps[usize::from(sps.vps_id)].clone();
    let verdict = match (&sps.facts, &vps) {
      (Some(facts), Some(vps)) if !sps.doubt && !vps.doubt => {
        Some(params::hevc_pps(&mut reader, facts, vps.read.max_layers()))
      }
      _ => None,
    };
    let (past_end, doubt) = match verdict {
      Some(params::HevcPps::Refused) => return Read::Refused { aborts: false },
      Some(params::HevcPps::Dropped) => return Read::Taken,
      Some(params::HevcPps::Stored { past_end }) => (past_end, !certain),
      None => (false, true),
    };
    self.edit().pps[id] = Some(HevcPictureHeld {
      set: Arc::new(HevcSet {
        unit: Bytes::of(unit.raw()),
        identity: Bytes::of(identity),
      }),
      sps_id: sps_id as u8,
      past_end,
      doubt,
    });
    if verdict.is_none() {
      return Read::Unknown { may_end: false };
    }
    Read::Taken
  }
}

impl Hevc {
  /// `remove_sps` (hevc/ps.c:89-100): the sequence parameter set held under
  /// `id`, and every picture parameter set referring to it, dropped.
  fn drop_sps(&mut self, id: usize) {
    if self.sps[id].take().is_some() {
      for pps in self.pps.iter_mut() {
        if pps
          .as_ref()
          .is_some_and(|held| usize::from(held.sps_id) == id)
        {
          *pps = None;
        }
      }
    }
  }

  /// The sequence parameter set held under `id`, and every picture parameter
  /// set referring to it, in doubt: whether a set that may have replaced it
  /// did cannot be told.
  fn sps_in_doubt(&mut self, id: usize) {
    if let Some(sps) = &mut self.sps[id] {
      sps.doubt = true;
    }
    for pps in self.pps.iter_mut().flatten() {
      if usize::from(pps.sps_id) == id {
        pps.doubt = true;
      }
    }
  }
}
