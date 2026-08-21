//! Conversions from FFmpeg's [`ffmpeg_next::ChannelLayout`] /
//! [`ffmpeg_next::ffi::AVChannelOrder`] to the channel-layout vocabulary
//! [`mediaframe`] owns ([`ChannelLayout`], [`ChannelOrder`],
//! [`ChannelSpec`], [`ChannelLayoutDescription`]).
//!
//! These live as **free functions** (not `From` trait impls) because of
//! Rust's orphan rule: this crate owns neither `From` nor
//! `mediaframe::audio::*`, so we can't write the `impl` here. Calling
//! `mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(layout)`
//! is the ergonomic boundary instead.
//!
//! FFmpeg's own type is imported as [`AvChannelLayout`] so the name
//! [`ChannelLayout`] can stay with the vocabulary these functions
//! produce.

use core::{ffi::c_char, slice, str::FromStr};

use ffmpeg_next::{ChannelLayout as AvChannelLayout, ffi};
use mediaframe::audio::{ChannelLayout, ChannelLayoutDescription, ChannelOrder, ChannelSpec};
use smol_str::SmolStr;
use std::vec::Vec;

/// Maps an FFmpeg [`AvChannelLayout`] to the named
/// [`ChannelLayout`] vocabulary.
///
/// Two rungs, in order:
///
/// 1. the **constant-arm table** — exactly `ffmpeg_next`'s
///    `ChannelLayout` constant set, compared through
///    `av_channel_layout_compare`;
/// 2. the **describe rung** — for a layout that falls off the table,
///    FFmpeg names it via `av_channel_layout_describe` and that name
///    goes through [`ChannelLayout`]'s own total door (`FromStr`).
///
/// The second rung is what makes `binaural` / `5.1.2` / `9.1.6`
/// reachable: [`ChannelLayout`] names all three, `ffmpeg_next` 9.0.0
/// mints no constant for any of them, so the table alone can never
/// produce them. It is also why a layout a *later* FFmpeg adds is
/// reachable with no edit here, as long as the vocabulary already
/// names it — FFmpeg speaks the name, the vocabulary reads the word,
/// one source.
///
/// Returns [`ChannelLayout::default`] — the `Other("")` absent sentinel
/// — when neither rung names the layout. The rendering itself is not
/// smuggled into `Other`: an unrecognised layout stays *absent*, and
/// [`ChannelLayoutDescription::text`] is where its FFmpeg rendering
/// lives.
pub fn channel_layout_from_ffmpeg(value: &AvChannelLayout) -> ChannelLayout {
  mapped_constant(value).unwrap_or_else(|| channel_layout_from_describe(&describe_layout(value)))
}

