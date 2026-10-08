//! **Where a decoder can start without losing a picture**: the clean
//! random access points at which a software session returns to its threads
//! after a fallback, and the random-access pictures at which a post-commit
//! resync is anchored (see `CarrierVideoStreamDecoder::send_on_software`) —
//! each proved by the bitstream, never inferred from a decoder's state.
//!
//! A keyframe is a *clean* random access point when nothing after it in
//! decode order references a picture before it. A decoder opened there
//! then decodes every picture a decoder running through it would. An
//! open-GOP keyframe is not one. That covers an HEVC CRA, whose RASL
//! leading pictures reference the GOP before it, and an H.264 I-frame that
//! is a recovery point rather than an IDR. A decoder opened at such a
//! keyframe drops or conceals those pictures, so switching decoders there
//! would trade pictures for threads.
//!
//! A codec that codes every picture alone — its descriptor carries
//! `AV_CODEC_PROP_INTRA_ONLY`: ProRes, DNxHD, MJPEG, … — has no reference
//! to lose, so every one of its packets is both, its key flag or not.

use crate::CodecId;

#[cfg(test)]
mod tests;

/// How a stream's keyframes are read for cleanliness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyframeRule {
  /// H.264: a keyframe is clean when its first picture's NAL unit (types
  /// 1–5) is an IDR slice (5) that starts its picture — its
  /// `first_mb_in_slice` is 0. A packet with any picture before the IDR is
  /// not: a decoder started there would begin on a picture that references
  /// what it never saw; nor is one that opens on a later slice of the IDR
  /// picture, whose leading slices a decoder started there never sees. Its
  /// NAL units are length-prefixed by `nal_length` bytes (`avcC`), or
  /// start-coded (Annex B) when `None`.
  ///
  /// A stream whose sequence parameter set permits arbitrary slice order
  /// (`aso`) is read as having neither clean points nor anchors: its slices
  /// may come in any order, so the one holding macroblock 0 proves nothing
  /// about the slices before it.
  H264 {
    /// The NAL length field's width, from the `avcC` record.
    nal_length: Option<usize>,
    /// Whether the stream's sequence parameter set permits arbitrary slice
    /// order ([`sps_permits_aso`]).
    aso: bool,
  },
  /// HEVC: clean when its first base-layer picture is an IDR or a BLA (NAL
  /// types 16–20) whose first slice segment starts it
  /// (`first_slice_segment_in_pic_flag`). A BLA's RASL pictures are
  /// discarded by every decoder, so a new one loses nothing there. A CRA
  /// (21) is never clean.
  ///
  /// A stream whose video parameter set declares an auxiliary layer
  /// (`alpha`) is read as having neither clean points nor anchors: FFmpeg's
  /// HEVC decoder decodes that layer beside the base one, as the alpha
  /// plane of every picture, and nothing here proves where its pictures
  /// start.
  Hevc {
    /// The NAL length field's width, from the `hvcC` record.
    nal_length: Option<usize>,
    /// Whether the stream's video parameter set declares an auxiliary
    /// layer ([`vps_declares_auxiliary`]).
    alpha: bool,
  },
  /// MPEG-1 and MPEG-2 video: a keyframe is clean when a group-of-pictures
  /// header before its first picture says the GOP is closed (`closed_gop`):
  /// no B picture after the I picture references the GOP before it. A packet
  /// with no GOP header before its picture proves nothing.
  Mpeg12,
  /// VP8, VP9 and AV1: a keyframe refreshes every reference, and nothing
  /// leads it.
  Resets,
  /// A codec that codes every picture alone — its descriptor carries
  /// `AV_CODEC_PROP_INTRA_ONLY`: ProRes, DNxHD, MJPEG, Ut Video, … No
  /// picture references another, so EVERY packet is a clean random access
  /// point and a resync anchor, whatever its key flag says; the warning a
  /// fallback on one thread gives after a minute never fires for it.
  IntraOnly,
  /// Every other codec: never clean mid-stream. This crate reads none of
  /// its picture headers, and a decoder's `has_b_frames` before a keyframe
  /// proves nothing — FFmpeg raises it when it meets reordering, which an
  /// open GOP can introduce at that very keyframe — so the session returns to
  /// its threads only at a seek, whose first keyframe is a switch point
  /// whatever its kind.
  Reordering,
}

impl KeyframeRule {
  /// The rule for a stream of `codec_id` whose codec parameters carry
  /// `extradata`.
  pub(crate) fn of(codec_id: i32, extradata: &[u8]) -> Self {
    if codec_id == CodecId::H264.raw() {
      // FFmpeg's own test (`h264_decode_extradata`): `avcC` opens with
      // its version byte, 1.
      let nal_length = (extradata.first() == Some(&1) && extradata.len() >= 5)
        .then(|| usize::from(extradata[4] & 3) + 1);
      // An `avcC` record repeats its SPS's profile and constraint bytes in
      // its own header; Annex B extradata carries the SPS units themselves.
      let aso = match nal_length {
        Some(_) => extradata
          .get(1..3)
          .is_some_and(|profile| sps_permits_aso(profile[0], profile[1])),
        None => h264_units_permit_aso(extradata, None),
      };
      Self::H264 { nal_length, aso }
    } else if codec_id == CodecId::HEVC.raw() {
      // FFmpeg's own test (`hevc_decode_extradata`): extradata that does
      // not open with a start code is an `hvcC` record.
      let start_coded = extradata.starts_with(&[0, 0, 1]) || extradata.starts_with(&[0, 0, 0, 1]);
      let nal_length =
        (extradata.len() >= 23 && !start_coded).then(|| usize::from(extradata[21] & 3) + 1);
      // An `hvcC` record carries its parameter sets in arrays of its own;
      // start-coded extradata, as units.
      let alpha = match nal_length {
        Some(_) => hvcc_declares_auxiliary(extradata),
        None => hevc_units_declare_auxiliary(extradata, None),
      };
      Self::Hevc { nal_length, alpha }
    } else if codec_id == CodecId::MPEG2VIDEO.raw()
      || codec_id == ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG1VIDEO as i32
    {
      Self::Mpeg12
    } else if codec_id == CodecId::VP8.raw()
      || codec_id == CodecId::VP9.raw()
      || codec_id == CodecId::AV1.raw()
    {
      Self::Resets
    } else if CodecId::from_raw(codec_id).intra_only() {
      Self::IntraOnly
    } else {
      Self::Reordering
    }
  }

