//! What the `unsafe` crossings read through the pointers they are handed
//! — one section per FFmpeg object, stated once.
//!
//! Every `unsafe fn` of this crate (and of `mediadecode-ffmpeg`, which
//! re-exports them) names, in its own `# Safety` section, each pointer it
//! dereferences, directly or through the helpers it calls, and links the
//! section below that says what such a pointer must hold. A wrapper
//! forwards its worker's contract whole, by naming the same sections.
//!
//! # Reading
//!
//! A pointer a crossing **reads** — and every element of an array it
//! reads, for the whole declared count — must be, for the duration of the
//! call:
//!
//! - **live**: pointing at memory allocated for its type and not yet
//!   freed;
//! - **aligned** for its type;
//! - **initialised** in every byte the crossing reads;
//! - **unwritten**: nothing else — no other alias, no other thread —
//!   writes through it while the call reads it.
//!
//! A field marked *plain* below is an integer, or an open C enum, that a
//! crossing reads by value and judges itself: it may hold any value, and
//! an enum is read as the integer it is stored as, never as a Rust enum.
//! A field marked *checked* is a pointer or count whose consistency a
//! crossing establishes before it relies on it — it may be null, or
//! disagree with its neighbours, and is refused rather than trusted.
//! Only what remains carries an obligation, and that is what each
//! section lists.
//!
//! # What a crossing hands back
//!
//! On the **owned** lane every byte a returned carrier holds is a copy,
//! and the source may be dropped or written at once. On the **view** lane
//! a returned carrier may be a window onto the source's own
//! `AVBufferRef`, holding a reference of its own: the buffer outlives
//! the source by that reference, and nothing may write through those
//! bytes while the carrier — or any clone of it — lives.
//!
//! # `AVBufferRef`
//!
//! - the `AVBufferRef` itself, read;
//! - its `buffer`, live: its reference count is read
//!   (`av_buffer_get_ref_count`), and on the view lane taken
//!   (`av_buffer_ref`);
//! - its `data`, valid for reads of `size` bytes. The bytes a crossing
//!   copies or lends are initialised; the rest of the buffer is never
//!   read.
//!
//! # `AVPacket`
//!
//! - `buf`: null, or an [`AVBufferRef`](#avbufferref) — the payload's
//!   owner;
//! - the `size` payload bytes at `data`, when `buf` is non-null and
//!   `size > 0`: initialised. Their extent is *checked* against `buf`'s
//!   own `data` and `size` before anything is read;
//! - `side_data`, when `side_data_elems > 0`: valid for
//!   `side_data_elems` `AVPacketSideData` entries (the count is *checked*
//!   against this crate's cap and the array against null before the first
//!   entry is read), and each entry's `data`, when its `size > 0`, valid
//!   for and initialised in `size` bytes;
//! - *plain*: `flags`, `pts`, `dts`, `duration`, `stream_index`, each
//!   entry's `type`.
//!
//! An `ffmpeg_next::Packet` keeps its `AVPacket` inline, so the packet
//! itself is always live and aligned behind a borrow of one; what the
//! borrow cannot vouch for is the list above — and every
//! `ffmpeg_next::Packet` meets it, with one exception that is upstream's.
//! Its `buf`, `data`, `size` and `side_data` are written only by
//! libavformat, libavcodec and ffmpeg-next's own allocating calls
//! (`copy`, `grow`, `shrink`), which keep them in step, and no safe call
//! writes them otherwise. Its payload is initialised by every road that
//! fills one except `ffmpeg_next::Packet::new`, which leaves the bytes
//! unwritten — bytes ffmpeg-next's own safe `Packet::data` already lends
//! as `&[u8]`, recorded as an upstream defect rather than one this crate
//! can close. The safe conversions rely on this paragraph.
//!
//! # A picture `AVFrame`
//!
//! - `buf[0..8]`: each null, or an [`AVBufferRef`](#avbufferref);
//! - each plane's rows — `data[i]` plus `linesize[i]` per row, for the
//!   plane's height, `row_bytes` read from each — initialised. The planes
//!   are *checked*: a plane is read only after a buffer in `buf[]` is
//!   found whose `data .. data + size` contains every row it reads;
//! - `side_data`, when `nb_side_data > 0` and the array is not null:
//!   valid for `nb_side_data` pointers — at most the first 64 are
//!   read — each null (skipped) or a live
//!   `AVFrameSideData` whose `data` is valid for and initialised in its
//!   `size` bytes;
//! - *plain*: `format`, `width`, `height`, `linesize[]`, `crop_*`,
//!   `color_primaries`, `color_trc`, `colorspace`, `color_range`,
//!   `chroma_location`, `sample_aspect_ratio`, `pict_type`, `flags`,
//!   `pts`, `duration`, `best_effort_timestamp`, each side-data entry's
//!   `type`.
//!
//! # An audio `AVFrame`
//!
//! - `buf[0..8]`: each null, or an [`AVBufferRef`](#avbufferref);
//! - each plane's samples — `data[i]`, for the bytes `nb_samples` of the
//!   frame's format occupy in that plane — initialised. The planes are
//!   *checked* as a picture's are, against `buf[]`;
//! - `ch_layout`: an [`AVChannelLayout`](#avchannellayout), read in
//!   place;
//! - `side_data`, as for a picture;
//! - *plain*: `format`, `sample_rate`, `nb_samples`, `linesize[0]`, `pts`,
//!   `duration`, `best_effort_timestamp`.
//!
//! # `AVChannelLayout`
//!
//! - `u.map`, when `order` is `AV_CHANNEL_ORDER_CUSTOM`, `nb_channels`
//!   is a count the reader accepts — `1..=65_535` for the layout
//!   readers, `1..=255` for an audio frame's conversion — and the map is
//!   not null (both *checked*): valid for and initialised in `nb_channels`
//!   `AVChannelCustom` entries. An entry's `id` is read as an integer,
//!   its `name` as sixteen bytes, and its `opaque` as a pointer value
//!   that is never dereferenced;
//! - *plain*: `order`, `nb_channels`, `u.mask`.
//!
//! A layout FFmpeg filled — `av_channel_layout_copy`,
//! `av_channel_layout_custom_init` — always holds its map for its count.
//! Nothing safe can establish that of a layout it did not fill, which is
//! why the safe conversions refuse a custom map rather than read one.
//!
//! # `AVSubtitle`
//!
//! - `rects`, when `num_rects > 0`: valid for `num_rects` pointers (null
//!   is *checked*; at most the first 64 are read), each null or a live
//!   `AVSubtitleRect`;
//! - each rect's `text` and `ass`: null, or readable byte by byte up to
//!   and including their NUL terminator or for 65,537 bytes, whichever
//!   comes first — the reader stops at whichever it meets;
//! - each bitmap rect's `data[0]`: null, or valid for and initialised in
//!   `linesize[0] × h` bytes, when that product is within the reader's
//!   caps (16 MiB a rect; a larger one is refused before it is read);
//! - each bitmap rect's `data[1]`: null, or valid for and initialised in
//!   1,024 bytes — the 256-entry RGBA palette, read whole;
//! - *plain*: `num_rects`, `pts`, `start_display_time`,
//!   `end_display_time`, each rect's `type`, `x`, `y`, `w`, `h`,
//!   `linesize[0]`.
//!
//! An `ffmpeg_next::Subtitle` keeps its `AVSubtitle` inline, so the
//! top-level struct is live behind a borrow of one; the rects and what
//! they point at are the list above.
//!
//! # `AVCodecParameters`
//!
//! - `extradata`: null, or valid for and initialised in `extradata_size`
//!   bytes, for a reader that copies it (one that only measures reads the
//!   pointer value and the size);
//! - `coded_side_data`, when `nb_coded_side_data > 0`: valid for
//!   `nb_coded_side_data` `AVPacketSideData` entries — each entry's `size`
//!   is read — and, for a reader that copies them, each entry's `data`
//!   valid for and initialised in its `size` bytes;
//! - `ch_layout`: an [`AVChannelLayout`](#avchannellayout), read in
//!   place;
//! - *plain*: every other field a reader reads.
//!
//! An `ffmpeg_next::codec::Parameters` holds a **pointer**, and its safe
//! constructor stores whatever `avcodec_parameters_alloc` answered —
//! null after an allocation failure. The safe readers of this crate check
//! for that before they read a field.
//!
//! # `AVFormatContext`
//!
//! - `iformat`: null, or a live `AVInputFormat` whose `name` and
//!   `long_name` are each null or a NUL-terminated string that stays
//!   unwritten for the life of the process — libavformat's own static
//!   tables (read for at most 1,024 bytes, stopping at the NUL).
//!
//! # `AVCodecContext`, for the callbacks
//!
//! - `opaque`: null, or the [`CallbackState`](#callbackstate) that
//!   [`install_allocator_judge`](crate::ffi::install_allocator_judge)
//!   installed;
//! - `hw_device_ctx`: null, or a live `AVBufferRef` to an
//!   `AVHWDeviceContext` — handed to `avcodec_get_hw_frames_parameters`,
//!   whose answer, an `AVHWFramesContext`, is read and released inside
//!   the call;
//! - everything else libavcodec itself reads when it is called with the
//!   context: these are libavcodec's callbacks, and a context libavcodec
//!   passes them holds it.
//!
//! # `AVCodec`
//!
//! - the `AVCodec` itself: one libavcodec handed out (`avcodec_find_decoder`
//!   and its kin), which points into a table that lives for the process.
//!   Its hardware configurations are read through `avcodec_get_hw_config`,
//!   each one's `methods`, `device_type` and `pix_fmt` as integers.
//!
//! # `CallbackState`
//!
//! - null, or a live, aligned `CallbackState`. Its atomics are read and
//!   written through shared access, so another thread may use the state
//!   during the call; its plain fields (`wanted`, `wanted_int`,
//!   `max_frame_bytes`) are read and must not be written while the call
//!   runs.
