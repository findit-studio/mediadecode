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

use core::{ffi::c_char, slice};

use ffmpeg_next::{ChannelLayout as AvChannelLayout, ffi};
use mediaframe::audio::{ChannelLayout, ChannelLayoutDescription, ChannelOrder, ChannelSpec};
use smol_str::SmolStr;
use std::vec::Vec;

/// Maps an FFmpeg [`AvChannelLayout`] to the named
/// [`ChannelLayout`] vocabulary.
///
/// Returns [`ChannelLayout::default`] — the `Other("")` absent sentinel
/// — for layouts that don't match one of FFmpeg's named-layout
/// constants.
///
/// The arm list is exactly `ffmpeg_next`'s `ChannelLayout` constant set:
/// its `_7POINT1_TOP_BACK` is a `#define` alias of
/// `AV_CH_LAYOUT_5POINT1POINT2_BACK` and so has no arm of its own, and
/// its `BINAURAL` / `_5POINT1POINT2` / `_9POINT1POINT6` siblings — which
/// [`ChannelLayout`] does name — have no constant to match against.
pub fn channel_layout_from_ffmpeg(value: &AvChannelLayout) -> ChannelLayout {
  match () {
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
    () => ChannelLayout::default(),
  }
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
  let known_kind = if matches!(order, ChannelOrder::Unspecified) {
    ChannelLayout::default()
  } else {
    // SAFETY: `order` is one of {Native, Custom, Ambisonic} — all of
    // which are valid `AVChannelOrder` discriminants present in our
    // bindgen output, so `&*ptr` is sound to form here.
    let layout_ref = unsafe { &*(ptr as *const AvChannelLayout) };
    channel_layout_from_ffmpeg(layout_ref)
  };
  let custom_channels_vec = unsafe { custom_channels_raw(ptr, order) };
  let text = if matches!(order, ChannelOrder::Unspecified) {
    SmolStr::default()
  } else {
    // SAFETY: same as above — `order` is a known, valid discriminant.
    let layout_ref = unsafe { &*(ptr as *const AvChannelLayout) };
    describe_layout(layout_ref)
  };

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