  /// Whether the keyframe `data` is a clean random access point: proved so by
  /// its bitstream, never by the state of the decoder it would replace. Bytes
  /// that do not parse are not clean: nothing is proved about them. EVERY NAL unit is read whole —
  /// every header byte present and valid, a picture's unit carrying
  /// payload past its header — or the access unit is not clean; the first
  /// picture's unit decides, and it must START its picture
  /// ([`h264_slice_starts_picture`], [`hevc_segment_starts_picture`]): a
  /// packet that opens on a later slice of a picture, or whose first
  /// picture's unit is cut before its slice header says so, is not clean. An
  /// H.264 stream whose sequence parameter set permits arbitrary slice order
  /// — the rule's (`aso`), or one the packet carries — has no clean point,
  /// nor does an HEVC stream whose video parameter set declares an auxiliary
  /// layer — the rule's (`alpha`), or one the packet carries.
  ///
  /// The units are walked one at a time ([`NalUnits`]) and never collected,
  /// so the memory a packet costs here does not grow with how many units it
  /// packs: a hostile keyframe of one-byte units costs a walk, not a slice
  /// entry per unit.
  pub(crate) fn is_clean(self, data: &[u8]) -> bool {
    match self {
      Self::H264 { nal_length, aso } => {
        !aso
          && !h264_units_permit_aso(data, nal_length)
          && first_h264_picture(data, nal_length)
            .is_some_and(|(kind, unit)| kind == 5 && h264_slice_starts_picture(unit))
      }
      Self::Hevc { nal_length, alpha } => {
        !alpha
          && !hevc_units_declare_auxiliary(data, nal_length)
          && first_hevc_picture(data, nal_length).is_some_and(|(kind, unit)| {
            (16..=20).contains(&kind) && hevc_segment_starts_picture(unit)
          })
      }
      Self::Mpeg12 => closed_gop(data),
      Self::Resets | Self::IntraOnly => true,
      Self::Reordering => false,
    }
  }

  /// Whether every packet of the stream is a random access point, key flag
  /// or not: a codec that codes every picture alone.
  pub(crate) const fn every_packet(self) -> bool {
    matches!(self, Self::IntraOnly)
  }

  /// Whether this rule reads its codec parameters' extradata: how H.264 and
  /// HEVC pack their NAL units.
  pub(crate) const fn reads_extradata(self) -> bool {
    matches!(self, Self::H264 { .. } | Self::Hevc { .. })
  }

  /// Whether this H.264 rule's stream permits arbitrary slice order (`aso`).
  pub(crate) const fn permits_aso(self) -> bool {
    matches!(self, Self::H264 { aso: true, .. })
  }

  /// This rule, for an H.264 stream, with arbitrary slice order permitted
  /// where `aso` says a sequence parameter set the session read elsewhere
  /// permits it; any other rule as it is.
  pub(crate) const fn permitting_aso(self, aso: bool) -> Self {
    match self {
      Self::H264 {
        nal_length,
        aso: own,
      } => Self::H264 {
        nal_length,
        aso: own || aso,
      },
      other => other,
    }
  }

  /// Whether this HEVC rule's stream declares an auxiliary layer (`alpha`).
  pub(crate) const fn declares_alpha(self) -> bool {
    matches!(self, Self::Hevc { alpha: true, .. })
  }

  /// This rule, for an HEVC stream, with an auxiliary layer declared where
  /// `alpha` says a video parameter set the session read elsewhere — or the
  /// output the decoder serving negotiated — declares one; any other rule as
  /// it is.
  pub(crate) const fn declaring_alpha(self, alpha: bool) -> Self {
    match self {
      Self::Hevc {
        nal_length,
        alpha: own,
      } => Self::Hevc {
        nal_length,
        alpha: own || alpha,
      },
      other => other,
    }
  }

  /// Whether `data`, an HEVC packet read under this rule's packing, carries a
  /// video parameter set that declares an auxiliary layer; `false` under any
  /// other rule.
  pub(crate) fn units_declare_alpha(self, data: &[u8]) -> bool {
    match self {
      Self::Hevc { nal_length, .. } => hevc_units_declare_auxiliary(data, nal_length),
      _ => false,
    }
  }

  /// Whether `data`, an H.264 packet read under this rule's packing, carries
  /// a sequence parameter set that permits arbitrary slice order; `false`
  /// under any other rule.
  pub(crate) fn units_permit_aso(self, data: &[u8]) -> bool {
    match self {
      Self::H264 { nal_length, .. } => h264_units_permit_aso(data, nal_length),
      _ => false,
    }
  }

  /// What the key-flagged packet `data` is as a post-commit resync anchor:
  /// `Some` where the bitstream proves it a random-access point, where the
  /// codec lets this crate read it; `None` otherwise, whatever its flag
  /// says. The anchor is [definitive](Anchor::definitive) where nothing after
  /// it references anything before it ([`Self::is_clean`]).
  ///
  /// - **H.264:** the first picture is an IDR picture (NAL unit 5); or the
  ///   access unit carries, before its first picture, a recovery point SEI
  ///   message (NAL unit 6, payload type 6), which the anchor carries
  ///   ([`RecoveryPoint`]). Either way the first picture's slice header
  ///   parses and starts the picture: `first_mb_in_slice` is 0, `slice_type`
  ///   at most 9 ([`h264_slice_starts_picture`]). An I picture with no such
  ///   message anchors nothing: a slice type describes that slice alone, a
  ///   non-IDR intra picture resets no reference, H.264 lets the pictures
  ///   after it reference what the decoder never saw until the signalled
  ///   recovery point, and FFmpeg's parser flags some such pictures key by
  ///   heuristic. A stream whose sequence parameter set permits arbitrary
  ///   slice order has no anchor ([`Self::is_clean`]).
  /// - **HEVC:** the first base-layer picture's NAL unit is an IRAP picture
  ///   (16–23), a CRA (21) among them, whose first slice segment starts it
  ///   ([`hevc_segment_starts_picture`]): the decoder resyncing kept its
  ///   references, and the reorder bound ([`Proof::ReorderBound`]) covers
  ///   the leading pictures a CRA has. A stream whose video parameter set
  ///   declares an auxiliary layer has no anchor ([`Self::is_clean`]).
  /// - **Every other codec** — one picture per packet: MPEG-4 part 2, VP8,
  ///   VP9, AV1 and the rest — **the key flag FFmpeg's parser set from the
  ///   bitstream is the proof.** That is the trust boundary: this crate
  ///   reads no picture header of theirs.
  ///
  /// Every NAL unit is read whole, as for [`Self::is_clean`], and every SEI
  /// message before the first picture walked by its size over the raw byte
  /// sequence payload; bytes that do not parse anchor nothing.
  pub(crate) fn anchor(self, data: &[u8]) -> Option<Anchor> {
    let anchor = |recovery| Anchor {
      definitive: self.is_clean(data),
      recovery,
    };
    match self {
      Self::H264 { nal_length, aso } => {
        if aso || h264_units_permit_aso(data, nal_length) {
          return None;
        }
        let (kind, unit) = first_h264_picture(data, nal_length)?;
        if !h264_slice_starts_picture(unit) {
          return None;
        }
        match kind {
          5 => Some(anchor(None)),
          1 | 2 => recovery_point(data, nal_length).map(|point| anchor(Some(point))),
          _ => None,
        }
      }
      Self::Hevc { nal_length, alpha } => (!alpha
        && !hevc_units_declare_auxiliary(data, nal_length)
        && first_hevc_picture(data, nal_length).is_some_and(|(kind, unit)| {
          (16..=23).contains(&kind) && hevc_segment_starts_picture(unit)
        }))
      .then(|| anchor(None)),
      Self::Mpeg12 | Self::Resets | Self::IntraOnly | Self::Reordering => Some(anchor(None)),
    }
  }

