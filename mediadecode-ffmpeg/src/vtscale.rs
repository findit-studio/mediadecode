//! The GPU-side scaled-output stage: one `VTPixelTransferSession`
//! between the hardware frame and the CPU download.
//!
//! # What this is, and what it deliberately is not
//!
//! FFmpeg's VideoToolbox hwaccel decode is **untouched** by this module.
//! The decode still runs through the generic `av_hwdevice_ctx_create` +
//! `get_format` road every [`crate::Backend`] takes, and it still
//! produces a full-coded-size `CVPixelBuffer` — inter prediction needs
//! full-resolution reference frames, so every road decodes full size
//! internally and no design can change that.
//!
//! What this stage changes is the **crossing**. Between the decoded
//! hardware frame and `av_hwframe_transfer_data` it inserts a
//! `VTPixelTransferSession`, which resizes one `CVPixelBuffer` into
//! another on the GPU. The frame that then crosses to the CPU is the
//! fitted one, so a 4K stream fitted to a 512-class box moves roughly
//! thirty times fewer bytes over that bus and allocates a CPU frame of
//! the same reduced size.
//!
//! A caller-owned `VTDecompressionSession` with its own
//! `destinationImageBufferAttributes` — decode and scale in one session,
//! bypassing FFmpeg's hwaccel negotiation entirely — remains
//! [mediadecode#55](https://github.com/findit-studio/mediadecode/issues/55)'s
//! standing future enhancement. It saves the same transfer bandwidth
//! this stage saves, at the cost of a second, parallel VideoToolbox
//! integration path and an async output-callback trampoline; the reopen
//! trigger is a measurement showing a gap this stage cannot close.
//!
//! # The destination buffers are FFmpeg's, not ours
//!
//! The one design choice that decides this module's safety profile: the
//! fitted `CVPixelBuffer` is **never** allocated, retained or released
//! by this crate. A second `AVHWFramesContext` is built over the decode
//! session's own VideoToolbox device at the fitted size, and every frame
//! comes out of it through `av_hwframe_get_buffer` — so the pixel buffer
//! arrives owned by an `AVFrame`, recycled through libavutil's pool, and
//! released by `av_frame_unref` exactly as the decoder's own frames are.
//! The only Core Foundation object this crate owns is the transfer
//! session itself.
//!
//! # The stage stands down rather than failing
//!
//! Scaled output is an opt-in bandwidth trade, so nothing in it may
//! break a decode. Every condition this stage cannot honor — a source
//! frame that is not a VideoToolbox surface, a pixel buffer whose extent
//! does not match the frame's, a standing request that a mid-stream
//! resolution change turned into an upscale, a session or pool that
//! fails to build, a transfer that returns an error — makes it *stand
//! down* for that frame: the full-size hardware frame goes to the CPU
//! download untouched, exactly as it did before this module existed.
//! There is no new error variant and no new failure mode on the decode
//! road.
//!
//! # Which builds have a stage at all
//!
//! **The target, and nothing else.**
//! `VTPixelTransferSessionCreate` / `Invalidate` / `TransferImage` are
//! `API_AVAILABLE(macos(10.8), ios(16.0), tvos(16.0), visionos(1.0))`
//! and `API_UNAVAILABLE(watchos)`. Rust has no equivalent of clang's
//! availability attributes and no weak-import spelling for a plain
//! `extern "C"` declaration, so a binary built for an iOS or tvOS
//! deployment target below 16.0 would carry a **strong** import of a
//! symbol its runtime does not export — a dynamic-loader failure at
//! launch, before any of this module's careful stand-down logic could
//! answer `Unsupported`. macOS 10.8 and visionOS 1.0 are below every
//! deployment target Rust supports on those platforms, so those two are
//! the whole list; reaching iOS/tvOS 16 and above would need a
//! `dlopen`/`dlsym` resolution path. Everything else takes
//! `elsewhere`'s body and answers `Unsupported`, as does a build that
//! sets `MEDIADECODE_FFMPEG_NO_VIDEOTOOLBOX`.
//!
//! # Why the FFmpeg build is not a second question
//!
//! Because this module is careful to name no FFmpeg symbol that one
//! could take away. The `AVHWFramesContext` it builds and the frames it
//! draws are generic libavutil; the VideoToolbox-specific calls are
//! Apple's own. In particular it does **not** steer the destination
//! pool's pixel format through `AVVTFramesContext.color_range` —
//! FFmpeg compiles that type only under `CONFIG_VIDEOTOOLBOX`, and
//! depending on it would put a build script in the business of
//! predicting what `bindgen` will generate from headers it has not
//! seen, across include overlays, cross sysroots and
//! `BINDGEN_EXTRA_CLANG_ARGS`. There is no honest way to win that, so
//! the module does not enter it: it *verifies* the pool's format on the
//! buffer it is about to write instead, which is both a stronger
//! guarantee and one that needs nothing from the build.
//!
//! An FFmpeg compiled without VideoToolbox is then handled where it
//! belongs — at run time. No VideoToolbox device opens, the session
//! takes another backend or software, and the stage never sees a frame.

#[cfg(all(ffmpeg_videotoolbox, any(target_os = "macos", target_os = "visionos")))]
#[path = "vtscale/videotoolbox.rs"]
mod imp;

#[cfg(not(all(ffmpeg_videotoolbox, any(target_os = "macos", target_os = "visionos"))))]
#[path = "vtscale/elsewhere.rs"]
mod imp;

pub(crate) use imp::ScaledOutput;

#[cfg(test)]
mod tests;
