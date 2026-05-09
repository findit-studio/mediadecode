//! Conversions from FFmpeg's [`ffmpeg_next::ChannelLayout`] /
//! [`ffmpeg_next::ffi::AVChannelOrder`] to the channel-layout types
//! mediadecode owns ([`mediadecode::channel::ChannelLayoutKind`],
//! [`mediadecode::channel::AudioChannelOrderKind`],
//! [`mediadecode::channel::AudioChannelSpec`],
//! [`mediadecode::channel::AudioChannelLayout`]).
//!
//! These live as **free functions** (not `From` trait impls) because of
//! Rust's orphan rule: this crate owns neither `From` nor
//! `mediadecode::channel::*`, so we can't write the `impl` here. Calling
//! `mediadecode_ffmpeg::audio_channel_layout_from_ffmpeg(layout)` is the
//! ergonomic boundary instead.

use core::{ffi::c_char, slice};

use ffmpeg_next::{ChannelLayout, ffi};
use mediadecode::channel::{
  AudioChannelLayout, AudioChannelOrderKind, AudioChannelSpec, ChannelLayoutKind,
};
use smol_str::SmolStr;
use std::vec::Vec;

/// Maps an FFmpeg [`ChannelLayout`] to the high-level
/// [`ChannelLayoutKind`] tag.
///
/// Returns [`ChannelLayoutKind::Unknown`] for layouts that don't match
/// one of FFmpeg's named-layout constants.
pub fn channel_layout_kind_from_ffmpeg(value: &ChannelLayout) -> ChannelLayoutKind {
  match () {
    () if value.eq(&ChannelLayout::MONO) => ChannelLayoutKind::Mono,
    () if value.eq(&ChannelLayout::STEREO) => ChannelLayoutKind::Stereo,
    () if value.eq(&ChannelLayout::STEREO_DOWNMIX) => ChannelLayoutKind::StereoDownmix,
    () if value.eq(&ChannelLayout::SURROUND) => ChannelLayoutKind::Surround,
    () if value.eq(&ChannelLayout::QUAD) => ChannelLayoutKind::Quad,
    () if value.eq(&ChannelLayout::HEXAGONAL) => ChannelLayoutKind::Hexagonal,
    () if value.eq(&ChannelLayout::OCTAGONAL) => ChannelLayoutKind::Octagonal,
    () if value.eq(&ChannelLayout::HEXADECAGONAL) => ChannelLayoutKind::Hexadecagonal,
    () if value.eq(&ChannelLayout::CUBE) => ChannelLayoutKind::Cube,
    () if value.eq(&ChannelLayout::_2POINT1) => ChannelLayoutKind::Ch2_1,
    () if value.eq(&ChannelLayout::_2_1) => ChannelLayoutKind::Ch2_1Alt,
    () if value.eq(&ChannelLayout::_2_2) => ChannelLayoutKind::Ch2_2,
    () if value.eq(&ChannelLayout::_3POINT1) => ChannelLayoutKind::Ch3_1,
    () if value.eq(&ChannelLayout::_3POINT1POINT2) => ChannelLayoutKind::Ch3_1_2,
    () if value.eq(&ChannelLayout::_4POINT0) => ChannelLayoutKind::Ch4_0,
    () if value.eq(&ChannelLayout::_4POINT1) => ChannelLayoutKind::Ch4_1,
    () if value.eq(&ChannelLayout::_5POINT0) => ChannelLayoutKind::Ch5_0,
    () if value.eq(&ChannelLayout::_5POINT0_BACK) => ChannelLayoutKind::Ch5_0Back,
    () if value.eq(&ChannelLayout::_5POINT1) => ChannelLayoutKind::Ch5_1,
    () if value.eq(&ChannelLayout::_5POINT1_BACK) => ChannelLayoutKind::Ch5_1Back,
    () if value.eq(&ChannelLayout::_5POINT1POINT2_BACK) => ChannelLayoutKind::Ch5_1_2Back,
    () if value.eq(&ChannelLayout::_5POINT1POINT4_BACK) => ChannelLayoutKind::Ch5_1_4Back,
    () if value.eq(&ChannelLayout::_6POINT0) => ChannelLayoutKind::Ch6_0,
    () if value.eq(&ChannelLayout::_6POINT0_FRONT) => ChannelLayoutKind::Ch6_0Front,
    () if value.eq(&ChannelLayout::_6POINT1) => ChannelLayoutKind::Ch6_1,
    () if value.eq(&ChannelLayout::_6POINT1_BACK) => ChannelLayoutKind::Ch6_1Back,
    () if value.eq(&ChannelLayout::_6POINT1_FRONT) => ChannelLayoutKind::Ch6_1Front,
    () if value.eq(&ChannelLayout::_7POINT0) => ChannelLayoutKind::Ch7_0,
    () if value.eq(&ChannelLayout::_7POINT0_FRONT) => ChannelLayoutKind::Ch7_0Front,
    () if value.eq(&ChannelLayout::_7POINT1) => ChannelLayoutKind::Ch7_1,
    () if value.eq(&ChannelLayout::_7POINT1_WIDE) => ChannelLayoutKind::Ch7_1Wide,
    () if value.eq(&ChannelLayout::_7POINT1_WIDE_BACK) => ChannelLayoutKind::Ch7_1WideBack,
    () if value.eq(&ChannelLayout::_7POINT1_TOP_BACK) => ChannelLayoutKind::Ch7_1TopBack,
    () if value.eq(&ChannelLayout::_7POINT1POINT2) => ChannelLayoutKind::Ch7_1_2,
    () if value.eq(&ChannelLayout::_7POINT1POINT4_BACK) => ChannelLayoutKind::Ch7_1_4Back,
    () if value.eq(&ChannelLayout::_7POINT2POINT3) => ChannelLayoutKind::Ch7_2_3,
    () if value.eq(&ChannelLayout::_9POINT1POINT4_BACK) => ChannelLayoutKind::Ch9_1_4Back,
    () if value.eq(&ChannelLayout::_22POINT2) => ChannelLayoutKind::Ch22_2,
    () => ChannelLayoutKind::Unknown,
  }
}