  /// How a post-commit resync anchored on this rule's stream is proved, where
  /// libavcodec's own decoder for the codec decodes it. Both proofs are
  /// invariants of those decoders: the session proves nothing on an
  /// implementation that wraps another — `h264_cuvid`, `h264_qsv`,
  /// `libdav1d`, … — and refuses a post-commit fallback onto one, and an
  /// H.264 anchor after a decode error across the gap takes the reorder
  /// bound (the session's proof table, `resync_proof`).
  ///
  /// - **H.264: [`Proof::Withheld`].** FFmpeg's H.264 decoder outputs only
  ///   pictures its own recovery tracking has marked recovered — an IDR
  ///   picture and every picture after it in decode order, a recovery
  ///   point's recovery and every picture after it in output order — as long
  ///   as neither `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor
  ///   `AV_CODEC_FLAG2_SHOW_ALL` is set, which the session's software
  ///   decoders refuse at the open. A decoder opened cold across the gap
  ///   starts with nothing recovered, so every picture it delivers is one its
  ///   tracking recovered: the first out after the anchor closes the gap, and
  ///   a picture from before the anchor that comes out after it is one of
  ///   those. That tracking is FFmpeg's, and this crate takes its word, as it
  ///   takes the parser's key flag for the codecs whose pictures it does not
  ///   read.
  /// - **Every other codec: [`Proof::ReorderBound`].** FFmpeg's HEVC decoder
  ///   keeps no such gate for a stream it is already decoding, and this
  ///   crate reads nothing of the other codecs' recovery.
  pub(crate) const fn proof(self) -> Proof {
    match self {
      Self::H264 { .. } => Proof::Withheld,
      Self::Hevc { .. } | Self::Mpeg12 | Self::Resets | Self::IntraOnly | Self::Reordering => {
        Proof::ReorderBound
      }
    }
  }

  /// Why a keyframe this rule reads may not be clean, for the warning a
  /// session gives when a fallback has run on one thread for a minute.
  pub(crate) const fn reason(self) -> &'static str {
    match self {
      Self::H264 { aso: true, .. } => {
        "its sequence parameter set permits arbitrary slice order, so no slice proves it starts its picture"
      }
      Self::H264 { .. } => {
        "its keyframes are recovery points rather than IDR pictures, and pictures after them may reference the GOP before"
      }
      Self::Hevc { alpha: true, .. } => {
        "its video parameter set declares an auxiliary layer, which FFmpeg decodes as an alpha plane beside the base layer, so no base-layer picture proves where the stream can start"
      }
      Self::Hevc { .. } => {
        "its keyframes are CRA pictures, whose RASL leading pictures reference the GOP before them"
      }
      Self::Mpeg12 => {
        "its GOPs are open (no GOP header before the keyframe says closed_gop), so B pictures after a keyframe may reference the GOP before it"
      }
      Self::Resets => "its keyframes reset every reference",
      Self::IntraOnly => "it codes every picture alone, so every packet is a clean point",
      Self::Reordering => {
        "this crate cannot prove its keyframes clean from its bitstream, so the session returns to its threads only at a seek"
      }
    }
  }
}

/// How a post-commit resync is proved once a packet has anchored it
/// ([`KeyframeRule::proof`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proof {
  /// **The decoder withholds every picture it has not recovered**, so the
  /// first picture out after the anchor closes the gap.
  Withheld,
  /// **The reorder bound**: the pictures from before the anchor that can
  /// still come out are the ones its reorder buffer holds, so the gap closes
  /// at the picture out past them — the first for a stream that does not
  /// reorder (VP8, VP9, AV1).
  ReorderBound,
}

/// A post-commit resync anchor, as its bitstream proves it
/// ([`KeyframeRule::anchor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
  definitive: bool,
  recovery: Option<RecoveryPoint>,
}

impl Anchor {
  /// Whether nothing after the anchor references anything before it — a
  /// clean random access point ([`KeyframeRule::is_clean`]): an H.264 IDR
  /// picture, an HEVC IDR or BLA picture, a closed MPEG-1/2 GOP, a VP8, VP9
  /// or AV1 keyframe, any packet of a codec that codes every picture alone.
  /// Such an anchor supersedes one taken across the same gap that is not.
  pub(crate) const fn definitive(self) -> bool {
    self.definitive
  }

  /// The H.264 recovery point the anchor stands on, if any — reported,
  /// never counted: FFmpeg's decoder withholds the pictures before the
  /// recovery it signals itself ([`Proof::Withheld`]).
  pub(crate) const fn recovery(self) -> Option<RecoveryPoint> {
    self.recovery
  }
}

/// An H.264 recovery point SEI message's account of its recovery (H.264
/// D.2.8), read field by field and carried for reporting.
///
/// **What anchoring at one proves**, exact or approximate: the pictures from
/// the anchor on are the pictures a decoder STARTED at this random-access
/// point produces — FFmpeg starts at recovery points whatever the message's
/// flags say — not that they match a decode that ran through the gap bit for
/// bit. An approximate recovery point ([`Self::exact_match`] `false`) says
/// they need not; it anchors all the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryPoint {
  frames: u32,
  exact_match: bool,
  broken_link: bool,
}

impl RecoveryPoint {
  /// `recovery_frame_cnt`: the pictures are recovered from the recovery
  /// point on, that many frames after it in output order.
  pub(crate) const fn frames(self) -> u32 {
    self.frames
  }

  /// `exact_match_flag`: whether the pictures from the recovery on match a
  /// decode that started before the recovery point exactly — `false` for an
  /// approximate recovery, whose pictures need not.
  pub(crate) const fn exact_match(self) -> bool {
    self.exact_match
  }

  /// `broken_link_flag`: whether the pictures at the recovery point may hold
  /// serious visual artefacts from what came before it — the stream was
  /// spliced there. Nothing forward changes for it.
  pub(crate) const fn broken_link(self) -> bool {
    self.broken_link
  }
}

/// The first picture of an H.264 access unit — its NAL unit type (1–5) and
/// its unit — once EVERY unit is read whole and valid; `None` when one is
/// not, or when no picture is there. `forbidden_zero_bit` (1) ·
/// `nal_ref_idc` (2) · `nal_unit_type` (5); a prefix unit and a slice
/// extension carry three more header bytes; a picture's unit carries a
/// slice header past its own; an IDR picture is a reference picture.
fn first_h264_picture(data: &[u8], nal_length: Option<usize>) -> Option<(u8, &[u8])> {
  let mut first = None;
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let &header = unit.first()?;
    if header & 0x80 != 0 {
      return None;
    }
    let kind = header & 0x1f;
    let picture = (1..=5).contains(&kind);
    let header_bytes = if matches!(kind, 14 | 20 | 21) { 4 } else { 1 };
    if unit.len() < header_bytes + usize::from(picture) {
      return None;
    }
    if kind == 5 && header & 0x60 == 0 {
      return None;
    }
    if picture && first.is_none() {
      first = Some((kind, unit));
    }
  }
  first
}

