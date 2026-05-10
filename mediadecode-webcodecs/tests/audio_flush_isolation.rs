//! Regression test for codex round 4: audio FIFO matching
//! across `flush()` boundaries.
//!
//! The audio adapter's output callback FIFO-pops the oldest
//! pending side-map record because every WebCodecs audio
//! codec is non-reordering, and Chrome's PCM decoders rebase
//! `AudioData.timestamp` so exact-id lookup wouldn't work
//! after the first chunk anyway. Codex flagged the cross-
//! flush case: a pre-flush callback that survives
//! `decoder.reset()` for any reason could FIFO-pop a *post-
//! flush* pending record, attach stale audio to a fresh
//! packet's PTS, and decrement `pending_input_bytes` for
//! bytes the JS decoder may still be holding.
//!
//! Defense-in-depth: `flush()` rebuilds the entire decoder
//! plus its callback closures. The old `Closure` values are
//! dropped when their fields are reassigned, and
//! `wasm-bindgen` invalidates the JS wrapper of every
//! dropped Rust `Closure`. Any pre-flush callback that
//! managed to be queued in the JS event loop before reset
//! would invoke a dropped wrapper and throw — observable
//! but not corrupting our state.
//!
//! This test verifies the post-flush correctness contract:
//! after a flush, the next decoded frame carries the PTS the
//! consumer stamped on the first post-flush packet — not a
//! recycled value from the pre-flush stream.
#![cfg(target_arch = "wasm32")]

use mediadecode::{
  Timebase, Timestamp,
  future::local::AudioStreamDecoder,
  packet::{AudioPacket, PacketFlags},
};
use mediadecode_webcodecs::{
  AudioDecodeError, AudioPacketExtra, WebCodecsAudioStreamDecoder, WebCodecsBuffer,
  empty_audio_frame,
};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u8 = 1;
const SAMPLES_PER_CHUNK: usize = 1024;

/// Fabricate a `pcm-s16` packet with the requested PTS.
/// Body is silence (zero samples), matching the `pcm_s16le`
/// audio-fixtures shape so Chrome's PCM decoder accepts it
/// through the same path the fixture suite exercises.
fn silent_pcm_s16_packet(
  pts_samples: i64,
  time_base: Timebase,
) -> AudioPacket<AudioPacketExtra, WebCodecsBuffer> {
  let body = vec![0u8; SAMPLES_PER_CHUNK * 2 * CHANNELS as usize];
  AudioPacket::new(
    WebCodecsBuffer::from_bytes(body),
    AudioPacketExtra::new(true),
  )
  .with_flags(PacketFlags::KEY)
  .with_pts(Some(Timestamp::new(pts_samples, time_base)))
}

#[wasm_bindgen_test]
async fn flush_isolates_pre_and_post_flush_streams() {
  let time_base = Timebase::new(
    1,
    core::num::NonZeroU32::new(SAMPLE_RATE).expect("non-zero rate"),
  );

  let mut decoder = WebCodecsAudioStreamDecoder::open_with_codec_string(
    "pcm-s16",
    None,
    SAMPLE_RATE,
    CHANNELS,
    time_base,
  )
  .expect("open pcm-s16 decoder");

  // --- Phase 1: pre-flush stream ---
  //
  // Send four packets with PTS 0, 1024, 2048, 3072 (sample-
  // domain). Drain the frames as they come out. Every PTS we
  // observe must be one of the four we stamped — no leakage
  // from a hypothetical "next" generation that doesn't exist
  // yet.
  let mut frame = empty_audio_frame();
  let pre_flush_ptses: [i64; 4] = [0, 1024, 2048, 3072];
  for &pts in &pre_flush_ptses {
    decoder
      .send_packet(&silent_pcm_s16_packet(pts, time_base))
      .await
      .expect("pre-flush send_packet");
    loop {
      match decoder.receive_frame(&mut frame).await {
        Ok(()) => {
          let observed = frame.pts().expect("pre-flush frame pts is Some").pts();
          assert!(
            pre_flush_ptses.contains(&observed),
            "pre-flush frame pts {observed} not in {pre_flush_ptses:?}"
          );
        }
        Err(AudioDecodeError::NoFrameReady) => break,
        Err(e) => panic!("pre-flush receive_frame: {e:?}"),
      }
    }
  }

  // --- Flush boundary ---
  //
  // After this point any pre-flush callback the runtime might
  // have buffered must NOT consume a post-flush record.
  decoder.flush().await.expect("flush succeeded");

  // --- Phase 2: post-flush stream ---
  //
  // Use a PTS namespace that is *disjoint* from the pre-flush
  // values. If the cross-flush race codex described still
  // existed (a stale callback popping the new record), the
  // first post-flush frame would carry one of the post-flush
  // ptses but the decoded *audio* would correspond to a pre-
  // flush chunk — observable here as the frame appearing at
  // all (we'd see at least one) and carrying a post-flush PTS
  // (the `is_post_flush_pts` check) while the underlying
  // stream silently corrupts. The check that's actually
  // load-bearing for the regression: every observed PTS must
  // be in the post-flush set, and the *number* of post-flush
  // frames we receive must equal the number of post-flush
  // packets we sent (no frame disappeared into a stale
  // callback's record-stealing path).
  let post_flush_ptses: [i64; 4] = [1_000_000, 1_001_024, 1_002_048, 1_003_072];
  let mut post_flush_frames_observed: u32 = 0;
  for &pts in &post_flush_ptses {
    decoder
      .send_packet(&silent_pcm_s16_packet(pts, time_base))
      .await
      .expect("post-flush send_packet");
    loop {
      match decoder.receive_frame(&mut frame).await {
        Ok(()) => {
          let observed = frame.pts().expect("post-flush frame pts is Some").pts();
          assert!(
            post_flush_ptses.contains(&observed),
            "post-flush frame pts {observed} bled in from outside the \
             post-flush namespace ({post_flush_ptses:?}); the cross-flush \
             record-stealing race appears to have re-emerged",
          );
          post_flush_frames_observed = post_flush_frames_observed.saturating_add(1);
        }
        Err(AudioDecodeError::NoFrameReady) => break,
        Err(e) => panic!("post-flush receive_frame: {e:?}"),
      }
    }
  }

  // Drain the decoder so a tail packet whose output didn't
  // surface during the loop above still gets counted before
  // EOF — Chrome's PCM decoder emits one AudioData per chunk
  // synchronously enough that the loop usually catches them
  // all, but we don't want to depend on that here.
  decoder.send_eof().await.expect("post-flush send_eof");
  loop {
    match decoder.receive_frame(&mut frame).await {
      Ok(()) => {
        let observed = frame.pts().expect("eof-drain frame pts").pts();
        assert!(
          post_flush_ptses.contains(&observed),
          "eof-drain frame pts {observed} outside post-flush set {post_flush_ptses:?}"
        );
        post_flush_frames_observed = post_flush_frames_observed.saturating_add(1);
      }
      Err(AudioDecodeError::Eof) => break,
      Err(AudioDecodeError::NoFrameReady) => continue,
      Err(e) => panic!("eof-drain receive_frame: {e:?}"),
    }
  }

  assert_eq!(
    post_flush_frames_observed,
    post_flush_ptses.len() as u32,
    "post-flush frame count drift: expected {} frames carrying the four \
     post-flush ptses, observed {post_flush_frames_observed}",
    post_flush_ptses.len(),
  );
}