/// Maps FFmpeg's [`AVChannelOrder`](ffi::AVChannelOrder) to the
/// [`AudioChannelOrderKind`] tag.
pub fn audio_channel_order_kind_from_ffmpeg(value: ffi::AVChannelOrder) -> AudioChannelOrderKind {
  // Compare via integer rather than enum-matching: the caller often
  // sources `value` from raw FFmpeg memory (`AVChannelLayout.order`),
  // and an unknown variant would already be UB before reaching this
  // function. Going through `as i32` here is sound because the caller
  // is responsible for the up-conversion path; for the raw-pointer
  // path use [`audio_channel_order_kind_from_raw`].
  audio_channel_order_kind_from_raw(value as i32)
}

/// Variant of [`audio_channel_order_kind_from_ffmpeg`] that takes the
/// raw integer directly. Use this when the caller has just read
/// `AVChannelLayout.order` from FFmpeg memory and doesn't want to
/// risk constructing an invalid bindgen enum value first.
pub fn audio_channel_order_kind_from_raw(raw: i32) -> AudioChannelOrderKind {
  match raw {
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 => AudioChannelOrderKind::Native,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 => AudioChannelOrderKind::Custom,
    x if x == ffi::AVChannelOrder::AV_CHANNEL_ORDER_AMBISONIC as i32 => {
      AudioChannelOrderKind::Ambisonic
    }
    _ => AudioChannelOrderKind::Unspecified,
  }
}

/// Builds a fully-populated [`AudioChannelLayout`] from an FFmpeg
/// [`ChannelLayout`].
///
/// - Native / Ambisonic layouts populate `native_mask` from
///   [`ChannelLayout::bits`] (clearing it to `None` if zero).
/// - Custom layouts populate `custom_channels` from FFmpeg's per-channel
///   list (`AVChannelLayout.u.map`), with each label drawn from
///   `AVChannelCustom.name`.
/// - `description` carries the result of `av_channel_layout_describe`
///   (FFmpeg's human-readable rendering — e.g. `"5.1(side)"`).
pub fn audio_channel_layout_from_ffmpeg(value: &ChannelLayout) -> AudioChannelLayout {
  // SAFETY: `value` is a live reference; the inner `AVChannelLayout`
  // stays valid for the duration of this call. We hand the raw
  // address into the pointer-based variant which is the canonical
  // implementation (avoids forming `&AVChannelLayout` over a
  // potentially-invalid `order` discriminant).
  unsafe { audio_channel_layout_from_raw_ptr(&value.0 as *const ffi::AVChannelLayout) }
}

