use super::*;

fn ms() -> Timebase {
  Timebase::MILLIS
}

fn at(ticks: i64) -> Timestamp {
  Timestamp::new(ticks, ms())
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
#[test]
fn a_packets_end_is_its_timestamp_plus_its_duration() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(1_960), 40);
  assert_eq!(m.get(0, ms()), Some(MeasuredEnd::new(at(2_000), false)));
}

/// A packet that carries no duration ends where it starts. Zero and
/// negative both mean none, the way the packet conversion reads them.
#[test]
fn a_packet_without_a_duration_ends_where_it_starts() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(500), 0);
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(500)));
  m.observe(0, Some(600), -3);
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(600)));
}

/// The greatest end, not the last one delivered: presentation order is
/// not delivery order once a track has reordered frames.
#[test]
fn the_figure_is_the_greatest_end_and_not_the_last_one() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(80), 40);
  m.observe(0, Some(40), 40);
  m.observe(0, Some(0), 40);
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(120)));
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
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(50)));
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
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(2_000)));
  assert_eq!(
    m.get(1, Timebase::SECONDS).map(|e| e.end()),
    Some(Timestamp::new(4, Timebase::SECONDS)),
    "the figure is expressed in the timebase the caller names",
  );
  assert_eq!(m.get(7, ms()), None);
}

/// An end that does not fit in an `i64` is no end at all. Saturating
/// would name `i64::MAX` as an exact end and let end of file call it
/// final; instead the track answers none, a later smaller packet does
/// not resurrect it, and no other track's figure is called final while
/// this one is missing.
#[test]
fn an_end_that_does_not_fit_is_no_end_and_no_figure_is_final() {
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
    Some(MeasuredEnd::new(at(40), false)),
    "one track's end is missing, so the pass is not final for the others",
  );
}

/// The largest end that does fit is an ordinary figure: the check is
/// for overflow, not for large.
#[test]
fn the_largest_end_that_fits_is_still_a_figure() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(i64::MAX - 40), 40);
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(i64::MAX)));
  m.end_of_file();
  assert!(m.get(0, ms()).expect("measured").reached_end());
}

/// A track that starts before zero still ends where its last packet
/// does, which can itself be before zero.
#[test]
fn an_end_before_zero_is_still_an_end() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(-50), 40);
  assert_eq!(m.get(0, ms()).map(|e| e.end()), Some(at(-10)));
}

/// The walk is final only when end of file answers an unbroken pass.
#[test]
fn end_of_file_makes_the_figures_final_on_an_unbroken_pass() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  assert!(!m.get(0, ms()).expect("measured").reached_end());
  m.end_of_file();
  assert!(m.get(0, ms()).expect("measured").reached_end());
}

/// A pass a seek or a dropped packet broke never claims to be final,
/// though the figure it measured is still reported.
#[test]
fn a_broken_pass_never_claims_to_be_final() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.break_pass();
  m.end_of_file();
  let measured = m.get(0, ms()).expect("measured");
  assert!(!measured.reached_end());
  assert_eq!(measured.end(), at(40));
}

/// A figure that is already final stays final when a later seek breaks
/// the pass: the packets delivered again are the ones already counted.
#[test]
fn a_final_figure_stays_final_when_a_later_seek_breaks_the_pass() {
  let mut m = Measured::new(1).expect("reserve");
  m.observe(0, Some(0), 40);
  m.end_of_file();
  m.break_pass();
  m.observe(0, Some(0), 40);
  m.end_of_file();
  assert_eq!(m.get(0, ms()), Some(MeasuredEnd::new(at(40), true)),);
}
