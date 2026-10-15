//! The panic guard on the custom-writer boundary — the write side of
//! [`crate::reader_guard`].
//!
//! A muxer writes into any `Write + Seek` a caller hands it, and
//! libavformat drives that sink through `AVIOContext` callbacks that are
//! `extern "C"` functions (`ffmpeg_next::format::context::StreamIo`'s
//! `write` and `seek`), with no `catch_unwind` between the caller's code
//! and C. A panic crossing an `extern "C"` frame is not an unwind any
//! caller can catch — it aborts the process.
//!
//! [`GuardedWriter`] closes that on this side of the boundary, exactly as
//! the reader guard does: every call into the caller's sink runs under
//! [`catch_unwind`](std::panic::catch_unwind), a panic is latched in the
//! shared [`PanicLatch`] and reported to C as an ordinary I/O error, and
//! the muxer names the latch once control is back in Rust. The panic
//! never reaches the `extern "C"` frame.
//!
//! **Nor does an error's payload.** ffmpeg-next drops an error the sink
//! returns inside its `extern "C"` callback, and an `io::Error` can box
//! a payload whose `Drop` panics. Every error the sink returns is
//! reduced inside the guard's `catch_unwind` to its OS code or kind, its
//! payload dropped there ([`InertError`]): a destructor that panics is
//! latched like any other panic, and what reaches C owns nothing.
//!
//! **A latched sink is terminal.** Unlike a reader, which libavformat may
//! simply stop reading, a sink that panicked part way through a write is
//! in a state nothing can describe — some bytes of a packet may have
//! landed and some not. Every call after the latch answers an error
//! without touching the sink again, so nothing more is written into a
//! file whose writer has already failed in a way it could not report.
//!
//! The guard cannot help under `panic = "abort"`, where no unwind exists
//! to catch; that is a whole-binary choice the caller makes.

use std::{
  io::{self, Seek, SeekFrom, Write},
  panic::{AssertUnwindSafe, catch_unwind},
  sync::Arc,
};

use crate::reader_guard::{InertError, PanicLatch};

/// A caller's byte sink, wrapped so that a panic inside it becomes an
/// I/O error instead of an abort, and stays one.
///
/// Deliberately does **not** override `Write::write_all`,
/// `Write::write_vectored` or `Seek::stream_position`: their default
/// implementations are written in terms of [`Write::write`] and
/// [`Seek::seek`], which are guarded, so forwarding them would only add
/// unguarded paths.
pub struct GuardedWriter<W> {
  inner: W,
  latch: Arc<PanicLatch>,
}

impl<W> GuardedWriter<W> {
  /// Wraps `inner`, returning the guard and a handle on the latch the
  /// muxer keeps to name a panic after the fact.
  pub fn new(inner: W) -> (Self, Arc<PanicLatch>) {
    let latch = Arc::new(PanicLatch::default());
    (
      Self {
        inner,
        latch: Arc::clone(&latch),
      },
      latch,
    )
  }

  /// The caller's sink, handed back.
  ///
  /// Whatever state a sink that panicked is in, it is the caller's to
  /// judge: the guard returns it either way, and the latch says whether
  /// it panicked.
  pub fn into_inner(self) -> W {
    self.inner
  }

  /// Runs one call into the caller's sink under `catch_unwind` — or none
  /// at all, once a panic is latched — an error it returns reduced there
  /// ([`InertError`]).
  fn guard<T>(&mut self, call: impl FnOnce(&mut W) -> io::Result<T>) -> io::Result<T> {
    let Self { inner, latch } = self;
    if latch.is_latched() {
      return Err(io::Error::other(
        "the caller's writer panicked earlier, and is not called again",
      ));
    }
    // `AssertUnwindSafe` because the sink is exactly what may be left
    // inconsistent by its own panic — and that is why the latch makes the
    // sink terminal: nothing calls it again.
    //
    // An error the sink returns is reduced inside the catch, so its
    // payload is dropped here and not in ffmpeg-next's `extern "C"`
    // callback: a payload whose `Drop` panics takes the path below, and
    // the sink that returned it is terminal like one that panicked.
    match catch_unwind(AssertUnwindSafe(|| call(inner).map_err(InertError::reduce))) {
      Ok(result) => result.map_err(InertError::rebuild),
      Err(payload) => {
        latch.latch(&*payload);
        // Described, then deliberately forgotten, for the reason the
        // reader guard gives: a payload whose `Drop` panics would send
        // that second panic out of `write` and into ffmpeg-next's
        // `extern "C"` callback — the abort this guard exists to
        // prevent, reached through the guard itself.
        core::mem::forget(payload);
        Err(io::Error::other("the caller's writer panicked"))
      }
    }
  }
}

