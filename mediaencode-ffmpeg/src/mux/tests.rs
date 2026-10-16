use core::num::NonZeroI32;

use mediaio_ffmpeg::FfmpegBytes;

use super::*;

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
}

/// **MP4 spells a language in exactly three of its letters**; anything
/// else is reported, and Matroska and WebM write any tag as given.
#[test]
fn mp4_carries_a_three_letter_language_and_reports_any_other() {
  for carried in ["eng", "jpn", "und", "ger"] {
    assert!(language_carried(Family::Mp4, carried), "{carried}");
  }
  for dropped in ["en", "en-US", "engl", "ENG", "", "e1g"] {
    assert!(!language_carried(Family::Mp4, dropped), "{dropped:?}");
  }
  for family in [Family::Matroska, Family::WebM, Family::Other] {
    assert!(language_carried(family, "en-US"));
  }
}

/// **Each container drops the flags it has no field for on a timed
/// stream, and only those.**
#[test]
fn each_container_reports_the_disposition_flags_it_drops() {
  use TrackDisposition as D;
  let every = D::DEFAULT | D::FORCED | D::COMMENT | D::KARAOKE | D::ORIGINAL;
  let timed = |family, codec, flags| disposition_dropped(family, Road::Timed, codec, flags);
  assert_eq!(timed(Family::Matroska, CodecId::H264, every), D::KARAOKE);
  // Original and dub at once: Matroska writes neither.
  assert_eq!(
    timed(Family::Matroska, CodecId::AAC, D::ORIGINAL | D::DUB),
    D::ORIGINAL | D::DUB
  );
  assert_eq!(
    timed(Family::WebM, CodecId::VP9, every),
    D::COMMENT | D::KARAOKE | D::ORIGINAL
  );
  // A WebVTT track's first kind travels in its codec id.
  assert_eq!(
    timed(Family::WebM, CodecId::WEBVTT, D::CAPTIONS | D::METADATA),
    D::METADATA
  );
  assert_eq!(
    timed(Family::Mp4, CodecId::H264, every),
    D::KARAOKE | D::ORIGINAL
  );
  assert_eq!(
    timed(Family::Other, CodecId::H264, every),
    D::empty(),
    "nothing is claimed for a container this release does not know",
  );
}

/// **An attachment's road keeps fewer fields than a timed stream.** An
/// attachment Matroska writes has no track entry, so every flag is
/// dropped; MP4's cover keeps its attached-picture flag alone.
#[test]
fn an_attachment_road_drops_the_flags_it_has_no_track_for() {
  use TrackDisposition as D;
  assert_eq!(
    disposition_dropped(
      Family::Matroska,
      Road::Attachment,
      CodecId::TTF,
      D::DEFAULT | D::FORCED
    ),
    D::DEFAULT | D::FORCED
  );
  assert_eq!(
    disposition_dropped(
      Family::Mp4,
      Road::Cover,
      CodecId::PNG,
      D::ATTACHED_PIC | D::DEFAULT | D::COMMENT
    ),
    D::DEFAULT | D::COMMENT
  );
  assert_eq!(
    disposition_dropped(Family::Mp4, Road::Cover, CodecId::PNG, D::ATTACHED_PIC),
    D::empty()
  );
}

/// **Matroska labels an attachment with its codec's MIME type, or
/// `text/plain` for text, and nothing else.**
#[test]
fn matroska_names_an_attachments_mime_type_from_its_codec() {
  assert_eq!(
    matroska_mime_type(CodecId::TTF),
    Some("application/x-truetype-font")
  );
  assert_eq!(matroska_mime_type(CodecId::PNG), Some("image/png"));
  assert_eq!(matroska_mime_type(CodecId::TEXT), Some("text/plain"));
  assert_eq!(matroska_mime_type(CodecId::NONE), None);
  assert!(is_cover_codec(CodecId::PNG) && is_cover_codec(CodecId::MJPEG));
  assert!(!is_cover_codec(CodecId::TTF));
}