/// Pointer variant of [`audio_channel_layout_from_ffmpeg`]. Safe-API
/// callers that already hold a `&ChannelLayout` should prefer that
/// function; the pointer form exists so the convert path
/// (which never forms `&AVFrame`) can pass `addr_of!((*av_frame).ch_layout)`
/// straight through without materializing a typed reference.
///
/// # Safety
/// `ptr` must be a live `*const AVChannelLayout` for the duration of
/// this call. The function reads `order` raw, then `nb_channels`,
/// then either `u.mask` (NATIVE / AMBISONIC) or `u.map`
/// (CUSTOM) — only after the order discriminant has been validated.
/// It never forms a `&AVChannelLayout` reference.
pub unsafe fn audio_channel_layout_from_raw_ptr(
  ptr: *const ffi::AVChannelLayout,
) -> AudioChannelLayout {
  use core::ptr::{addr_of, read_unaligned};
  // Read `order` as a raw integer first — never let Rust assume
  // the field is a valid `AVChannelOrder`.
  // SAFETY: `ptr` is a valid `*const AVChannelLayout`; `addr_of!`
  // computes the field address without forming a reference; reading
  // as `i32` matches the bindgen enum's `c_int` storage.
  let order_raw = unsafe { read_unaligned(addr_of!((*ptr).order) as *const i32) };
  let order = audio_channel_order_kind_from_raw(order_raw);
  let nb_channels = unsafe { (*ptr).nb_channels };

  // Native / Ambisonic carry the bitmask in the union. Only read
  // `u.mask` after the order is validated so we don't trip on an
  // unknown order writing into a future variant of the union.
  let native_mask = match order {
    AudioChannelOrderKind::Native | AudioChannelOrderKind::Ambisonic => {
      // SAFETY: `u.mask` is the union variant for NATIVE/AMBISONIC.
      let mask = unsafe { (*ptr).u.mask };
      if mask != 0 { Some(mask) } else { None }
    }
    _ => None,
  };

  // Build kind / description through ffmpeg-next helpers. They take
  // `&ChannelLayout` (which is `repr(transparent)` over
  // `AVChannelLayout`), but at this point we've already validated
  // `order`, so forming the reference is sound: the only enum-typed
  // field in `AVChannelLayout` is `order`, and it now holds a value
  // that came back from `audio_channel_order_kind_from_raw` with the
  // unknown bucket folded into a known variant — but the *underlying
  // struct* still has the original raw bytes. We can't form `&AVChannelLayout`
  // over an unknown order without UB, so for those helpers we
  // explicitly only call them when order is one of the known variants.
  let known_kind = if matches!(order, AudioChannelOrderKind::Unspecified) {
    ChannelLayoutKind::Unknown
  } else {
    // SAFETY: `order` is one of {Native, Custom, Ambisonic} — all of
    // which are valid `AVChannelOrder` discriminants present in our
    // bindgen output, so `&*ptr` is sound to form here.
    let layout_ref = unsafe { &*(ptr as *const ChannelLayout) };
    channel_layout_kind_from_ffmpeg(layout_ref)
  };
  let custom_channels_vec = unsafe { custom_channels_raw(ptr, order) };
  let description = if matches!(order, AudioChannelOrderKind::Unspecified) {
    SmolStr::default()
  } else {
    // SAFETY: same as above — `order` is a known, valid discriminant.
    let layout_ref = unsafe { &*(ptr as *const ChannelLayout) };
    describe_layout(layout_ref)
  };

  AudioChannelLayout::new(nb_channels.max(0) as u32)
    .with_order(order)
    .with_known_kind(known_kind)
    .with_native_mask(native_mask)
    .with_custom_channels(custom_channels_vec)
    .with_description(description)
}

/// Pointer-form of `custom_channels`. `order` must be the result of
/// reading `(*ptr).order` as `i32` and folding through
/// [`audio_channel_order_kind_from_raw`]; this skips re-reading it.
///
/// # Safety
/// `ptr` must be a live `*const AVChannelLayout`. Reads only fields
/// (`u.map`, `nb_channels`, and the per-channel array) — no `&AVChannelLayout`
/// reference is ever formed.
unsafe fn custom_channels_raw(
  ptr: *const ffi::AVChannelLayout,
  order: AudioChannelOrderKind,
) -> Vec<AudioChannelSpec> {
  use core::ptr::{addr_of, read_unaligned};
  if !matches!(order, AudioChannelOrderKind::Custom) {
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
    out.push(AudioChannelSpec::new(index as u32, raw_id as u32).with_label(label));
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
fn custom_channels(layout: &ChannelLayout) -> Vec<AudioChannelSpec> {
  // Same raw-integer check as in `audio_channel_layout_from_ffmpeg`:
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
      AudioChannelSpec::new(index as u32, raw_id as u32).with_label(custom_channel_label(channel))
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

fn describe_layout(layout: &ChannelLayout) -> SmolStr {
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