/// The constant-arm table — the first and authoritative rung of
/// [`channel_layout_from_ffmpeg`]. `None` means the layout fell off the
/// table and the caller should try the describe rung.
///
/// The arm list is exactly `ffmpeg_next`'s `ChannelLayout` constant set:
/// its `_7POINT1_TOP_BACK` is a `#define` alias of
/// `AV_CH_LAYOUT_5POINT1POINT2_BACK` and so has no arm of its own, and
/// its `BINAURAL` / `_5POINT1POINT2` / `_9POINT1POINT6` siblings — which
/// [`ChannelLayout`] does name — have no constant to match against.
fn mapped_constant(value: &AvChannelLayout) -> Option<ChannelLayout> {
  let named = match () {
    () if value.eq(&AvChannelLayout::MONO) => ChannelLayout::Mono,
    () if value.eq(&AvChannelLayout::STEREO) => ChannelLayout::Stereo,
    () if value.eq(&AvChannelLayout::STEREO_DOWNMIX) => ChannelLayout::StereoDownmix,
    () if value.eq(&AvChannelLayout::SURROUND) => ChannelLayout::Ch3_0,
    () if value.eq(&AvChannelLayout::QUAD) => ChannelLayout::Quad,
    () if value.eq(&AvChannelLayout::HEXAGONAL) => ChannelLayout::Hexagonal,
    () if value.eq(&AvChannelLayout::OCTAGONAL) => ChannelLayout::Octagonal,
    () if value.eq(&AvChannelLayout::HEXADECAGONAL) => ChannelLayout::Hexadecagonal,
    () if value.eq(&AvChannelLayout::CUBE) => ChannelLayout::Cube,
    () if value.eq(&AvChannelLayout::_2POINT1) => ChannelLayout::Ch2_1,
    () if value.eq(&AvChannelLayout::_2_1) => ChannelLayout::Ch3_0Back,
    () if value.eq(&AvChannelLayout::_2_2) => ChannelLayout::QuadSide,
    () if value.eq(&AvChannelLayout::_3POINT1) => ChannelLayout::Ch3_1,
    () if value.eq(&AvChannelLayout::_3POINT1POINT2) => ChannelLayout::Ch3_1_2,
    () if value.eq(&AvChannelLayout::_4POINT0) => ChannelLayout::Ch4_0,
    () if value.eq(&AvChannelLayout::_4POINT1) => ChannelLayout::Ch4_1,
    () if value.eq(&AvChannelLayout::_5POINT0) => ChannelLayout::Ch5_0,
    () if value.eq(&AvChannelLayout::_5POINT0_BACK) => ChannelLayout::Ch5_0Back,
    () if value.eq(&AvChannelLayout::_5POINT1) => ChannelLayout::Ch5_1,
    () if value.eq(&AvChannelLayout::_5POINT1_BACK) => ChannelLayout::Ch5_1Back,
    () if value.eq(&AvChannelLayout::_5POINT1POINT2_BACK) => ChannelLayout::Ch5_1_2Back,
    () if value.eq(&AvChannelLayout::_5POINT1POINT4_BACK) => ChannelLayout::Ch5_1_4Back,
    () if value.eq(&AvChannelLayout::_6POINT0) => ChannelLayout::Ch6_0,
    () if value.eq(&AvChannelLayout::_6POINT0_FRONT) => ChannelLayout::Ch6_0Front,
    () if value.eq(&AvChannelLayout::_6POINT1) => ChannelLayout::Ch6_1,
    () if value.eq(&AvChannelLayout::_6POINT1_BACK) => ChannelLayout::Ch6_1Back,
    () if value.eq(&AvChannelLayout::_6POINT1_FRONT) => ChannelLayout::Ch6_1Front,
    () if value.eq(&AvChannelLayout::_7POINT0) => ChannelLayout::Ch7_0,
    () if value.eq(&AvChannelLayout::_7POINT0_FRONT) => ChannelLayout::Ch7_0Front,
    () if value.eq(&AvChannelLayout::_7POINT1) => ChannelLayout::Ch7_1,
    () if value.eq(&AvChannelLayout::_7POINT1_WIDE) => ChannelLayout::Ch7_1Wide,
    () if value.eq(&AvChannelLayout::_7POINT1_WIDE_BACK) => ChannelLayout::Ch7_1WideBack,
    () if value.eq(&AvChannelLayout::_7POINT1POINT2) => ChannelLayout::Ch7_1_2,
    () if value.eq(&AvChannelLayout::_7POINT1POINT4_BACK) => ChannelLayout::Ch7_1_4Back,
    () if value.eq(&AvChannelLayout::_7POINT2POINT3) => ChannelLayout::Ch7_2_3,
    () if value.eq(&AvChannelLayout::_9POINT1POINT4_BACK) => ChannelLayout::Ch9_1_4Back,
    () if value.eq(&AvChannelLayout::_22POINT2) => ChannelLayout::Ch22_2,
    () => return None,
  };
  Some(named)
}

/// The describe rung: read FFmpeg's own rendering of a layout
/// (`av_channel_layout_describe`, e.g. `"binaural"`, `"5.1(side)"`)
/// through [`ChannelLayout`]'s total `FromStr` door.
///
/// A **named** variant wins. Anything the vocabulary does not name —
/// `FromStr`'s `Other` escape, which is where `"3 channels (FL+FR+TFL)"`
/// and every custom-order rendering land — collapses to
/// [`ChannelLayout::default`], the absent sentinel. That collapse is
/// deliberate: `known_kind` answers *which named layout is this*, and
/// "none of them" is `Other("")`; the rendering is already carried
/// verbatim by [`ChannelLayoutDescription::text`], so letting it ride
/// `Other` too would put a second, differently-shaped copy of the same
/// string in the same struct.
fn channel_layout_from_describe(rendered: &str) -> ChannelLayout {
  ChannelLayout::from_str(rendered)
    .ok()
    .filter(|layout| !matches!(layout, ChannelLayout::Other(_)))
    .unwrap_or_default()
}

