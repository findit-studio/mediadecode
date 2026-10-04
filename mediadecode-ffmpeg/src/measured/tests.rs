use super::*;

fn ms() -> Timebase {
  Timebase::MILLIS
}

fn at(ticks: i64) -> Timestamp {
  Timestamp::new(ticks, ms())
}

/// The end of track `track`, if it has one.
fn end_of(m: &Measured, track: usize) -> Option<Timestamp> {
  m.get(track, ms()).map(|e| e.end())
}

/// Nothing is measured before a packet arrives, on any track — and a
/// track outside the table is simply absent.
#[test]
fn nothing_is_measured_before_a_packet_arrives() {
  let m = Measured::new(3).expect("reserve");
  for track in 0..3 {
    assert_eq!(m.get(track, ms()), None);
  }
  assert_eq!(m.get(3, ms()), None, "outside the table");
  assert_eq!(Measured::new(0).expect("reserve").get(0, ms()), None);
}

/// The end is the packet's timestamp plus its duration: the last 25 fps
/// video frame of a two-second clip starts at 1960 ms and ends at 2000.
/// Every packet so far carried both, so the end is exact.
#[test]
fn a_packets_end_is_its_timestamp_plus_its_duration() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(1_960), 40);
  assert_eq!(
    m.get(0, ms()),
    Some(MeasuredEnd::new(at(2_000), false, true))
  );
}

/// A packet that carries no duration ends where it starts — and the end
/// is then a lower bound, because the walk cannot say where that packet
/// really ends. Zero and negative both mean none, the way the packet
/// conversion reads them.
#[test]
fn a_packet_without_a_duration_ends_where_it_starts_and_the_end_is_a_lower_bound() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(500), 0);
  let figure = m.get(0, ms()).expect("measured");
  assert_eq!(figure.end(), at(500));
  assert!(!figure.exact());

  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(600), -3);
  let figure = m.get(0, ms()).expect("measured");
  assert_eq!(figure.end(), at(600));
  assert!(!figure.exact());
}

/// The first packet with an unknown duration costs the track its
/// exactness for good: later packets that carry one do not give it
/// back, because the unknown end may be the greatest.
#[test]
fn a_lower_bound_stays_a_lower_bound() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  assert!(m.get(0, ms()).expect("measured").exact());
  m.observe(0, Some(40), 0);
  assert!(!m.get(0, ms()).expect("measured").exact());
  m.observe(0, Some(80), 40);
  let figure = m.get(0, ms()).expect("measured");
  assert_eq!(figure.end(), at(120));
  assert!(!figure.exact());
}

/// A track whose every packet carries a timestamp and a positive
/// duration is exact.
#[test]
fn a_track_whose_packets_all_carry_a_duration_is_exact() {
  let mut m = Measured::new(1).expect("reserve");
  for pts in (0..2_000).step_by(40) {
    m.observe(0, Some(pts), 40);
  }
  let figure = m.get(0, ms()).expect("measured");
  assert_eq!(figure.end(), at(2_000));
  assert!(figure.exact());
}

/// A packet that was delivered with no timestamp has no end to place,
/// so the end is a lower bound — and the figure itself does not move.
#[test]
fn a_delivered_packet_without_a_timestamp_makes_the_end_a_lower_bound() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.untimed_delivery(0);
  let figure = m.get(0, ms()).expect("measured");
  assert_eq!(figure.end(), at(40));
  assert!(!figure.exact());
}

/// Exactness is a property of one track: another track's unknown
/// duration does not reach it.
#[test]
fn inexactness_is_kept_per_track() {
  let mut m = Measured::new(2).expect("reserve");
  m.observe(0, Some(0), 40);
  m.observe(1, Some(0), 0);
  assert!(m.get(0, ms()).expect("measured").exact());
  assert!(!m.get(1, ms()).expect("measured").exact());
}

/// The greatest end, not the last one delivered: presentation order is
/// not delivery order once a track has reordered frames.
#[test]
fn the_figure_is_the_greatest_end_and_not_the_last_one() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(80), 40);
  m.observe(0, Some(40), 40);
  m.observe(0, Some(0), 40);
  assert_eq!(end_of(&m, 0), Some(at(120)));
}

/// A packet with no timestamp says nothing about when anything ends,
/// whatever duration it carries.
#[test]
fn a_packet_without_a_timestamp_is_passed_over() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, None, 40);
  assert_eq!(m.get(0, ms()), None);
  m.observe(0, Some(10), 40);
  m.observe(0, None, 1_000_000);
  assert_eq!(end_of(&m, 0), Some(at(50)));
}