/// The first picture of an HEVC access unit's base layer — its NAL unit
/// type and its unit — once EVERY unit is read whole and valid; `None` when
/// one is not, or when the base layer has no picture there.
/// `forbidden_zero_bit` (1) · `nal_unit_type` (6) · `nuh_layer_id` (6) ·
/// `nuh_temporal_id_plus1` (3), which is never zero; a picture's unit
/// carries a slice segment header past its two header bytes, and an IRAP
/// picture (16–23) a temporal id of 0.
///
/// **The layer FFmpeg decodes.** Its NAL splitter drops every unit of layer
/// 63, whatever the rest of its header says, and its decoder outputs the
/// base layer's pictures (layer 0) unless it is asked for more views
/// (`view_ids`, which this crate never sets): so a unit of layer 63 is
/// skipped here as there, and the first picture is the base layer's. A unit
/// of any other layer is still read whole.
fn first_hevc_picture(data: &[u8], nal_length: Option<usize>) -> Option<(u8, &[u8])> {
  let mut first = None;
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let [head, second, ..] = unit else {
      return None;
    };
    let layer = ((head & 1) << 5) | (second >> 3);
    if layer == 63 {
      continue;
    }
    if head & 0x80 != 0 || second & 0x07 == 0 {
      return None;
    }
    let kind = (head >> 1) & 0x3f;
    if kind < 32 {
      if unit.len() <= 2 || ((16..=23).contains(&kind) && second & 0x07 != 1) {
        return None;
      }
      if layer == 0 {
        first.get_or_insert((kind, unit));
      }
    }
  }
  first
}

/// Whether the H.264 slice whose NAL unit is `unit` starts its picture: its
/// header opens as a slice header must — `first_mb_in_slice`, then
/// `slice_type`, two exp-Golomb codes, the type one of the ten there are
/// (0–9; the upper five say every slice of the picture is that type) — and
/// `first_mb_in_slice` is 0, the slice holding the picture's first
/// macroblock. A later slice of the picture (what opens a packet that holds
/// the rest of a picture split across packets), or a unit cut before its
/// slice type, does not: a decoder started there would never see the slices
/// before it. That macroblock 0 is in the first slice holds where the
/// stream's slices come in order, which every profile but those permitting
/// arbitrary slice order requires ([`sps_permits_aso`]).
fn h264_slice_starts_picture(unit: &[u8]) -> bool {
  let mut bits = RbspBits::new(unit.get(1..).unwrap_or_default());
  bits.ue() == Some(0) && bits.ue().is_some_and(|slice_type| slice_type <= 9)
}

/// Whether an H.264 sequence parameter set with `profile_idc` and the
/// `constraint_set` flags byte `constraint_flags` after it permits arbitrary
/// slice order: the Baseline (66) and Extended (88) profiles allow a
/// picture's slices in any order — the slice holding macroblock 0 may follow
/// another slice of its picture — unless `constraint_set1_flag` holds the
/// stream to the Main profile's constraints, in-order slices among them
/// (Constrained Baseline). Every other profile requires them in order.
const fn sps_permits_aso(profile_idc: u8, constraint_flags: u8) -> bool {
  matches!(profile_idc, 66 | 88) && constraint_flags & 0x40 == 0
}

/// Whether any of `data`'s H.264 NAL units — length-prefixed by
/// `nal_length` bytes, or start-coded when `None` — is a sequence parameter
/// set (7) that permits arbitrary slice order ([`sps_permits_aso`]). Its
/// profile and constraint bytes follow the unit's header byte; the first is
/// never zero where it matters, so no emulation prevention byte stands
/// between them. Units that do not parse prove nothing either way.
fn h264_units_permit_aso(data: &[u8], nal_length: Option<usize>) -> bool {
  NalUnits::new(data, nal_length)
    .filter_map(Result::ok)
    .any(|unit| match unit {
      [header, profile_idc, constraint_flags, ..] => {
        header & 0x1f == 7 && sps_permits_aso(*profile_idc, *constraint_flags)
      }
      _ => false,
    })
}

/// Whether the HEVC slice segment whose NAL unit is `unit` starts its
/// picture: the first bit of its slice segment header,
/// `first_slice_segment_in_pic_flag`, is 1. The header's first byte follows
/// the two NAL header bytes, the second never zero, so no emulation
/// prevention byte can stand there.
fn hevc_segment_starts_picture(unit: &[u8]) -> bool {
  unit.get(2).is_some_and(|byte| byte & 0x80 != 0)
}

/// Whether any of `data`'s HEVC NAL units — length-prefixed by `nal_length`
/// bytes, or start-coded when `None` — is a video parameter set that
/// declares an auxiliary layer ([`vps_declares_auxiliary`]). Units that do
/// not parse prove nothing either way.
fn hevc_units_declare_auxiliary(data: &[u8], nal_length: Option<usize>) -> bool {
  NalUnits::new(data, nal_length)
    .filter_map(Result::ok)
    .any(|unit| hevc_unit_is_vps(unit) && vps_declares_auxiliary(unit))
}

/// Whether an `hvcC` record's parameter set arrays hold a video parameter
/// set that declares an auxiliary layer ([`vps_declares_auxiliary`]): its
/// 23 bytes, `numOfArrays`, then each array's type byte, unit count and
/// units, each a 16-bit length and the unit — read as FFmpeg's
/// `ff_hevc_decode_extradata` reads them, in order, until one does not.
fn hvcc_declares_auxiliary(record: &[u8]) -> bool {
  let mut rest = record.get(23..).unwrap_or_default();
  for _ in 0..record.get(22).copied().unwrap_or(0) {
    let [_, high, low, tail @ ..] = rest else {
      return false;
    };
    rest = tail;
    for _ in 0..u16::from_be_bytes([*high, *low]) {
      let [high, low, tail @ ..] = rest else {
        return false;
      };
      let size = usize::from(u16::from_be_bytes([*high, *low]));
      let (Some(unit), Some(after)) = (tail.get(..size), tail.get(size..)) else {
        return false;
      };
      if hevc_unit_is_vps(unit) && vps_declares_auxiliary(unit) {
        return true;
      }
      rest = after;
    }
  }
  false
}

/// Whether `unit` is an HEVC video parameter set (NAL unit type 32) FFmpeg
/// reads: one of layer 63 its NAL splitter drops.
fn hevc_unit_is_vps(unit: &[u8]) -> bool {
  match unit {
    [head, second, ..] => (head >> 1) & 0x3f == 32 && ((head & 1) << 5) | (second >> 3) != 63,
    _ => false,
  }
}

/// Whether the HEVC video parameter set `unit` — its NAL unit, header and
/// all — declares an auxiliary layer FFmpeg 9 decodes as an alpha plane:
/// [`vps_alpha`] answers yes. Where FFmpeg's answer is not the unit's to give
/// — it reads past the unit's payload before it decides — the set declares
/// none: FFmpeg does not establish an alpha layer from the unit itself.
fn vps_declares_auxiliary(unit: &[u8]) -> bool {
  vps_alpha(unit) == Some(true)
}

/// The auxiliary scalability type's flag in `scalability_mask_flag` (its
/// index 3), whose `AuxId` 1 is alpha (`HEVC_SCALABILITY_AUXILIARY`).
const SCALABILITY_AUXILIARY: u32 = 1 << (15 - 3);

/// The multiview scalability type's flag in `scalability_mask_flag` (its
/// index 1, `HEVC_SCALABILITY_MULTIVIEW`).
const SCALABILITY_MULTIVIEW: u32 = 1 << (15 - 1);