/// Maps FFmpeg's [`AVChannelOrder`](ffi::AVChannelOrder) to the
/// [`ChannelOrder`] tag.
pub fn channel_order_from_ffmpeg(value: ffi::AVChannelOrder) -> ChannelOrder {
  // Compare via integer rather than enum-matching: the caller often
  // sources `value` from raw FFmpeg memory (`AVChannelLayout.order`),
  // and an unknown variant would already be UB before reaching this
  // function. Going through `as i32` here is sound because the caller
  // is responsible for the up-conversion path; for the raw-pointer
  // path use [`channel_order_from_raw`].
  channel_order_from_raw(value as i32)
}

/// Variant of [`channel_order_from_ffmpeg`] that takes the raw integer
/// directly. Use this when the caller has just read
/// `AVChannelLayout.order` from FFmpeg memory and doesn't want to
/// risk constructing an invalid bindgen enum value first.
pub fn channel_order_from_raw(raw: i32) -> ChannelOrder {
  match raw {
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 => ChannelOrder::Native,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 => ChannelOrder::Custom,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32 => ChannelOrder::Ambisonic,
    _ => ChannelOrder::Unspecified,
  }
}

/// Builds a fully-populated [`ChannelLayoutDescription`] from an FFmpeg
/// [`AvChannelLayout`].
///
/// - Native / Ambisonic layouts populate `native_mask` from
///   [`AvChannelLayout::bits`] (clearing it to `None` if zero).
/// - Custom layouts populate `custom_channels` from FFmpeg's per-channel
///   list (`AVChannelLayout.u.map`), with each label drawn from
///   `AVChannelCustom.name`.
/// - `text` carries the result of `av_channel_layout_describe`
///   (FFmpeg's human-readable rendering — e.g. `"5.1(side)"`).
/// - `known_kind` runs [`channel_layout_from_ffmpeg`]'s two rungs
///   against that same single rendering: constant table first, then
///   the describe rung.
pub fn channel_layout_description_from_ffmpeg(value: &AvChannelLayout) -> ChannelLayoutDescription {
  // SAFETY: `value` is a live reference; the inner `AVChannelLayout`
  // stays valid for the duration of this call. We hand the raw
  // address into the pointer-based variant which is the canonical
  // implementation (avoids forming `&AVChannelLayout` over a
  // potentially-invalid `order` discriminant).
  unsafe { channel_layout_description_from_raw_ptr(&value.0 as *const ffi::AVChannelLayout) }
}