/// **A chapter's span is counted in its own time base, and one that
/// starts before zero or runs backwards is refused by name.**
#[test]
fn a_chapter_span_is_counted_in_its_own_ruler_and_judged() {
  let nanos = Timebase::new(1, NonZeroI32::new(1_000_000_000).expect("non-zero"));
  let chapter = Chapter::<Ffmpeg>::new(
    1,
    nanos,
    Timestamp::new(1500, millis()),
    Timestamp::new(3000, millis()),
  );
  assert_eq!(
    chapter_span(0, &chapter).expect("a span"),
    (1_500_000_000, 3_000_000_000)
  );
  let backwards = Chapter::<Ffmpeg>::new(
    2,
    millis(),
    Timestamp::new(30, millis()),
    Timestamp::new(10, millis()),
  );
  assert!(matches!(
    chapter_span(4, &backwards),
    Err(MuxOpenError::ChapterSpan { chapter: 4 })
  ));
  let early = Chapter::<Ffmpeg>::new(
    3,
    millis(),
    Timestamp::new(-1, millis()),
    Timestamp::new(10, millis()),
  );
  assert!(matches!(
    chapter_span(5, &early),
    Err(MuxOpenError::ChapterSpan { chapter: 5 })
  ));
  let titled = chapter.with_title(Some(Utf8Bytes::from("a\0b")));
  assert!(matches!(
    chapter_span(6, &titled),
    Err(MuxOpenError::ChapterTitleWithNul { chapter: 6 })
  ));
}

/// **An ADTS header is its first twelve bits set**, in a packet of more
/// than two bytes, as MP4 and Matroska read one.
#[test]
fn an_adts_header_is_read_as_mp4_and_matroska_read_one() {
  assert!(adts_header(&[0xFF, 0xF1, 0x50]));
  assert!(adts_header(&[0xFF, 0xF9, 0x00, 0x00]));
  assert!(!adts_header(&[0xFF, 0xF1]), "two bytes are not enough");
  assert!(!adts_header(&[0x21, 0x10, 0x05]), "raw AAC");
  assert!(!adts_header(&[0xFF, 0xE1, 0x00]));
}

/// **OBUs parse as libavcodec's `parse_obu_header` parses them**: a
/// header byte with its forbidden bit clear, an extension byte where
/// flagged, a LEB128 size where flagged — else the rest of the packet —
/// and a payload inside the packet, OBU after OBU.
#[test]
fn obus_parse_as_libavcodec_parses_them() {
  // A temporal delimiter (type 2, sized, 0 bytes), then a frame OBU
  // (type 6, sized, 3 bytes).
  assert!(obus_parse(&[0x12, 0x00, 0x32, 0x03, 1, 2, 3]));
  // An OBU without a size runs to the packet's end.
  assert!(obus_parse(&[0x30, 1, 2, 3, 4]));
  // With an extension byte.
  assert!(obus_parse(&[0x36, 0x08, 0x02, 9, 9]));
  // A two-byte LEB128 size: 128.
  let mut long = vec![0x32, 0x80, 0x01];
  long.extend([7u8; 128]);
  assert!(obus_parse(&long));
  assert!(obus_parse(&[]), "nothing to parse");
  // The forbidden bit, a size past the packet, a header cut short, a
  // LEB128 running past the packet.
  assert!(!obus_parse(&[0x92, 0x00]));
  assert!(!obus_parse(&[0x32, 0x05, 1, 2]));
  assert!(!obus_parse(&[0x36]));
  assert!(!obus_parse(&[0x32, 0x80, 0x80]));
  assert!(!obus_parse(&[0x36, 0x00, 0x80, 0x80]));
  // A LEB128 is read for eight bytes at most (`leb.h` 57–68): eight with
  // their continuation bits set end it, a size of nothing, and the ten
  // bytes are a whole header.
  assert!(obus_parse(&[
    0x36, 0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00
  ]));
}