/// **FFmpeg 9's verdict on the HEVC video parameter set `unit`**, read as
/// its decoder reads it: `ff_hevc_decode_nal_vps`, the extension's
/// `decode_vps_ext` (hevc/ps.c), then `ff_hevc_is_alpha_video`
/// (hevc/hevcdec.c), which answers alpha for a set FFmpeg stores with two
/// layers, the second's `nuh_layer_id` not 0, and the auxiliary scalability
/// type set.
///
/// - **`Some(true)`, alpha**: a set of exactly two layers and at most two
///   layer sets whose extension FFmpeg reads to its end, or stops reading at
///   an unsupported feature (`AVERROR_PATCHWELCOME`) once the mask and the
///   second layer's id are read — "Broken VPS extension, treating as alpha
///   video": FFmpeg keeps such a set's two layers where the auxiliary type
///   is set, an auxiliary type other than alpha among them.
/// - **`Some(false)`**: a set of one layer, or with no extension; a set of
///   more than two layers or more than two layer sets, whose extension
///   FFmpeg ignores ("Ignoring unsupported VPS extension") and decodes the
///   base layer alone; an extension stopped before the second layer's id is
///   read; one read whole without the auxiliary type, or with a second
///   layer id of 0.
/// - **`None`**: a set FFmpeg refuses — `AVERROR_INVALIDDATA` anywhere,
///   `AVERROR_PATCHWELCOME` before the extension, the NAL header its
///   splitter drops — which it does not store; or a reading that runs past
///   the unit's payload before FFmpeg decides, the bits its reader goes on
///   to not the unit's ([`RbspBits::payload`]). Either declares none.
///
/// Every field FFmpeg checks is checked here as there; the bounds of its
/// exp-Golomb readers too ([`RbspBits::ue`], [`RbspBits::ue_short`],
/// [`RbspBits::ue_31`]).
fn vps_alpha(unit: &[u8]) -> Option<bool> {
  // The NAL header: a forbidden bit set, or a temporal id of -1, and FFmpeg's
  // splitter skips the unit (`hevc_parse_nal_header`, h2645_parse.c).
  let [head, second, ..] = unit else {
    return None;
  };
  if head & 0x80 != 0 || second & 0x07 == 0 {
    return None;
  }
  let mut bits = RbspBits::payload(unit, 2);
  bits.skip(4)?; // vps_video_parameter_set_id
  // vps_base_layer_internal_flag, vps_base_layer_available_flag: both, or
  // the set is refused.
  if !(bits.next_bit()? & bits.next_bit()?) {
    return None;
  }
  let max_layers = bits.bits(6)? + 1;
  let max_sub_layers = bits.bits(3)? + 1;
  bits.skip(1)?; // vps_temporal_id_nesting_flag
  if bits.bits(16)? != 0xffff || max_sub_layers > 7 {
    return None;
  }
  profile_tier_level(&mut bits, true, max_sub_layers)?;
  let ordering_for_each_sub_layer = bits.next_bit()?;
  let first = if ordering_for_each_sub_layer {
    0
  } else {
    max_sub_layers - 1
  };
  for _ in first..max_sub_layers {
    // vps_max_dec_pic_buffering_minus1, from 0 to 15;
    // vps_max_num_reorder_pics, over its bound only a warning;
    // vps_max_latency_increase_plus1.
    let buffering = bits.ue()?.checked_add(1)?;
    bits.ue()?;
    bits.ue()?;
    if buffering > 16 {
      return None;
    }
  }
  let max_layer_id = bits.bits(6)?;
  let layer_sets = bits.ue()?.checked_add(1)?;
  // layer_id_included_flag[i][j] for every layer set after the first, which
  // the set must hold.
  let included = (u64::from(layer_sets) - 1) * (u64::from(max_layer_id) + 1);
  if layer_sets > 1024 || included > bits.left() as u64 {
    return None;
  }
  let layer1_included = if layer_sets > 1 {
    bits.bits64(max_layer_id + 1)?
  } else {
    0
  };
  if layer_sets > 2 {
    bits.skip((layer_sets as usize - 2) * (max_layer_id as usize + 1))?;
  }
  if bits.next_bit()? {
    // vps_timing_info_present_flag
    bits.skip(64)?; // vps_num_units_in_tick, vps_time_scale
    if bits.next_bit()? {
      bits.ue()?; // vps_num_ticks_poc_diff_one_minus1
    }
    let hrd_parameter_sets = bits.ue()?;
    if hrd_parameter_sets > layer_sets {
      return None;
    }
    for index in 0..hrd_parameter_sets {
      bits.ue()?; // hrd_layer_set_idx
      let common = index == 0 || bits.next_bit()?;
      hrd_parameters(&mut bits, common, max_sub_layers)?;
    }
  }
  // One layer from here, unless the extension says two.
  if max_layers > 1 && bits.next_bit()? {
    // vps_extension_flag
    return vps_extension_alpha(
      &mut bits,
      max_layers,
      max_sub_layers,
      layer_sets,
      layer1_included,
    );
  }
  Some(false)
}