impl<W: Write> Write for GuardedWriter<W> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.guard(|inner| inner.write(buf))
  }

  fn flush(&mut self) -> io::Result<()> {
    self.guard(|inner| inner.flush())
  }
}

impl<W: Seek> Seek for GuardedWriter<W> {
  fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
    self.guard(|inner| inner.seek(pos))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A sink that panics in each of its three calls, and counts them.
  #[derive(Default)]
  struct Panicking {
    calls: u32,
  }

  impl Write for Panicking {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      self.calls += 1;
      panic!("write exploded");
    }
    fn flush(&mut self) -> io::Result<()> {
      self.calls += 1;
      panic!("flush exploded");
    }
  }

  impl Seek for Panicking {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      self.calls += 1;
      panic!("seek exploded");
    }
  }

  /// Silences the default hook for one call, so a deliberate panic does
  /// not print a backtrace into the test log. The hook is global; the
  /// swap is confined to this module's laws, as the reader guard's is.
  fn quietly<T>(call: impl FnOnce() -> T) -> T {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = call();
    std::panic::set_hook(previous);
    out
  }

  /// **A panicking write becomes an error, latches its message, and the
  /// sink is never called again.** The second write, the flush and the
  /// seek all answer errors without reaching it — a sink that failed in a
  /// way it could not report is not written into further.
  #[test]
  fn a_panicking_write_latches_and_the_sink_is_not_called_again() {
    let (mut guarded, latch) = GuardedWriter::new(Panicking::default());
    assert!(latch.message().is_none(), "nothing has panicked yet");

    let err = quietly(|| guarded.write(b"bytes")).expect_err("a panic is an error, not an abort");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(latch.message().as_deref(), Some("write exploded"));

    assert!(guarded.write(b"more").is_err());
    assert!(guarded.flush().is_err());
    assert!(guarded.seek(SeekFrom::Start(0)).is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "only the first call reached the sink",
    );
  }

  /// **A panicking seek, and a panicking flush, latch the same way** —
  /// and `stream_position`, the default over `seek`, is guarded with it.
  #[test]
  fn a_panicking_seek_or_flush_latches_its_message() {
    let (mut guarded, latch) = GuardedWriter::new(Panicking::default());
    assert!(quietly(|| guarded.seek(SeekFrom::Start(4))).is_err());
    assert_eq!(latch.message().as_deref(), Some("seek exploded"));
    assert!(quietly(|| guarded.stream_position()).is_err());

    let (mut guarded, latch) = GuardedWriter::new(Panicking::default());
    assert!(quietly(|| guarded.flush()).is_err());
    assert_eq!(latch.message().as_deref(), Some("flush exploded"));
  }

  /// A panic payload whose destructor panics in turn.
  struct PanicOnDrop;

  impl Drop for PanicOnDrop {
    fn drop(&mut self) {
      panic!("and the payload went too");
    }
  }

  struct PanicsWithAPayload;

  impl Write for PanicsWithAPayload {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      std::panic::panic_any(PanicOnDrop);
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  /// **A payload that panics on drop is not dropped** — this law
  /// completing at all is the assertion, as the reader guard's is.
  #[test]
  fn a_payload_that_panics_on_drop_does_not_get_dropped() {
    let (mut guarded, latch) = GuardedWriter::new(PanicsWithAPayload);
    assert!(quietly(|| guarded.write(b"bytes")).is_err());
    assert!(latch.message().is_some());
  }

  /// An error's payload whose destructor panics — safe code, returned
  /// rather than raised.
  #[derive(Debug)]
  struct HostilePayload;

  impl core::fmt::Display for HostilePayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
      f.write_str("an error whose payload panics when dropped")
    }
  }

  impl std::error::Error for HostilePayload {}

  impl Drop for HostilePayload {
    fn drop(&mut self) {
      panic!("the error's payload exploded");
    }
  }

  /// A sink whose every call returns an error carrying that payload, and
  /// counts the calls.
  #[derive(Default)]
  struct ReturnsAHostileError {
    calls: u32,
  }

  impl ReturnsAHostileError {
    fn fail<T>(&mut self) -> io::Result<T> {
      self.calls += 1;
      Err(io::Error::new(io::ErrorKind::StorageFull, HostilePayload))
    }
  }

  impl Write for ReturnsAHostileError {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
      self.fail()
    }
    fn flush(&mut self) -> io::Result<()> {
      self.fail()
    }
  }

  impl Seek for ReturnsAHostileError {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
      self.fail()
    }
  }

  /// **An error whose payload panics on drop is reduced inside the
  /// guard: the destructor's panic is latched and named, and the sink is
  /// not called again.** ffmpeg-next drops a returned error inside its
  /// `extern "C"` callback, where that panic would abort the process
  /// (`output::tests` drives the whole road in a child process); here it
  /// is caught where the error is reduced, and what leaves the guard is
  /// the guard's own error.
  #[test]
  fn an_error_whose_payload_panics_on_drop_is_latched_and_named() {
    let (mut guarded, latch) = GuardedWriter::new(ReturnsAHostileError::default());
    let err = quietly(|| guarded.write(b"bytes")).expect_err("the write failed");
    let said = err.to_string();
    // Forgotten, not dropped: were the guard to hand the payload out,
    // dropping it while this law fails would panic during the unwind and
    // abort the test binary instead of naming the defect.
    core::mem::forget(err);
    assert_eq!(said, "the caller's writer panicked");
    assert_eq!(
      latch.message().as_deref(),
      Some("the error's payload exploded")
    );
    assert!(guarded.write(b"more").is_err());
    assert!(guarded.flush().is_err());
    assert!(guarded.seek(SeekFrom::Start(0)).is_err());
    assert_eq!(
      guarded.into_inner().calls,
      1,
      "a sink whose error panicked is not called again",
    );
  }

  /// **An error leaves the guard saying what it said, and owning
  /// nothing**: an OS error keeps its code, any other its kind — all
  /// ffmpeg-next reads of it — and neither carries a payload out. Nothing
  /// is latched: an error is not a panic, and the sink stays callable.
  #[test]
  fn a_returned_error_keeps_its_code_or_kind_and_no_payload() {
    #[derive(Default)]
    struct Failing {
      calls: u32,
    }
    impl Write for Failing {
      fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        Err(io::Error::from_raw_os_error(28))
      }
      fn flush(&mut self) -> io::Result<()> {
        self.calls += 1;
        Err(io::Error::new(
          io::ErrorKind::WriteZero,
          "the sink's own words",
        ))
      }
    }
    let (mut guarded, latch) = GuardedWriter::new(Failing::default());
    let os = guarded.write(b"bytes").expect_err("the write failed");
    assert_eq!(os.raw_os_error(), Some(28));
    assert!(os.get_ref().is_none(), "no payload leaves the guard");
    let custom = guarded.flush().expect_err("the flush failed");
    assert_eq!(custom.kind(), io::ErrorKind::WriteZero);
    assert!(custom.get_ref().is_none(), "no payload leaves the guard");
    assert!(latch.message().is_none(), "an error is not a panic");
    assert!(guarded.write(b"again").is_err());
    assert_eq!(guarded.into_inner().calls, 3, "the sink is called again");
  }

  #[test]
  fn an_unremarkable_writer_passes_straight_through() {
    let (mut guarded, latch) = GuardedWriter::new(io::Cursor::new(Vec::new()));
    guarded.write_all(b"abcd").expect("write");
    assert_eq!(guarded.seek(SeekFrom::Start(1)).expect("seek"), 1);
    guarded.write_all(b"X").expect("write");
    guarded.flush().expect("flush");
    assert!(latch.message().is_none());
    assert_eq!(guarded.into_inner().into_inner(), b"aXcd");
  }
}