/// Pointer variant of [`channel_layout_description_from_ffmpeg`].
/// Safe-API callers that already hold a `&AvChannelLayout` should prefer
/// that function; the pointer form exists so the convert path
/// (which never forms `&AVFrame`) can pass `addr_of!((*av_frame).ch_layout)`
/// straight through without materializing a typed reference.
///
/// # Safety
/// `ptr` must be a live `*const AVChannelLayout` for the duration of
/// this call. The function reads `order` raw, then `nb_channels`,
/// then either `u.mask` (NATIVE / AMBISONIC) or `u.map`
/// (CUSTOM) — only after the order discriminant has been validated.
/// It never forms a `&AVChannelLayout` reference.
pub unsafe fn channel_layout_description_from_raw_ptr(
  ptr: *const ffi::AVChannelLayout,
) -> ChannelLayoutDescription {
  use core::ptr::{addr_of, read_unaligned};
  // Read `order` as a raw integer first — never let Rust assume
  // the field is a valid `AVChannelOrder`.
  // SAFETY: `ptr` is a valid `*const AVChannelLayout`; `addr_of!`
  // computes the field address without forming a reference; reading
  // as `i32` matches the bindgen enum's `c_int` storage.
  let order_raw = unsafe { read_unaligned(addr_of!((*ptr).order) as *const i32) };
  let order = channel_order_from_raw(order_raw);
  let nb_channels = unsafe { (*ptr).nb_channels };

  // Native / Ambisonic carry the bitmask in the union. Only read
  // `u.mask` after the order is validated so we don't trip on an
  // unknown order writing into a future variant of the union.
  let native_mask = match order {
    ChannelOrder::Native | ChannelOrder::Ambisonic => {
      // SAFETY: `u.mask` is the union variant for NATIVE/AMBISONIC.
      let mask = unsafe { (*ptr).u.mask };
      if mask != 0 { Some(mask) } else { None }
    }
    _ => None,
  };

  // Build name / rendering through ffmpeg-next helpers. They take
  // `&AvChannelLayout` (which is `repr(transparent)` over
  // `AVChannelLayout`), but at this point we've already validated
  // `order`, so forming the reference is sound: the only enum-typed
  // field in `AVChannelLayout` is `order`, and it now holds a value
  // that came back from `channel_order_from_raw` with the
  // unknown bucket folded into a known variant — but the *underlying
  // struct* still has the original raw bytes. We can't form `&AVChannelLayout`
  // over an unknown order without UB, so for those helpers we
  // explicitly only call them when order is one of the known variants.
  let (known_kind, text) = if matches!(order, ChannelOrder::Unspecified) {
    (ChannelLayout::default(), SmolStr::default())
  } else {
    // SAFETY: `order` is one of {Native, Custom, Ambisonic} — all of
    // which are valid `AVChannelOrder` discriminants present in our
    // bindgen output, so `&*ptr` is sound to form here.
    let layout_ref = unsafe { &*(ptr as *const AvChannelLayout) };
    let text = describe_layout(layout_ref);
    // Constant-arm table first, exactly as in
    // `channel_layout_from_ffmpeg`; the rendering is consulted only
    // when the layout falls off it. Describing once and feeding both
    // fields from that one string keeps `known_kind` and `text`
    // answering from the same FFmpeg call.
    let known_kind =
      mapped_constant(layout_ref).unwrap_or_else(|| channel_layout_from_describe(&text));
    (known_kind, text)
  };
  let custom_channels_vec = unsafe { custom_channels_raw(ptr, order) };

  ChannelLayoutDescription::new(nb_channels.max(0) as u32)
    .with_order(order)
    .with_known_kind(known_kind)
    .with_native_mask(native_mask)
    .with_custom_channels(custom_channels_vec)
    .with_text(text)
}

/// Pointer-form of `custom_channels`. `order` must be the result of
/// reading `(*ptr).order` as `i32` and folding through
/// [`channel_order_from_raw`]; this skips re-reading it.
///
/// # Safety
/// `ptr` must be a live `*const AVChannelLayout`. Reads only fields
/// (`u.map`, `nb_channels`, and the per-channel array) — no `&AVChannelLayout`
/// reference is ever formed.
unsafe fn custom_channels_raw(
  ptr: *const ffi::AVChannelLayout,
  order: ChannelOrder,
) -> Vec<ChannelSpec> {
  use core::ptr::{addr_of, read_unaligned};
  if !matches!(order, ChannelOrder::Custom) {
    return Vec::new();
  }
  let count = unsafe { (*ptr).nb_channels }.max(0) as usize;
  if count == 0 {
    return Vec::new();
  }
  // SAFETY: The `u` field is a union; reading `.map` is sound when
  // `order == CUSTOM` per FFmpeg's documented contract. Guard
  // explicitly for null.
  let map_ptr = unsafe { (*ptr).u.map };
  if map_ptr.is_null() {
    return Vec::new();
  }
  // Iterate the AVChannelCustom array via raw pointers — never form
  // `&[AVChannelCustom]` or `&AVChannelCustom`, because each entry
  // contains `id: AVChannel`, a bindgen enum. If FFmpeg writes an
  // unknown channel id (version skew / hostile decoder), the
  // reference itself would be UB before the raw `id` read could
  // sanitize it.
  let mut out = Vec::with_capacity(count);
  for index in 0..count {
    // SAFETY: `map_ptr` points to `count == nb_channels` valid
    // `AVChannelCustom` entries per FFmpeg's contract; `index < count`,
    // so `entry_ptr` lies inside the allocation.
    let entry_ptr: *const ffi::AVChannelCustom = unsafe { map_ptr.add(index) };
    // SAFETY: `entry_ptr` is a valid pointer; `addr_of!((*p).field)`
    // computes the field address without forming a reference.
    let raw_id = unsafe { read_unaligned(addr_of!((*entry_ptr).id) as *const i32) };
    let label = unsafe { custom_channel_label_raw(entry_ptr) };
    out.push(ChannelSpec::new(index as u32, raw_id as u32).with_label(label));
  }
  out
}