/// [`vps_alpha`]'s reading of the extension, as `decode_vps_ext` reads it
/// (hevc/ps.c), past `vps_extension_flag`, and `ff_hevc_decode_nal_vps`'s
/// reading of its answer: `Some` with whether FFmpeg decodes an alpha layer,
/// `None` where it refuses the set or reads past it.
fn vps_extension_alpha(
  bits: &mut RbspBits<'_>,
  max_layers: u32,
  max_sub_layers: u32,
  layer_sets: u32,
  layer1_included: u64,
) -> Option<bool> {
  // More than two layers, or layer sets: FFmpeg ignores the extension, the
  // set one layer.
  if max_layers > 2 || layer_sets > 2 {
    return Some(false);
  }
  bits.align()?;
  // Two layers from here. The second layer's `nuh_layer_id` is 0 until read
  // (`layer_id_in_nuh[1]`). An unsupported feature met past this point keeps
  // two layers where the mask sets the auxiliary type and that id is not 0
  // ("Broken VPS extension, treating as alpha video"), and one otherwise.
  let mut layer_id = 0u32;
  let unsupported =
    |mask: u32, layer_id: u32| Some(mask & SCALABILITY_AUXILIARY != 0 && layer_id != 0);
  profile_tier_level(bits, false, max_sub_layers)?;
  let splitting = bits.next_bit()?;
  let mask = bits.bits(16)?;
  let types = mask.count_ones() as usize;
  if types == 0 {
    return None;
  }
  if mask & (SCALABILITY_MULTIVIEW | SCALABILITY_AUXILIARY) == 0 {
    return unsupported(mask, layer_id);
  }
  let mut lengths = [0u32; 16];
  for length in lengths.iter_mut().take(types - usize::from(splitting)) {
    *length = bits.bits(3)? + 1; // dimension_id_len_minus1
  }
  layer_id = if bits.next_bit()? {
    // vps_nuh_layer_id_present_flag, then layer_id_in_nuh[1]
    bits.bits(6)?
  } else {
    1
  };
  if !splitting {
    let mut dimensions = [0u32; 16];
    for (dimension, &length) in dimensions.iter_mut().zip(&lengths).take(types) {
      *dimension = bits.bits(length)?;
    }
    // The auxiliary type's `dimension_id`, read where FFmpeg reads it: after
    // the multiview type's, if that is set. `AuxId` 1 is alpha; another is
    // unsupported.
    let index = usize::from(mask & SCALABILITY_MULTIVIEW != 0);
    if mask & SCALABILITY_AUXILIARY != 0 && dimensions[index] != 1 {
      return unsupported(mask, layer_id);
    }
  }
  let view_id_len = bits.bits(4)?;
  if view_id_len != 0 {
    let views = if mask & SCALABILITY_MULTIVIEW != 0 {
      2
    } else {
      1
    };
    bits.skip(views * view_id_len as usize)?;
  }
  let direct_dependency = bits.next_bit()?;
  let mut add_layer_sets = 0;
  if !direct_dependency {
    add_layer_sets = bits.ue_short()?;
    if add_layer_sets > 1 {
      return unsupported(mask, layer_id);
    }
    // highest_layer_idx_plus1
    if add_layer_sets == 1 && !bits.next_bit()? {
      return unsupported(mask, layer_id);
    }
  }
  if layer_sets + add_layer_sets != 2 {
    // num_output_layer_sets
    return unsupported(mask, layer_id);
  }
  let mut sub_layers = [1u32; 2];
  if bits.next_bit()? {
    // vps_sub_layers_max_minus1_present_flag
    for count in &mut sub_layers {
      *count = bits.bits(3)? + 1;
    }
  }
  if bits.next_bit()? {
    // max_tid_ref_present_flag
    bits.skip(3)?;
  }
  bits.skip(1)?; // default_ref_layers_active_flag
  let profiles = bits.ue_short()?.checked_add(1)?;
  for _ in 2..profiles {
    let present = bits.next_bit()?;
    profile_tier_level(bits, present, max_sub_layers)?;
  }
  if bits.ue_short()? != 0 {
    // num_add_olss
    return unsupported(mask, layer_id);
  }
  let default_output_layer_idc = bits.bits(2)?;
  if default_output_layer_idc != 0 {
    return unsupported(mask, layer_id);
  }
  let both = 1u64 | (1u64 << layer_id);
  if layer1_included != 0 && layer1_included != both {
    return unsupported(mask, layer_id);
  }
  let output_layers = if layer1_included == 0 { 1 } else { 2 };
  if layer_sets == 1 {
    bits.skip(1)?;
  }
  if profiles > 1 {
    let width = 32 - (profiles - 1).leading_zeros();
    for _ in 0..output_layers {
      if bits.bits(width)? >= profiles {
        // profile_tier_level_idx
        return None;
      }
    }
  }
  if bits.ue_31()? != 0 {
    // vps_num_rep_formats_minus1
    return unsupported(mask, layer_id);
  }
  let width = bits.bits(16)?;
  let height = bits.bits(16)?;
  if !bits.next_bit()? {
    // chroma_and_bit_depth_vps_present_flag
    return None;
  }
  let chroma_format_idc = bits.bits(2)?;
  if chroma_format_idc == 3 {
    bits.skip(1)?; // separate_colour_plane_flag
  }
  let luma = bits.bits(4)? + 8;
  let chroma = bits.bits(4)? + 8;
  if luma > 16 || chroma > 16 || luma != chroma {
    return unsupported(mask, layer_id);
  }
  if bits.next_bit()? {
    // conformance_window_vps_flag, read as `read_window` reads it: offsets
    // in chroma units that must leave a picture.
    let (horizontal, vertical) = match chroma_format_idc {
      1 => (2u64, 2u64),
      2 => (2, 1),
      _ => (1, 1),
    };
    let left = u64::from(bits.ue()?) * horizontal;
    let right = u64::from(bits.ue()?) * horizontal;
    let top = u64::from(bits.ue()?) * vertical;
    let bottom = u64::from(bits.ue()?) * vertical;
    if u64::from(width) <= left + right || u64::from(height) <= top + bottom {
      return None;
    }
  }
  bits.skip(2)?; // max_one_active_ref_layer_flag, vps_poc_lsb_aligned_flag
  if !direct_dependency {
    bits.skip(1)?; // poc_lsb_not_present_flag
  }
  let sub_layer_flag_info_present = bits.next_bit()?;
  for sub_layer in 0..sub_layers[0].max(sub_layers[1]) {
    let present = sub_layer == 0 || !sub_layer_flag_info_present || bits.next_bit()?;
    if present {
      // max_vps_dec_pic_buffering_minus1 for each output layer, then
      // max_vps_num_reorder_pics and max_vps_latency_increase_plus1.
      for _ in 0..output_layers + 2 {
        bits.ue()?;
      }
    }
  }
  let dependency_type_len = bits.ue_31()? + 2;
  if bits.next_bit()? {
    // direct_dep_type_len_minus2 above; direct_dependency_all_layers_flag
    if bits.bits(dependency_type_len)? > 2 {
      return unsupported(mask, layer_id);
    }
  }
  let non_vui_extension_length = bits.ue_short()?;
  if non_vui_extension_length > 4096 {
    return None;
  }
  bits.skip(non_vui_extension_length as usize * 8)?;
  bits.skip(1)?; // vps_vui_present_flag
  Some(mask & SCALABILITY_AUXILIARY != 0 && layer_id != 0)
}

/// Reads past a `profile_tier_level(profilePresentFlag,
/// maxNumSubLayersMinus1)` as FFmpeg's `parse_ptl` does: the general
/// profile's 88 bits where present, the general level, then each sub-layer's
/// flags, padding, profile and level.
fn profile_tier_level(bits: &mut RbspBits<'_>, profile: bool, max_sub_layers: u32) -> Option<()> {
  if profile {
    bits.skip(88)?;
  }
  bits.skip(8)?; // general_level_idc
  let sub_layers = (max_sub_layers - 1) as usize;
  let mut present = [(false, false); 6];
  for flags in present.iter_mut().take(sub_layers) {
    *flags = (bits.next_bit()?, bits.next_bit()?);
  }
  if sub_layers > 0 {
    bits.skip(2 * (8 - sub_layers))?; // reserved_zero_2bits
  }
  for (profile_present, level_present) in present.into_iter().take(sub_layers) {
    if profile_present {
      bits.skip(88)?;
    }
    if level_present {
      bits.skip(8)?;
    }
  }
  Some(())
}

/// Reads past an `hrd_parameters(commonInfPresentFlag,
/// maxNumSubLayersMinus1)` as FFmpeg's `decode_hrd` does — an absent common
/// part reads as no NAL and no VCL parameters, as there. Where a sub-layer
/// counts more than 32 CPBs, `decode_hrd` stops there, and the video
/// parameter set's reading, which does not look at its answer, goes on from
/// that bit (hevc/ps.c): so does this.
fn hrd_parameters(bits: &mut RbspBits<'_>, common: bool, max_sub_layers: u32) -> Option<()> {
  let (mut nal, mut vcl, mut sub_picture) = (false, false, false);
  if common {
    nal = bits.next_bit()?;
    vcl = bits.next_bit()?;
    if nal || vcl {
      sub_picture = bits.next_bit()?;
      if sub_picture {
        // tick_divisor_minus2, du_cpb_removal_delay_increment_length_minus1,
        // sub_pic_cpb_params_in_pic_timing_sei_flag,
        // dpb_output_delay_du_length_minus1
        bits.skip(8 + 5 + 1 + 5)?;
      }
      bits.skip(4 + 4)?; // bit_rate_scale, cpb_size_scale
      if sub_picture {
        bits.skip(4)?; // cpb_size_du_scale
      }
      // initial_cpb_removal_delay_length_minus1,
      // au_cpb_removal_delay_length_minus1, dpb_output_delay_length_minus1
      bits.skip(5 + 5 + 5)?;
    }
  }
  for _ in 0..max_sub_layers {
    let fixed_general = bits.next_bit()?;
    let fixed_within = !fixed_general && bits.next_bit()?;
    let mut low_delay = false;
    if fixed_general || fixed_within {
      bits.ue()?; // elemental_duration_in_tc_minus1
    } else {
      low_delay = bits.next_bit()?;
    }
    let mut cpbs = 1;
    if !low_delay {
      let minus1 = bits.ue()?;
      if minus1 > 31 {
        return Some(());
      }
      cpbs = minus1 + 1;
    }
    // sub_layer_hrd_parameters, for the NAL and the VCL parameters present.
    for _ in 0..(u32::from(nal) + u32::from(vcl)) * cpbs {
      bits.ue()?; // bit_rate_value_minus1
      bits.ue()?; // cpb_size_value_minus1
      if sub_picture {
        bits.ue()?; // cpb_size_du_value_minus1
        bits.ue()?; // bit_rate_du_value_minus1
      }
      bits.skip(1)?; // cbr_flag
    }
  }
  Some(())
}