/// A WavPack block of `payload` bytes after its 32-byte header.
fn wavpack_block(payload: usize) -> Vec<u8> {
  let mut block = b"wvpk".to_vec();
  block.extend(u32::try_from(payload + 24).expect("small").to_le_bytes());
  block.extend([0u8; 24]);
  block.extend(vec![5u8; payload]);
  block
}

/// **WavPack blocks parse as Matroska's reformatter parses them, whole**:
/// each tagged `wvpk`, its size in 24..=2^20, and nothing left over.
#[test]
fn wavpack_blocks_parse_whole() {
  let mut two = wavpack_block(10);
  two.extend(wavpack_block(0));
  assert!(wavpack_parse(&two));
  assert!(wavpack_parse(&[]));
  let mut over = wavpack_block(10);
  over.extend([1, 2, 3]);
  assert!(
    !wavpack_parse(&over),
    "bytes left over after the last block"
  );
  let mut short = wavpack_block(10);
  short.truncate(40);
  assert!(!wavpack_parse(&short), "a block cut short");
  let mut tag = wavpack_block(4);
  tag[0] = b'W';
  assert!(!wavpack_parse(&tag));
  let mut size = wavpack_block(4);
  size[4..8].copy_from_slice(&23u32.to_le_bytes());
  assert!(!wavpack_parse(&size), "a size below the header's");
}

/// **Skip-samples side data shortens an MP4 sample as MP4 shortens it**:
/// the longer of a frame and the packet's duration, less the samples to
/// drop from its end, each rounded to the nearest tick.
#[test]
fn skip_samples_shorten_an_mp4_sample_as_mp4_does() {
  let side = |end: u32| {
    let mut bytes = 0u32.to_le_bytes().to_vec();
    bytes.extend(end.to_le_bytes());
    bytes.extend([0, 0]);
    vec![SideDataEntry::new(
      AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
      FfmpegBytes::try_copy_from_slice(&bytes).expect("a copy"),
    )]
  };
  // AAC at 48 kHz, in milliseconds: a frame is 21 ms.
  let aac = (1024, 48_000);
  assert!(!skips_below_nothing(&side(1024), 0, aac, millis()));
  assert!(skips_below_nothing(&side(2048), 0, aac, millis()));
  assert!(!skips_below_nothing(&side(2048), 43, aac, millis()));
  assert!(!skips_below_nothing(&[], 0, aac, millis()), "no side data");
  let short = vec![SideDataEntry::new(
    AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32,
    FfmpegBytes::try_copy_from_slice(&[0; 9]).expect("a copy"),
  )];
  assert!(
    !skips_below_nothing(&short, 0, aac, millis()),
    "under 10 bytes"
  );
}

/// **Rescaling rounds to the nearest, half away from zero**, as
/// `av_rescale_q` does.
#[test]
fn a_rescale_rounds_as_av_rescale_q_does() {
  // 1024 samples at 48 kHz: 21.33 ms.
  assert_eq!(rescale_nearest(1024, 1000, 48_000), 21);
  // 312 samples at 48 kHz: 6.5 ms, half away from zero.
  assert_eq!(rescale_nearest(312, 1000, 48_000), 7);
  assert_eq!(rescale_nearest(0, 1000, 48_000), 0);
}

