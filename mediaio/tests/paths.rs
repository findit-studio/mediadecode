//! Each default feature brings its member under its short name, and the
//! name is the member's own item, not a look-alike.

#![cfg(all(
  feature = "core",
  feature = "decode",
  feature = "encode",
  feature = "timeline"
))]

use core::marker::PhantomData;

/// Compiles only where both arguments are one type.
fn same<T>(_: PhantomData<T>, _: PhantomData<T>) {}

#[test]
fn the_default_members_answer_under_their_short_names() {
  same(
    PhantomData::<mediaio::spine::Received>,
    PhantomData::<mediaspine::Received>,
  );
  same(
    PhantomData::<mediaio::decode::Received>,
    PhantomData::<mediaio::encode::Received>,
  );
  same(
    PhantomData::<mediaio::spine::demuxer::TrackIndex>,
    PhantomData::<mediaio::decode::demuxer::TrackIndex>,
  );
  same(
    PhantomData::<mediaio::encode::Flushed>,
    PhantomData::<mediaencode::Flushed>,
  );
  same(
    PhantomData::<mediaio::timeline::Timeline>,
    PhantomData::<mediaio_timeline::Timeline>,
  );
}