/// The recovery point SEI message an H.264 access unit carries before its
/// first picture; `None` where none does, or where an SEI unit before it
/// does not parse. The units are read as [`first_h264_picture`] reads them,
/// one at a time.
fn recovery_point(data: &[u8], nal_length: Option<usize>) -> Option<RecoveryPoint> {
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let kind = unit.first()? & 0x1f;
    if (1..=5).contains(&kind) {
      return None;
    }
    if kind == 6
      && let Some(frames) = sei_recovery_point(unit.get(1..)?)?
    {
      return Some(frames);
    }
  }
  None
}

/// The SEI payload type of a recovery point message (H.264 D.1.8).
const RECOVERY_POINT: u32 = 6;

/// The recovery point an SEI unit's raw byte sequence payload `rbsp` states:
/// `Some(Some(point))` for its first recovery point message, `Some(None)`
/// where it has none, `None` where its messages do not parse.
/// Every `sei_message` is walked whole by its `payload_size` — its type and
/// size each a run of `FF` bytes and a last byte — over the payload with its
/// emulation prevention bytes removed, up to the `rbsp_trailing_bits`; a
/// payload that runs past the unit does not parse. A recovery point's
/// `recovery_frame_cnt` (`ue(v)`), `exact_match_flag`, `broken_link_flag`
/// and `changing_slice_group_idc` (two bits) lie within its payload.
fn sei_recovery_point(rbsp: &[u8]) -> Option<Option<RecoveryPoint>> {
  let mut bits = RbspBits::new(rbsp);
  let mut found = None;
  while !bits.at_trailing_bits() {
    let payload_type = bits.sei_value()?;
    let payload_size = bits.sei_value()?;
    let end = bits
      .read()
      .checked_add(usize::try_from(payload_size).ok()?.checked_mul(8)?)?;
    if payload_type == RECOVERY_POINT && found.is_none() {
      let frames = bits.ue()?;
      let exact_match = bits.next_bit()?;
      let broken_link = bits.next_bit()?;
      // `changing_slice_group_idc`, two bits.
      bits.next_bit()?;
      bits.next_bit()?;
      if bits.read() > end {
        return None;
      }
      found = Some(RecoveryPoint {
        frames,
        exact_match,
        broken_link,
      });
    }
    while bits.read() < end {
      bits.next_bit()?;
    }
  }
  Some(found)
}

/// The bits of an H.264 or HEVC raw byte sequence payload, most
/// significant first, its emulation prevention bytes — the `03` of a
/// `00 00 03` — skipped.
struct RbspBits<'a> {
  bytes: &'a [u8],
  at: usize,
  bit: u8,
  zeros: usize,
  /// The payload bits read so far, emulation prevention bytes not among
  /// them.
  read: usize,
  /// The payload bits there are to read; past them a read answers `None`.
  limit: usize,
}

impl<'a> RbspBits<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      at: 0,
      bit: 0,
      zeros: 0,
      read: 0,
      limit: usize::MAX,
    }
  }

  /// **The payload of the NAL unit `unit` past its `header` bytes, ending
  /// where FFmpeg's NAL splitter ends it**: its emulation prevention bytes
  /// removed, its trailing zero bytes stripped, then the last byte's
  /// `rbsp_stop_one_bit` and the zero bits after it — `get_bit_length`
  /// (h2645_parse.c), where an HEVC unit's two header bytes are the least it
  /// holds. Its bits are the ones FFmpeg's readers count as left
  /// (`get_bits_left`); a read past them answers `None`, the bits FFmpeg's
  /// reader goes on to — whatever follows the unit in memory, its safe
  /// reader clamping its index a byte past the payload (get_bits.h) — not
  /// the unit's.
  fn payload(unit: &'a [u8], header: usize) -> Self {
    let (mut kept, mut last, mut zeros, mut end) = (0usize, 0u8, 0usize, 0usize);
    for &byte in unit {
      if zeros >= 2 && byte == 3 {
        zeros = 0;
        continue;
      }
      kept += 1;
      zeros = if byte == 0 { zeros + 1 } else { 0 };
      if byte != 0 {
        (end, last) = (kept, byte);
      }
    }
    let size_bits = if end <= header {
      8 * header
    } else {
      8 * end - (last.trailing_zeros() as usize + 1)
    };
    let mut bits = Self::new(unit.get(header..).unwrap_or_default());
    bits.limit = size_bits.saturating_sub(8 * header);
    bits
  }

  /// The payload bits read so far.
  const fn read(&self) -> usize {
    self.read
  }

  /// The payload bits left to read (`get_bits_left`).
  const fn left(&self) -> usize {
    self.limit.saturating_sub(self.read)
  }

  /// Whether only the `rbsp_trailing_bits` are left, at a byte boundary:
  /// the stop bit's `80`, or nothing — a unit never ends in a zero byte.
  fn at_trailing_bits(&self) -> bool {
    self.bit == 0 && matches!(self.bytes.get(self.at..), Some([] | [0x80]) | None)
  }

  /// An SEI message's payload type or size: a run of `FF` bytes, each 255,
  /// and a last byte.
  fn sei_value(&mut self) -> Option<u32> {
    let mut value = 0u32;
    loop {
      let mut byte = 0u32;
      for _ in 0..8 {
        byte = (byte << 1) | u32::from(self.next_bit()?);
      }
      value = value.checked_add(byte)?;
      if byte != 0xff {
        return Some(value);
      }
    }
  }

  fn next_bit(&mut self) -> Option<bool> {
    if self.read >= self.limit {
      return None;
    }
    if self.bit == 0 {
      // A new byte: an emulation prevention byte is not payload.
      if self.zeros >= 2 && self.bytes.get(self.at) == Some(&3) {
        self.at += 1;
        self.zeros = 0;
      }
      let &byte = self.bytes.get(self.at)?;
      self.zeros = if byte == 0 { self.zeros + 1 } else { 0 };
    }
    let byte = *self.bytes.get(self.at)?;
    let value = byte & (0x80 >> self.bit) != 0;
    self.read += 1;
    self.bit += 1;
    if self.bit == 8 {
      self.bit = 0;
      self.at += 1;
    }
    Some(value)
  }

  /// `u(n)`: `n` bits, the first the most significant; `n` at most 32.
  fn bits(&mut self, n: u32) -> Option<u32> {
    let mut value = 0u32;
    for _ in 0..n {
      value = (value << 1) | u32::from(self.next_bit()?);
    }
    Some(value)
  }

  /// `u(n)` for `n` up to 64 (`get_bits64`).
  fn bits64(&mut self, n: u32) -> Option<u64> {
    let mut value = 0u64;
    for _ in 0..n {
      value = (value << 1) | u64::from(self.next_bit()?);
    }
    Some(value)
  }

  /// Reads past `n` bits.
  fn skip(&mut self, n: usize) -> Option<()> {
    for _ in 0..n {
      self.next_bit()?;
    }
    Some(())
  }

  /// Reads past the bits left before the payload's next byte boundary — the
  /// raw bytes' too, since an emulation prevention byte is a whole byte.
  fn align(&mut self) -> Option<()> {
    while self.bit != 0 {
      self.next_bit()?;
    }
    Some(())
  }

  /// An unsigned exp-Golomb code, `ue(v)`: `n` zeros, a one, `n` more bits.
  fn ue(&mut self) -> Option<u32> {
    let mut zeros = 0u32;
    while !self.next_bit()? {
      zeros += 1;
      if zeros > 31 {
        return None;
      }
    }
    let mut value = 0u32;
    for _ in 0..zeros {
      value = (value << 1) | u32::from(self.next_bit()?);
    }
    Some((1u32 << zeros) - 1 + value)
  }

  /// `ue(v)` as FFmpeg's `get_ue_golomb` reads it, which answers codes of at
  /// most 12 leading zeros — values to 8190 — and an error for longer ones
  /// (golomb.h): `None` past that, where its reading is not the code's.
  fn ue_short(&mut self) -> Option<u32> {
    self.ue().filter(|&value| value <= 8190)
  }

  /// `ue(v)` as FFmpeg's `get_ue_golomb_31` reads it, a table of the codes
  /// of at most nine bits — values to 30 (golomb.h): `None` past that.
  fn ue_31(&mut self) -> Option<u32> {
    self.ue().filter(|&value| value <= 30)
  }
}

