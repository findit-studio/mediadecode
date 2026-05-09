# hwdecode

Cross-platform hardware-accelerated video decoder for Rust, built on top of
[`ffmpeg-next`](https://crates.io/crates/ffmpeg-next).

`VideoDecoder` mirrors the `send_packet` / `receive_frame` interface of
`ffmpeg::decoder::Video` and auto-probes the host's hardware backends.
This crate is **hardware-only** — there is no software fallback inside it.
If no hardware backend can decode the stream, `Error::AllBackendsFailed`
surfaces from `VideoDecoder::open` (when no backend opens) or from
`receive_frame` / `send_packet` / `send_eof` (when the initially-opened
backend fails at decode time and every remaining backend in the probe order
also fails — the only way it surfaces on single-backend platforms like macOS).
The caller decides how to fall back (typically by opening an
`ffmpeg::decoder::Video` directly). Output frames are CPU-side, downloaded
with `av_hwframe_transfer_data` (NV12 for 8-bit, P010 for 10-bit). Pixel-
format conversion is intentionally out of scope; safe per-row access is via
`Frame::row` / `Frame::rows` (clipped to visible byte width — never includes
FFmpeg's per-row alignment padding).

## Backends

| Target              | Probe order (HW only)             |
| ------------------- | --------------------------------- |
| macOS / iOS / tvOS  | VideoToolbox                      |
| Linux               | VAAPI → CUDA                      |
| Windows             | D3D11VA → CUDA                    |
| other               | (none)                            |

If `open` returns `Error::AllBackendsFailed`, software fallback is the
caller's responsibility (this crate intentionally does not include one).

## Usage

```rust,no_run
use ffmpeg_next as ffmpeg;
use ffmpeg::{format, media};
use hwdecode::{Frame, VideoDecoder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ffmpeg::init()?;

    let path = std::env::args()
        .nth(1)
        .expect("usage: hwdecode <input-file>");
    let mut input = format::input(&path)?;
    let stream = input.streams().best(media::Type::Video).unwrap();
    let stream_index = stream.index();

    // HW-only open. On AllBackendsFailed, fall back to software yourself.
    // `AllBackendsFailed` can also surface from `send_packet` / `send_eof` /
    // `receive_frame` if the only backend on this platform fails at decode
    // time; `unconsumed_packets` then carries the packets the decoder
    // already accepted, so non-seekable inputs (live streams, pipes) can
    // replay them through software without re-demuxing.
    let mut sw_fallback_packets: Vec<ffmpeg::Packet> = Vec::new();
    let mut decoder = match VideoDecoder::open(stream.parameters()) {
        Ok(d) => d,
        Err(hwdecode::Error::AllBackendsFailed { unconsumed_packets, .. }) => {
            // open-time failure: no packets were sent, vec is empty.
            sw_fallback_packets = unconsumed_packets;
            let _sw = ffmpeg::codec::Context::from_parameters(stream.parameters())?
                .decoder()
                .video()?;
            // Replay any rescued packets into _sw, then continue with
            // input.packets() through send_packet / receive_frame yourself.
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
println!("backend = {:?}", decoder.backend());

let mut frame = Frame::empty()?;
for (s, packet) in input.packets() {
    if s.index() != stream_index { continue; }
    match decoder.send_packet(&packet) {
        Ok(()) => {}
        Err(hwdecode::Error::AllBackendsFailed { unconsumed_packets, .. }) => {
            // Runtime exhaustion: the rescued packets are the bytes
            // the decoder already consumed from `input`. Feed them to
            // your software decoder before the current packet so even
            // a non-seekable source recovers cleanly.
            sw_fallback_packets = unconsumed_packets;
            sw_fallback_packets.push(packet);
            // ... open ffmpeg::decoder::Video and replay ...
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    while decoder.receive_frame(&mut frame).is_ok() {
        // frame.pix_fmt() is the integer constant — match against
        // hwdecode::pix_fmt::{NV12, P010LE, ...} and dispatch to your
        // pixel-format pipeline (e.g. `colconv`).
        // ... do something with frame ...
    }
}
decoder.send_eof()?;
while decoder.receive_frame(&mut frame).is_ok() {
    // ... drain ...
}
```

To force a specific hardware backend (no probe, no fallback):

```rust
use hwdecode::{Backend, VideoDecoder};
let decoder = VideoDecoder::open_with(parameters, Backend::VideoToolbox)?;
```

`hwdecode` is hardware-only: there is no `Backend::Software`. If `open`
returns `Error::AllBackendsFailed`, fall back to a software decoder
yourself (typically `ffmpeg::decoder::Video`).

## Running tests and benches

The integration test and benchmark expect a real video file. Set
`HWDECODE_SAMPLE_VIDEO` to enable them:

```sh
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo test
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo test --test hw_smoke -- --ignored
HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo bench
```

Without the env var the integration test skips with a notice; unit tests run
unconditionally.

## Build requirements

- A system FFmpeg ≥ **5.1** linkable via `pkg-config` (we reference
  `AV_PIX_FMT_P212LE` / `AV_PIX_FMT_P412LE`, which were added in 5.1).
  Tested against 8.1. Verify with
  `ffmpeg -hwaccels` that your build has the backends you expect compiled in
  (e.g. `videotoolbox` on macOS, `vaapi` / `cuda` on Linux,
  `d3d11va` / `cuda` on Windows).
- Rust ≥ 1.95.

## License

MIT or Apache-2.0, at your option.