/// Pointer-form of `custom_channel_label` — never forms
/// `&AVChannelCustom`, since the struct contains an enum-typed `id`.
///
/// # Safety
/// `entry_ptr` must be a live `*const AVChannelCustom`.
unsafe fn custom_channel_label_raw(entry_ptr: *const ffi::AVChannelCustom) -> SmolStr {
  use core::ptr::addr_of;
  // SAFETY: `name: [c_char; 16]` is an inline byte array — no
  // validity invariant beyond initialization (FFmpeg guarantees that).
  // `addr_of!` computes the address; we then re-interpret as `*const u8`
  // for UTF-8 lossy decoding.
  let name_ptr = unsafe { addr_of!((*entry_ptr).name) } as *const u8;
  // SAFETY: `name` is exactly 16 bytes wide.
  let bytes = unsafe { slice::from_raw_parts(name_ptr, 16) };
  let end = bytes
    .iter()
    .position(|byte| *byte == 0)
    .unwrap_or(bytes.len());
  if end == 0 {
    return SmolStr::default();
  }
  SmolStr::new(std::string::String::from_utf8_lossy(&bytes[..end]))
}

#[allow(dead_code)]
fn custom_channels(layout: &AvChannelLayout) -> Vec<ChannelSpec> {
  // Same raw-integer check as in `channel_layout_description_from_ffmpeg`:
  // never let Rust form an `AVChannelOrder` value from runtime data
  // before we've validated its discriminant.
  use core::ptr::{addr_of, read_unaligned};
  // SAFETY: `layout.0` is the inner `AVChannelLayout`; reading the
  // `order` field as `i32` matches the bindgen enum's storage.
  let order_raw = unsafe { read_unaligned(addr_of!(layout.0.order) as *const i32) };
  if order_raw != ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
    return Vec::new();
  }
  let count = layout.0.nb_channels.max(0) as usize;
  if count == 0 {
    return Vec::new();
  }
  // SAFETY: The `u` field is a union; reading `.map` is sound when
  // `order == CUSTOM` per FFmpeg's documented contract. The pointer
  // may still be null on a malformed layout — guard explicitly.
  let ptr = unsafe { layout.0.u.map };
  if ptr.is_null() {
    return Vec::new();
  }
  // SAFETY: AVChannelLayout's contract says `.u.map` points to
  // `nb_channels` valid `AVChannelCustom` entries when order == CUSTOM.
  let slice_ref = unsafe { slice::from_raw_parts(ptr, count) };
  slice_ref
    .iter()
    .enumerate()
    .map(|(index, channel)| {
      // Read `channel.id` as raw `i32` to avoid constructing an
      // invalid `AVChannel` enum from a value we don't recognize.
      // SAFETY: `channel` is a valid `&AVChannelCustom`; `id` has the
      // bindgen enum layout (c_int).
      let raw_id = unsafe { read_unaligned(addr_of!(channel.id) as *const i32) };
      ChannelSpec::new(index as u32, raw_id as u32).with_label(custom_channel_label(channel))
    })
    .collect()
}

fn custom_channel_label(channel: &ffi::AVChannelCustom) -> SmolStr {
  // SAFETY: AVChannelCustom.name is a fixed-size [c_char; 16] inline
  // buffer. Re-interpreting as bytes for UTF-8 lossy decoding is sound.
  let bytes =
    unsafe { slice::from_raw_parts(channel.name.as_ptr() as *const u8, channel.name.len()) };
  let end = bytes
    .iter()
    .position(|byte| *byte == 0)
    .unwrap_or(bytes.len());
  if end == 0 {
    return SmolStr::default();
  }
  SmolStr::new(std::string::String::from_utf8_lossy(&bytes[..end]))
}