/// **MP4 configures from the extradata a stream will hold**: H.264 with
/// none is refused, with an `avcC` written as offered, with an Annex B
/// configuration rewritten as length prefixes — in Matroska too, which
/// reads Annex B by its start code.
#[test]
fn a_configuration_is_judged_and_its_rewrite_reported() {
  let rewrites_of = |family, kind, codec, extradata: &[u8]| {
    rewrites(family, kind, codec, extradata, true).collect::<Vec<_>>()
  };
  assert!(configuration_missing(Family::Mp4, CodecId::H264, &[]));
  assert!(configuration_missing(Family::Mp4, CodecId::HEVC, &[]));
  assert!(!configuration_missing(Family::Mp4, CodecId::H264, &[1, 2]));
  assert!(configuration_missing(Family::Matroska, CodecId::AV1, &[]));
  assert!(configuration_missing(Family::WebM, CodecId::AV1, &[]));
  assert!(configuration_missing(Family::Matroska, CodecId::AAC, &[]));
  assert!(!configuration_missing(Family::Other, CodecId::H264, &[]));
  let avcc = [1, 0x64, 0, 0x1F];
  let annex = [0, 0, 0, 1, 0x67];
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, &avcc),
    vec![]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, &annex),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Video, CodecId::H264, &annex),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Video, CodecId::H264, &avcc),
    vec![]
  );
  // HEVC needs more than six bytes.
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::HEVC, &annex),
    vec![]
  );
  assert_eq!(
    rewrites_of(
      Family::Mp4,
      TrackKind::Video,
      CodecId::HEVC,
      &[0, 0, 1, 0x40, 1, 2, 3]
    ),
    vec![Rewrite::LengthPrefixed]
  );
  assert_eq!(
    rewrites_of(Family::WebM, TrackKind::Video, CodecId::AV1, &[0x81]),
    vec![Rewrite::ObusFiltered]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Subtitle, CodecId::MOV_TEXT, &[]),
    vec![Rewrite::CueEnds]
  );
  assert_eq!(
    rewrites_of(
      Family::Mp4,
      TrackKind::Subtitle,
      CodecId::TTML,
      b"lavc-ttmlenc"
    ),
    vec![Rewrite::ParagraphsJoined, Rewrite::Durations]
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Audio, CodecId::EAC3, &[]),
    vec![Rewrite::Ac3Parsed]
  );
  assert_eq!(
    rewrites_of(Family::Matroska, TrackKind::Audio, CodecId::WAVPACK, &[]),
    vec![Rewrite::WavPackHeaders]
  );
  assert_eq!(
    rewrites_of(Family::Other, TrackKind::Video, CodecId::H264, &annex),
    vec![],
    "nothing is claimed for a container this release does not know",
  );
}

/// **The durations a container does not keep are reported**: MP4's of a
/// subtitle or data track other than `mov_text`, and Matroska's and
/// WebM's of a video or audio track with no default duration; APV's size
/// prefix is not, as MP4's reader takes it back off.
#[test]
fn a_duration_the_container_does_not_keep_is_reported() {
  let rewrites_of = |family, kind, codec, default_duration| {
    rewrites(family, kind, codec, &[], default_duration).collect::<Vec<_>>()
  };
  for family in [Family::Matroska, Family::WebM] {
    assert_eq!(
      rewrites_of(family, TrackKind::Video, CodecId::VP9, false),
      vec![Rewrite::Durations],
      "{family:?}"
    );
    assert_eq!(
      rewrites_of(family, TrackKind::Audio, CodecId::OPUS, true),
      vec![],
      "{family:?}"
    );
  }
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::H264, false),
    vec![],
    "MP4 keeps a video packet's duration as the distance to the next"
  );
  assert_eq!(
    rewrites_of(Family::Mp4, TrackKind::Video, CodecId::APV, true),
    vec![]
  );
  assert!(configuration_missing(Family::Mp4, CodecId::EVC, &[]));
  assert!(!configuration_missing(Family::Mp4, CodecId::VP9, &[]));
}

/// A ProRes frame of `len` bytes, opening on its atom.
fn prores_frame(len: usize) -> Vec<u8> {
  let mut frame = u32::try_from(len).expect("small").to_be_bytes().to_vec();
  frame.extend(b"icpf");
  frame.resize(len, 7);
  frame
}

/// **A ProRes packet reads back from Matroska only where it opens on its
/// own atom**: the packet's size and `icpf`, with no second atom after it.
#[test]
fn a_prores_packet_reads_back_from_matroska_only_on_its_own_atom() {
  assert!(prores_atom(&prores_frame(64)));
  assert!(prores_atom(&prores_frame(8)));
  let mut wrong_size = prores_frame(64);
  wrong_size[3] = 63;
  assert!(!prores_atom(&wrong_size));
  let mut wrong_tag = prores_frame(64);
  wrong_tag[4] = b'x';
  assert!(!prores_atom(&wrong_tag));
  assert!(!prores_atom(&prores_frame(64)[..7]), "shorter than an atom");
  let mut twice = prores_frame(64);
  twice[12..16].copy_from_slice(b"icpf");
  assert!(!prores_atom(&twice), "a second atom after the first");
}

