//! [`ContainerFormat`] — what libavformat decided the bytes are wrapped
//! in, surfaced as the demuxer's own word.
//!
//! Read once from `AVFormatContext.iformat` while the session is opened
//! and kept for its life: libavformat picks the demuxer during
//! `avformat_open_input` and never changes it, so re-reading could only
//! answer the same thing more expensively.

use smol_str::SmolStr;

/// Upper bound on the NUL search over an `AVInputFormat`'s strings.
///
/// The longest name in FFmpeg's demuxer table is a comma list a few
/// dozen bytes long and the longest description a couple of hundred;
/// the cap is generous for both and exists only so that a
/// version-skewed table cannot turn the walk into an unbounded read.
const FORMAT_TEXT_MAX_BYTES: usize = 1024;

/// The container identification libavformat made for a session.
///
/// # This is the DEMUXER's identity, not a filename's
///
/// It is `AVInputFormat`'s own words — what libavformat concluded from
/// the *bytes*, having probed them. That is precisely what a
/// content-addressed row wants and what an extension cannot give: the
/// same bytes at `movie.mov` and `movie.mp4` are one content, and this
/// answers identically for both because it never looked at the path.
///
/// # One demuxer, several words — and [`name`](Self::name) is the list
///
/// FFmpeg registers one demuxer per *family*, so its name is a
/// comma-separated list of the short names that family handles:
/// `"mov,mp4,m4a,3gp,3g2,mj2"` for the ISOBMFF demuxer, `"matroska,webm"`
/// for Matroska. [`name`](Self::name) is that string verbatim and
/// [`names`](Self::names) walks it.
///
/// **The list does not narrow to one word, and this type does not
/// pretend it does.** libavformat identified the *demuxer*; which brand
/// inside that family a file is — an `.mp4` against an `.m4a` — is a
/// question it did not answer and one nothing here can answer for it.
/// A door that returned a single word would have had to pick, and a
/// pick is the guess this whole seat exists to avoid.
///
/// # Crossing into a typed vocabulary
///
/// The words are FFmpeg's own slugs, which is what makes them
/// crossable: a consumer that wants
/// [`mediaframe::container::Format`](https://docs.rs/mediaframe) tries
/// [`names`](Self::names) against its `FromStr` and takes what it
/// recognises, deciding for itself what to do when a family offers
/// several. This crate stays out of that decision — the fold belongs to
/// whoever owns the vocabulary, and doing it here would put a second
/// one somewhere a caller cannot see it.
///
/// # An open vocabulary
///
/// Never an enum. A container FFmpeg learns to demux in its next
/// release names itself here with nothing to change on this side, which
/// is the whole reason the identity is carried as text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContainerFormat {
  name: SmolStr,
  long_name: Option<SmolStr>,
}

impl ContainerFormat {
  /// Constructs a `ContainerFormat` from a demuxer's short-name list and
  /// its optional description.
  ///
  /// Public so a consumer can build the value in a test or a fake; a
  /// session builds its own from `AVFormatContext.iformat`.
  #[inline]
  pub const fn new(name: SmolStr, long_name: Option<SmolStr>) -> Self {
    Self { name, long_name }
  }

  /// The demuxer's short-name list, verbatim — `"mov,mp4,m4a,3gp,3g2,mj2"`.
  ///
  /// See [`names`](Self::names) for the words in it.
  #[inline]
  pub fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The demuxer's human description — `"QuickTime / MOV"` — or `None`
  /// where the table carries none.
  ///
  /// FFmpeg's prose, for display. It is not stable across releases and
  /// nothing should key on it.
  #[inline]
  pub fn long_name(&self) -> Option<&str> {
    self.long_name.as_deref()
  }

  /// The short names in [`name`](Self::name), in the order the demuxer
  /// lists them.
  ///
  /// Every FFmpeg name is a non-empty word or a comma list of them, so
  /// this yields at least one item for any format a session opened
  /// with.
  pub fn names(&self) -> impl Iterator<Item = &str> {
    self.name.split(',').filter(|word| !word.is_empty())
  }

  /// Reads the format out of a live `AVFormatContext`, or `None` where
  /// it carries no `iformat` or the table's name is not readable text.
  ///
  /// The name is the identity: a format that cannot be named is no
  /// answer at all, so it is `None` rather than a value with an empty
  /// word. A missing *description* is not the same thing and keeps the
  /// value.
  ///
  /// # Safety
  ///
  /// `context` must be a live `*const AVFormatContext`.
  pub(crate) unsafe fn from_context(
    context: *const ffmpeg_next::ffi::AVFormatContext,
  ) -> Option<Self> {
    if context.is_null() {
      return None;
    }
    // SAFETY: `context` is live per the contract; `iformat` is a public
    // field holding a pointer into libavformat's own demuxer table (or
    // null before a successful open).
    let iformat = unsafe { (*context).iformat };
    if iformat.is_null() {
      return None;
    }
    // SAFETY: a non-null `iformat` points at a `static const`
    // `AVInputFormat` compiled into libavformat — live for the process
    // and never written — so both string fields may be read, and each
    // is independently nullable.
    let (name, long_name) = unsafe { ((*iformat).name, (*iformat).long_name) };
    // SAFETY: both are null or NUL-terminated string literals in that
    // same static table; the reader answers `None` for null.
    let name = unsafe { crate::ffi::table_text(name, FORMAT_TEXT_MAX_BYTES) }?;
    let long_name = unsafe { crate::ffi::table_text(long_name, FORMAT_TEXT_MAX_BYTES) };
    Some(Self::new(name, long_name))
  }
}

#[cfg(test)]
mod tests;