/// Whether an MPEG-1 or MPEG-2 video packet carries, before its first
/// picture (start code `00`), a group-of-pictures header (`B8`) whose
/// `closed_gop` flag is set: `time_code` is the header's first 25 bits, and
/// `closed_gop` the next. Start codes are walked one at a time, in constant
/// memory.
fn closed_gop(data: &[u8]) -> bool {
  let mut closed = false;
  let mut at = 0usize;
  while let Some((_, value)) = start_code(data, at) {
    match data.get(value) {
      Some(0x00) => return closed,
      Some(0xb8) => {
        // The start code's value, then `time_code`'s 25 bits: `closed_gop`
        // is the second bit of the fourth byte after the value.
        let Some(&flags) = data.get(value + 4) else {
          return false;
        };
        closed = flags & 0x40 != 0;
      }
      _ => {}
    }
    at = value;
  }
  false
}

/// A unit that does not parse whole: the walk ends there, and the access
/// unit is not clean.
struct Malformed;

/// **Every NAL unit in `data`, whole, one at a time**: length-prefixed by
/// `nal_length` bytes, or start-coded (Annex B) when `None`. Each unit is
/// found as the walk reaches it and nothing is collected, so a walk takes
/// the same memory for one unit as for a billion.
///
/// The walk answers [`Malformed`] and ends when a length-prefixed unit runs
/// past the end or is empty, when anything but zeros precedes the first
/// start code, or when a start-coded unit is empty once its
/// `trailing_zero_8bits` are stripped — a start code ending the data among
/// them. Data with no start code at all, zeros only, has no unit.
///
/// A four-byte start code (`00 00 00 01`) is read whole before a three-byte
/// one, so its leading zero is never left on the unit before it; and the
/// zero bytes Annex B allows after a unit (`trailing_zero_8bits`) are
/// stripped from it — a NAL unit never ends in a zero byte, since one whose
/// data would is given a final `03`.
struct NalUnits<'a> {
  data: &'a [u8],
  /// The NAL length field's width, or `None` for start codes.
  nal_length: Option<usize>,
  walk: Walk,
}

/// Where a walk over NAL units stands.
#[derive(Clone, Copy)]
enum Walk {
  /// The next unit begins here: at its length field, or after its start
  /// code.
  At(usize),
  /// The next item is [`Malformed`], and the walk ends with it.
  Malformed,
  /// The walk is over.
  Done,
}

impl<'a> NalUnits<'a> {
  fn new(data: &'a [u8], nal_length: Option<usize>) -> Self {
    let walk = if nal_length.is_some() {
      Walk::At(0)
    } else {
      // The first unit begins after the first start code, and only zeros
      // (`leading_zero_8bits`) may come before it.
      let first = start_code(data, 0);
      let leading = first.map_or(data.len(), |(code, _)| code);
      if data[..leading].iter().any(|&byte| byte != 0) {
        Walk::Malformed
      } else {
        first.map_or(Walk::Done, |(_, unit)| Walk::At(unit))
      }
    };
    Self {
      data,
      nal_length,
      walk,
    }
  }
}

impl<'a> Iterator for NalUnits<'a> {
  type Item = Result<&'a [u8], Malformed>;

  fn next(&mut self) -> Option<Self::Item> {
    let at = match self.walk {
      Walk::At(at) => at,
      Walk::Malformed => {
        self.walk = Walk::Done;
        return Some(Err(Malformed));
      }
      Walk::Done => return None,
    };
    let data = self.data;
    let unit = match self.nal_length {
      Some(width) => {
        if at == data.len() {
          self.walk = Walk::Done;
          return None;
        }
        // The length field, then that many bytes.
        let start = at.checked_add(width);
        let length = start.and_then(|start| data.get(at..start)).map(|field| {
          field
            .iter()
            .fold(0usize, |length, &byte| (length << 8) | usize::from(byte))
        });
        match (start, length) {
          (Some(start), Some(length)) => start.checked_add(length).and_then(|end| {
            self.walk = Walk::At(end);
            data.get(start..end)
          }),
          _ => None,
        }
      }
      None => {
        // To the next start code, or to the end; the zeros that trail the
        // unit are not its own.
        let end = match start_code(data, at) {
          Some((code, next)) => {
            self.walk = Walk::At(next);
            code
          }
          None => {
            self.walk = Walk::Done;
            data.len()
          }
        };
        let mut unit = &data[at..end];
        while let [rest @ .., 0] = unit {
          unit = rest;
        }
        Some(unit)
      }
    };
    match unit {
      Some(unit) if !unit.is_empty() => Some(Ok(unit)),
      _ => {
        self.walk = Walk::Done;
        Some(Err(Malformed))
      }
    }
  }
}

/// The first start code at or after `from`: where it begins and where its
/// unit does, a four-byte prefix read before a three-byte one.
fn start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
  let mut at = from;
  while at + 3 <= data.len() {
    if data[at..].starts_with(&[0, 0, 0, 1]) {
      return Some((at, at + 4));
    }
    if data[at..].starts_with(&[0, 0, 1]) {
      return Some((at, at + 3));
    }
    at += 1;
  }
  None
}