/// **A WebVTT cue reads back from Matroska's lines only where none of its
/// parts breaks a line it should not**: an identifier and settings each a
/// line, not empty, and a text that does not end in a line break.
#[test]
fn a_webvtt_cue_reads_back_from_its_lines_as_itself() {
  let entry = |kind: AVPacketSideDataType, text: &[u8]| {
    SideDataEntry::new(
      kind as i32,
      FfmpegBytes::try_copy_from_slice(text).expect("a copy"),
    )
  };
  let id = |text: &[u8]| entry(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_IDENTIFIER, text);
  let settings = |text: &[u8]| entry(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_SETTINGS, text);
  assert!(webvtt_cue(b"hello", &[]));
  assert!(webvtt_cue(b"two\nlines", &[id(b"c1"), settings(b"line:0")]));
  assert!(webvtt_cue(b"", &[]), "an empty cue");
  assert!(
    !webvtt_cue(b"hello\n", &[]),
    "a text ending in a line break"
  );
  assert!(!webvtt_cue(b"hello\r", &[]));
  assert!(!webvtt_cue(b"hello", &[id(b"c\n1")]));
  assert!(!webvtt_cue(b"hello", &[settings(b"a\rb")]));
  assert!(
    !webvtt_cue(b"hello", &[id(b"")]),
    "an empty identifier given"
  );
}

/// An AC-3 frame header: bit stream id 8, 48 kHz, frame size code 14 —
/// 448 bytes.
const AC3: [u8; 8] = [0x0B, 0x77, 0, 0, 0x0E, 0x40, 0, 0];

/// An E-AC-3 frame header of `blocks_code` and `kind` — 0 independent, 1
/// dependent — substream 0, 768 bytes, stereo, bit stream id 16.
fn eac3(kind: u8, blocks_code: u8) -> Vec<u8> {
  vec![
    0x0B,
    0x77,
    (kind << 6) | 0x01,
    0x7F,
    (blocks_code << 4) | 0x04,
    0x80,
    0,
    0,
  ]
}

/// `header` padded to the frame size it states: `size` bytes.
fn frame(header: &[u8], size: usize) -> Vec<u8> {
  let mut frame = header.to_vec();
  frame.resize(size, 0);
  frame
}

/// **AC-3 and E-AC-3 headers parse as libavcodec's parser parses them**:
/// its sync word and bit stream id, AC-3's sample rate and frame size
/// codes, E-AC-3's frame type, size, sample rates, substream and a
/// dependent frame's channel map — reading zeros past the bytes it has.
#[test]
fn ac3_headers_parse_as_libavcodec_parses_them() {
  assert_eq!(
    ac3_header(&AC3),
    Some(Ac3Header {
      bsid: 8,
      frame_type: 2,
      size: 448,
      blocks: 6
    })
  );
  assert_eq!(
    ac3_header(&eac3(0, 3)),
    Some(Ac3Header {
      bsid: 16,
      frame_type: 0,
      size: 768,
      blocks: 6
    })
  );
  assert_eq!(ac3_header(&eac3(0, 1)).map(|header| header.blocks), Some(2));
  // Two bytes of sync and zeros past them: an AC-3 frame of 128 bytes.
  assert_eq!(
    ac3_header(&[0x0B, 0x77]).map(|header| header.size),
    Some(128)
  );
  let mut no_sync = AC3;
  no_sync[0] = 0x0C;
  assert_eq!(ac3_header(&no_sync), None);
  let mut bsid = AC3;
  bsid[5] = 17 << 3;
  assert_eq!(ac3_header(&bsid), None, "a bit stream id past 16");
  let mut rate = AC3;
  rate[4] = 0xC0 | 0x0E;
  assert_eq!(ac3_header(&rate), None, "AC-3's reserved sample rate");
  let mut size = AC3;
  size[4] = 38;
  assert_eq!(ac3_header(&size), None, "a frame size code past 37");
  let mut reserved = eac3(0, 3);
  reserved[2] |= 0xC0;
  assert_eq!(ac3_header(&reserved), None, "E-AC-3's reserved frame type");
  let mut substream = eac3(0, 3);
  substream[2] |= 1 << 3;
  assert_eq!(
    ac3_header(&substream),
    None,
    "a substream other than the first"
  );
  let mut tiny = eac3(0, 3);
  tiny[2..4].copy_from_slice(&[0x00, 0x02]);
  assert_eq!(ac3_header(&tiny), None, "a frame of 6 bytes");
  let mut rates = eac3(0, 3);
  rates[4] = 0xF4;
  assert_eq!(ac3_header(&rates), None, "both sample rate codes reserved");
  // A dependent frame's channel map: 22 channels is past 16; two is not.
  let mut wide = eac3(1, 3);
  wide.truncate(6);
  wide.extend([0x1F, 0xFF, 0xF0]);
  assert_eq!(ac3_header(&wide), None);
  let mut pair = eac3(1, 3);
  pair.truncate(6);
  pair.extend([0x1A, 0x00, 0x00]);
  assert_eq!(ac3_header(&pair).map(|header| header.frame_type), Some(1));
}