/// Each track keeps its own figure, in its own ticks, and an index
/// outside the table is ignored rather than a panic: the packet loop
/// passes such a packet by before it gets here, and this does not
/// depend on that.
#[test]
fn each_track_keeps_its_own_figure_and_a_stray_index_is_ignored() {
  let mut m = Measured::new(2).expect("reserve");
  m.observe(0, Some(1_960), 40);
  m.observe(1, Some(3), 1);
  m.observe(7, Some(99), 1);
  m.untimed_delivery(7);
  assert_eq!(end_of(&m, 0), Some(at(2_000)));
  assert_eq!(
    m.get(1, Timebase::SECONDS).map(|e| e.end()),
    Some(Timestamp::new(4, Timebase::SECONDS)),
    "the figure is expressed in the timebase the caller names",
  );
  assert_eq!(m.get(7, ms()), None);
}

/// An end that does not fit in an `i64` is no end at all. Saturating
/// would name `i64::MAX` as an exact end and let end of file call the
/// walk complete over it; instead the track answers none, a later
/// smaller packet does not resurrect it, and no other track's figure is
/// called complete while this one is missing.
#[test]
fn an_end_that_does_not_fit_is_no_end_and_no_walk_is_complete() {
  let mut m = Measured::new(2).expect("reserve");
  m.observe(0, Some(0), 40);
  m.observe(1, Some(i64::MAX - 1), 40);
  assert_eq!(m.get(1, ms()), None, "the end is unrepresentable");

  m.observe(1, Some(5), 40);
  assert_eq!(
    m.get(1, ms()),
    None,
    "a later, smaller packet does not make it representable"
  );

  m.end_of_file();
  assert_eq!(
    m.get(0, ms()),
    Some(MeasuredEnd::new(at(40), false, true)),
    "one track's end is missing, so the walk is not complete for the others",
  );
}

/// The largest end that does fit is an ordinary figure: the check is
/// for overflow, not for large.
#[test]
fn the_largest_end_that_fits_is_still_a_figure() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(i64::MAX - 40), 40);
  assert_eq!(end_of(&m, 0), Some(at(i64::MAX)));
  m.end_of_file();
  assert!(m.get(0, ms()).expect("measured").walk_complete());
}

/// A track that starts before zero still ends where its last packet
/// does, which can itself be before zero.
#[test]
fn an_end_before_zero_is_still_an_end() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(-50), 40);
  assert_eq!(end_of(&m, 0), Some(at(-10)));
}

/// The walk is complete only when end of file answers a walk that
/// skipped nothing.
#[test]
fn end_of_file_completes_a_walk_that_skipped_nothing() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  assert!(!m.get(0, ms()).expect("measured").walk_complete());
  m.end_of_file();
  assert!(m.get(0, ms()).expect("measured").walk_complete());
}

/// **A completed walk is frozen.** Once end of file has completed it,
/// nothing that is read afterwards — a larger end, a delivered packet
/// with no timestamp, an end too large to represent — changes the
/// figure, its exactness or the flag: the figure is final, and the
/// doc's word for it is the implementation's.
#[test]
fn a_completed_walk_is_frozen() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.end_of_file();
  let frozen = m.get(0, ms()).expect("measured");
  assert_eq!(frozen, MeasuredEnd::new(at(40), true, true));

  m.observe(0, Some(1_000), 40);
  assert_eq!(
    m.get(0, ms()),
    Some(frozen),
    "a larger end does not move it"
  );
  m.observe(0, Some(2_000), 0);
  assert_eq!(m.get(0, ms()), Some(frozen), "nor does an unknown duration");
  m.untimed_delivery(0);
  assert_eq!(m.get(0, ms()), Some(frozen), "nor an untimed delivery");
  m.observe(0, Some(i64::MAX - 1), 40);
  assert_eq!(m.get(0, ms()), Some(frozen), "nor an unrepresentable end");
}

/// **A seek after completion keeps the walk complete.** The break is
/// ignored by a frozen walk, and the packets read again are the ones
/// already counted.
#[test]
fn a_seek_after_completion_keeps_the_walk_complete() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.end_of_file();
  m.break_walk();
  m.observe(0, Some(0), 40);
  m.end_of_file();
  assert_eq!(m.get(0, ms()), Some(MeasuredEnd::new(at(40), true, true)),);
}

/// **A break before completion means the walk can never complete in
/// this session, and measurement carries on as a "so far" maximum.**
/// The figure keeps growing, and end of file does not complete it.
#[test]
fn a_break_before_completion_means_never_complete_and_the_figure_keeps_growing() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.break_walk();
  m.observe(0, Some(40), 40);
  assert_eq!(
    m.get(0, ms()),
    Some(MeasuredEnd::new(at(80), false, true)),
    "the figure grows after the break",
  );
  m.end_of_file();
  m.observe(0, Some(80), 40);
  assert_eq!(
    m.get(0, ms()),
    Some(MeasuredEnd::new(at(120), false, true)),
    "end of file does not complete a broken walk, and the figure keeps growing",
  );
}