/// Renders a layout the way FFmpeg names it (`av_channel_layout_describe`).
fn describe_layout(layout: &AvChannelLayout) -> SmolStr {
  // `av_channel_layout_describe` returns the number of bytes needed
  // (excluding the NUL terminator). Start with a 128-byte buffer —
  // comfortably bigger than every named layout — and grow once if it
  // wasn't enough. Use `c_char` for portability (signed on
  // x86/aarch64-Apple, unsigned on aarch64-Linux).
  let mut buf = std::vec![0 as c_char; 128];
  let mut needed =
    unsafe { ffi::av_channel_layout_describe(&layout.0 as *const _, buf.as_mut_ptr(), buf.len()) };
  if needed < 0 {
    return SmolStr::default();
  }
  if needed as usize >= buf.len() {
    buf.resize(needed as usize + 1, 0 as c_char);
    needed = unsafe {
      ffi::av_channel_layout_describe(&layout.0 as *const _, buf.as_mut_ptr(), buf.len())
    };
    if needed < 0 {
      return SmolStr::default();
    }
  }
  // SAFETY: buf is heap-allocated, NUL-terminated by FFmpeg's contract.
  let bytes = unsafe { slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
  let end = bytes
    .iter()
    .position(|byte| *byte == 0)
    .unwrap_or(needed as usize);
  if end == 0 {
    return SmolStr::default();
  }
  SmolStr::new(std::string::String::from_utf8_lossy(&bytes[..end]))
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Builds a NATIVE-order layout from a channel mask, the way a decoder
  /// hands one over. This is how a layout `ffmpeg_next` mints no constant
  /// for can be reached at all: `av_channel_layout_from_mask` fills in the
  /// order and the channel count, so nothing about the value is
  /// hand-forged.
  fn native(mask: u64) -> AvChannelLayout {
    // SAFETY: an all-zero `AVChannelLayout` is `AV_CHANNEL_ORDER_UNSPEC`
    // with no channels — a valid value, and the same starting point
    // `ffmpeg_next`'s own `ChannelLayout::default` uses. The constructor
    // then overwrites every field.
    let mut raw: ffi::AVChannelLayout = unsafe { core::mem::zeroed() };
    // SAFETY: `raw` is a live, writable `AVChannelLayout`.
    let rc = unsafe { ffi::av_channel_layout_from_mask(&mut raw, mask) };
    assert_eq!(rc, 0, "av_channel_layout_from_mask({mask:#x}) failed");
    AvChannelLayout(raw)
  }

  /// `AV_CH_LAYOUT_BINAURAL`, spelled the way the FFmpeg header spells it
  /// (`1ULL << AV_CHAN_BINAURAL_*`). The composed `AV_CH_BINAURAL_LEFT` /
  /// `_RIGHT` macros do not survive bindgen's macro evaluation, but the
  /// `AVChannel` enum they shift by does, so the mask is still derived
  /// from FFmpeg's own numbers rather than typed out.
  fn binaural_mask() -> u64 {
    (1u64 << ffi::AVChannel::AV_CHAN_BINAURAL_LEFT as u64)
      | (1u64 << ffi::AVChannel::AV_CHAN_BINAURAL_RIGHT as u64)
  }

  /// The three layouts [`ChannelLayout`] names but `ffmpeg_next` 9.0.0
  /// mints no constant for. The constant table cannot reach them by
  /// construction; the describe rung does, because FFmpeg's own layout
  /// map names all three and the vocabulary reads that word.
  #[test]
  fn orphan_layouts_are_named_through_the_describe_rung() {
    let cases = [
      (binaural_mask(), "binaural", ChannelLayout::Binaural),
      (
        ffi::AV_CH_LAYOUT_5POINT1 | ffi::AV_CH_TOP_FRONT_LEFT | ffi::AV_CH_TOP_FRONT_RIGHT,
        "5.1.2",
        ChannelLayout::Ch5_1_2,
      ),
      (
        ffi::AV_CH_LAYOUT_9POINT1POINT4_BACK | ffi::AV_CH_TOP_SIDE_LEFT | ffi::AV_CH_TOP_SIDE_RIGHT,
        "9.1.6",
        ChannelLayout::Ch9_1_6,
      ),
    ];
    for (mask, slug, expected) in cases {
      let layout = native(mask);
      assert_eq!(
        mapped_constant(&layout),
        None,
        "{slug} must fall off the constant table — that is what makes it an orphan"
      );
      assert_eq!(
        describe_layout(&layout).as_str(),
        slug,
        "FFmpeg must name {slug} for the rung to have a word to read"
      );
      assert_eq!(
        channel_layout_from_ffmpeg(&layout),
        expected,
        "{slug} must reach its named variant through the rung"
      );

      let described = channel_layout_description_from_ffmpeg(&layout);
      assert_eq!(
        described.known_kind(),
        &expected,
        "{slug} must be named on the description path too"
      );
      assert_eq!(described.text(), slug, "{slug} rendering rides `text`");
    }
  }

  /// FFmpeg 9's actual `5.1.4`: the *side*-surround mask
  /// (`FL+FR+FC+LFE+SL+SR` plus the four heights), which its layout map
  /// names and no constant here reaches.
  ///
  /// `ffmpeg_sys_next` 9.0.0 bundles a `channel_layout_fixed.h` that
  /// `#undef`s FFmpeg's layout macros and re-declares them as C
  /// constants, and its `AV_CH_LAYOUT_5POINT1POINT4_BACK` still carries
  /// FFmpeg 8's *back*-surround formula. So `ffmpeg_next`'s
  /// `_5POINT1POINT4_BACK` constant — the one the table compares against
  /// — is a mask FFmpeg 9 no longer names, and the mask FFmpeg 9 *does*
  /// name has no constant at all. This is the ruling's "a layout the
  /// vocabulary already names is reachable with zero adapter edits",
  /// arriving earlier than expected.
  ///
  /// Asserted through the public entry point alone, deliberately: if the
  /// upstream shim is ever refreshed the constant table will start
  /// answering this mask itself, and `5.1.4` must come out named either
  /// way.
  #[test]
  fn ffmpeg_nines_own_5_1_4_is_named() {
    let layout = native(
      ffi::AV_CH_LAYOUT_5POINT1
        | ffi::AV_CH_TOP_FRONT_LEFT
        | ffi::AV_CH_TOP_FRONT_RIGHT
        | ffi::AV_CH_TOP_BACK_LEFT
        | ffi::AV_CH_TOP_BACK_RIGHT,
    );
    assert_eq!(describe_layout(&layout).as_str(), "5.1.4");
    assert_eq!(
      channel_layout_from_ffmpeg(&layout),
      ChannelLayout::Ch5_1_4Back
    );
  }

  /// The constant table is the first rung and answers alone.
  ///
  /// `Some` here *is* the bypass proof: [`channel_layout_from_ffmpeg`] is
  /// `mapped_constant(..).unwrap_or_else(<describe rung>)`, and
  /// `unwrap_or_else` does not evaluate its closure on `Some` — so a
  /// mapped constant never renders, never parses, and cannot be
  /// re-answered by a word.
  ///
  /// The sample is the crossed-slug family (where FFmpeg qualifies the
  /// *side* layout in one place and the *back* one in another, so a
  /// name-based answer is the one that could plausibly differ), plus the
  /// `_7POINT1_TOP_BACK` alias that shares `_5POINT1POINT2_BACK`'s mask
  /// and therefore has no arm of its own.
  #[test]
  fn mapped_constants_are_answered_by_the_table_alone() {
    let table = [
      ("MONO", AvChannelLayout::MONO, ChannelLayout::Mono),
      ("STEREO", AvChannelLayout::STEREO, ChannelLayout::Stereo),
      (
        "STEREO_DOWNMIX",
        AvChannelLayout::STEREO_DOWNMIX,
        ChannelLayout::StereoDownmix,
      ),
      ("SURROUND", AvChannelLayout::SURROUND, ChannelLayout::Ch3_0),
      ("_5POINT0", AvChannelLayout::_5POINT0, ChannelLayout::Ch5_0),
      (
        "_5POINT0_BACK",
        AvChannelLayout::_5POINT0_BACK,
        ChannelLayout::Ch5_0Back,
      ),
      ("_5POINT1", AvChannelLayout::_5POINT1, ChannelLayout::Ch5_1),
      (
        "_5POINT1_BACK",
        AvChannelLayout::_5POINT1_BACK,
        ChannelLayout::Ch5_1Back,
      ),
      (
        "_5POINT1POINT2_BACK",
        AvChannelLayout::_5POINT1POINT2_BACK,
        ChannelLayout::Ch5_1_2Back,
      ),
      (
        "_7POINT1_TOP_BACK",
        AvChannelLayout::_7POINT1_TOP_BACK,
        ChannelLayout::Ch5_1_2Back,
      ),
      (
        "_7POINT1_WIDE",
        AvChannelLayout::_7POINT1_WIDE,
        ChannelLayout::Ch7_1Wide,
      ),
      (
        "_7POINT1_WIDE_BACK",
        AvChannelLayout::_7POINT1_WIDE_BACK,
        ChannelLayout::Ch7_1WideBack,
      ),
      (
        "_22POINT2",
        AvChannelLayout::_22POINT2,
        ChannelLayout::Ch22_2,
      ),
    ];
    for (name, layout, expected) in table {
      assert_eq!(
        mapped_constant(&layout),
        Some(expected.clone()),
        "{name} must be answered by the constant table, not by a rendering"
      );
      assert_eq!(channel_layout_from_ffmpeg(&layout), expected, "{name}");
    }
  }

  /// A layout nobody names stays *absent*. The rung upgrades the sentinel
  /// to a named variant or leaves it alone; it never smuggles FFmpeg's
  /// rendering into `known_kind`'s escape, because `text` already carries
  /// that rendering verbatim.
  #[test]
  fn an_unnamed_layout_stays_absent_with_its_rendering_in_text() {
    // FL+FR+TFL: a native mask FFmpeg's layout map does not carry, so
    // `av_channel_layout_describe` falls back to listing the channels.
    let layout = native(ffi::AV_CH_FRONT_LEFT | ffi::AV_CH_FRONT_RIGHT | ffi::AV_CH_TOP_FRONT_LEFT);
    assert_eq!(mapped_constant(&layout), None);

    let rendering = describe_layout(&layout);
    assert!(
      rendering.contains("TFL"),
      "FFmpeg should list the channels it cannot name: {rendering:?}"
    );
    assert_eq!(
      channel_layout_from_ffmpeg(&layout),
      ChannelLayout::default(),
      "an unnamed layout must land on the absent sentinel"
    );

    let described = channel_layout_description_from_ffmpeg(&layout);
    assert_eq!(described.known_kind(), &ChannelLayout::default());
    assert_eq!(
      described.text(),
      rendering.as_str(),
      "the rendering is what `text` carries"
    );
  }

  /// The rung itself, on describe-shaped strings — the half of the door
  /// that needs no `AVChannelLayout` to exercise.
  #[test]
  fn the_describe_rung_reads_names_and_refuses_everything_else() {
    // The three orphans, as words.
    assert_eq!(
      channel_layout_from_describe("binaural"),
      ChannelLayout::Binaural
    );
    assert_eq!(
      channel_layout_from_describe("5.1.2"),
      ChannelLayout::Ch5_1_2
    );
    assert_eq!(
      channel_layout_from_describe("9.1.6"),
      ChannelLayout::Ch9_1_6
    );
    // The crossed slugs: unqualified `5.1` is the *back* layout and the
    // side one is qualified, so reading the word is the only way to tell
    // these two apart.
    assert_eq!(
      channel_layout_from_describe("5.1"),
      ChannelLayout::Ch5_1Back
    );
    assert_eq!(
      channel_layout_from_describe("5.1(side)"),
      ChannelLayout::Ch5_1
    );
    // Case folding is the vocabulary's, not ours.
    assert_eq!(
      channel_layout_from_describe("BINAURAL"),
      ChannelLayout::Binaural
    );

    // Everything else is absent — never `Other(<the rendering>)`.
    for unnamed in [
      "",
      "3 channels",
      "3 channels (FL+FR+TFL)",
      "FL@Left+FR@Right",
      "ambisonic 2",
      "not-a-layout",
    ] {
      assert_eq!(
        channel_layout_from_describe(unnamed),
        ChannelLayout::default(),
        "{unnamed:?} must stay absent"
      );
    }
  }
}