/// **MP4's AC-3 handling is held as MP4 holds it**: a header that does not
/// parse; an E-AC-3 track's first packet tiled by dependent frames; a
/// second independent frame ending the first set; frames of fewer than six
/// blocks joined into sixes, and none past.
#[test]
fn mp4s_ac3_handling_is_mirrored() {
  let start = Ac3State::default();
  let ac3 = frame(&AC3, 448);
  let learned = ac3_packet(start, false, &ac3).expect("an AC-3 frame");
  assert_eq!(
    learned,
    Ac3State {
      done: true,
      bsid: 8,
      pending: 0
    }
  );
  assert_eq!(
    ac3_packet(learned, false, &[1, 2, 3]),
    Err(PayloadRule::Ac3Header)
  );
  // E-AC-3: the first packet, alone or with a dependent frame after it.
  let independent = frame(&eac3(0, 3), 768);
  let first = ac3_packet(start, true, &independent).expect("one frame");
  assert_eq!(
    first,
    Ac3State {
      done: false,
      bsid: 16,
      pending: 0
    }
  );
  let mut with_dependent = independent.clone();
  with_dependent.extend(frame(&eac3(1, 3), 768));
  assert!(ac3_packet(start, true, &with_dependent).is_ok());
  let mut with_independent = independent.clone();
  with_independent.extend(frame(&eac3(0, 3), 768));
  assert_eq!(
    ac3_packet(start, true, &with_independent),
    Err(PayloadRule::Ac3Substreams)
  );
  let mut left_over = independent.clone();
  left_over.extend([1, 2, 3]);
  assert_eq!(
    ac3_packet(start, true, &left_over),
    Err(PayloadRule::Ac3Substreams)
  );
  // The second independent frame ends the first set: no tiling is asked
  // of it.
  let second = ac3_packet(first, true, &left_over).expect("the second packet");
  assert!(second.done);
  // Two-block frames join into sixes; a three-block frame after four
  // blocks, or a six-block frame after two, runs past.
  let two = frame(&eac3(0, 1), 768);
  let three = frame(&eac3(0, 2), 768);
  let mut state = second;
  for pending in [2, 4, 0] {
    state = ac3_packet(state, true, &two).expect("two blocks");
    assert_eq!(state.pending, pending);
  }
  let four = ac3_packet(ac3_packet(state, true, &two).expect("two"), true, &two).expect("four");
  assert_eq!(ac3_packet(four, true, &three), Err(PayloadRule::Ac3Blocks));
  let held = ac3_packet(state, true, &two).expect("two");
  assert_eq!(
    ac3_packet(held, true, &independent),
    Err(PayloadRule::Ac3Blocks)
  );
}
