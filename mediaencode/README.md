<div align="center">
<h1>mediaencode</h1>
</div>

The write faces of the mediaio family: the traits an encoder and a muxer
implement, the settings an encoder is opened with, and the refusals
every write backend names the same way. It holds no encoder and no
FFmpeg, and it builds with `no_std` and no allocator.

| item | what it is |
|---|---|
| `VideoEncoder`, `AudioEncoder` | raw frames in, compressed packets out, in the push rhythm the decoders keep: `send_frame` answers `Sent`, `receive_packet` answers `Received`, `send_eof` drains to `Ended`; `flush` resets, or answers `Flushed::Refused` where the codec cannot; `force_keyframe` (video); `frame_size` (audio) |
| `Muxer` | per-kind writes of the packets a demuxer delivers, and `finish`, which ends the session; opened by the backend, never by the face |
| `EncoderSettings` | the codec, rate control, profile and level, and the kind's own parameters: `VideoParams` (dimensions, pixel format, timebase, frame rate, GOP, color, HDR metadata) or `AudioParams` (sample rate, sample format, channel layout) |
| `EncodeError`, `MuxError` | a named refusal every backend shares (`EncodeRefusal`, `MuxRefusal`), or the backend's own fault |

The vocabulary these faces speak — frames, packets, the rhythm's
`Sent` and `Received`, the demuxer's track types — is `mediaspine`'s,
re-exported here as `mediaencode::mediaspine`. The read faces live in
`mediadecode`; the FFmpeg write adapter is `mediaencode-ffmpeg`.

## Features

| feature | default | what it adds |
|---|---|---|
| `std` | yes | the vocabulary's `std` tier |
| `alloc` | no | the vocabulary's allocating tier, for builds without `std` |

With no features the crate is `core`-only.

#### License

`mediaencode` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.
