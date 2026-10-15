use core::num::NonZeroI32;

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
