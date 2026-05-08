//! Frame types and supporting building blocks.
//!
//! `Rect` and `Plane<B>` are the shared building blocks. The full
//! `VideoFrame` / `AudioFrame` / `SubtitleFrame` types land in later
//! tasks.

/// An axis-aligned integer rectangle.
///
/// Used for `VideoFrame::visible_rect` (FFmpeg crop /
/// WebCodecs `visibleRect` / ProRes RAW `CleanAperture`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    x:      u32,
    y:      u32,
    width:  u32,
    height: u32,
}

impl Rect {
    /// Constructs a `Rect` at `(x, y)` with the given size.
    #[inline]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Returns the X coordinate of the top-left corner.
    #[inline]
    pub const fn x(&self) -> u32 { self.x }

    /// Returns the Y coordinate of the top-left corner.
    #[inline]
    pub const fn y(&self) -> u32 { self.y }

    /// Returns the width.
    #[inline]
    pub const fn width(&self) -> u32 { self.width }

    /// Returns the height.
    #[inline]
    pub const fn height(&self) -> u32 { self.height }

    /// Sets the X coordinate (consuming builder).
    #[inline]
    pub const fn with_x(mut self, x: u32) -> Self { self.x = x; self }
    /// Sets the Y coordinate (consuming builder).
    #[inline]
    pub const fn with_y(mut self, y: u32) -> Self { self.y = y; self }
    /// Sets the width (consuming builder).
    #[inline]
    pub const fn with_width(mut self, w: u32) -> Self { self.width = w; self }
    /// Sets the height (consuming builder).
    #[inline]
    pub const fn with_height(mut self, h: u32) -> Self { self.height = h; self }

    /// Sets the X coordinate in place.
    #[inline]
    pub const fn set_x(&mut self, x: u32) -> &mut Self { self.x = x; self }
    /// Sets the Y coordinate in place.
    #[inline]
    pub const fn set_y(&mut self, y: u32) -> &mut Self { self.y = y; self }
    /// Sets the width in place.
    #[inline]
    pub const fn set_width(&mut self, w: u32) -> &mut Self { self.width = w; self }
    /// Sets the height in place.
    #[inline]
    pub const fn set_height(&mut self, h: u32) -> &mut Self { self.height = h; self }
}

/// One plane of pixel or audio data.
///
/// Generic over the buffer type `B` so the same `Plane` shape works
/// for owned (`Vec<u8>`, `bytes::Bytes`), borrowed (`&'a [u8]`), or
/// custom backend-supplied buffers. The bound `B: AsRef<[u8]>` lives
/// at the use site (`Frame<A, B: AsRef<[u8]>>`); `Plane` itself is
/// unbounded so it can be used in const contexts.
///
/// `stride` is bytes per row for video planes, total plane size in
/// bytes for audio planar formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Plane<B> {
    data:   B,
    stride: u32,
}

impl<B> Plane<B> {
    /// Constructs a `Plane` from a buffer and a stride.
    #[inline]
    pub const fn new(data: B, stride: u32) -> Self {
        Self { data, stride }
    }

    /// Returns the stride in bytes.
    #[inline]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Borrows the underlying buffer.
    #[inline]
    pub const fn data(&self) -> &B {
        &self.data
    }

    /// Mutably borrows the underlying buffer.
    #[inline]
    pub fn data_mut(&mut self) -> &mut B {
        &mut self.data
    }

    /// Consumes the plane and returns the underlying buffer.
    #[inline]
    pub fn into_data(self) -> B {
        self.data
    }

    /// Sets the stride (consuming builder).
    #[inline]
    pub const fn with_stride(mut self, stride: u32) -> Self {
        self.stride = stride;
        self
    }

    /// Sets the stride in place.
    #[inline]
    pub const fn set_stride(&mut self, stride: u32) -> &mut Self {
        self.stride = stride;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_construct_and_access() {
        let r = Rect::new(10, 20, 1920, 1080);
        assert_eq!(r.x(), 10);
        assert_eq!(r.y(), 20);
        assert_eq!(r.width(), 1920);
        assert_eq!(r.height(), 1080);
    }

    #[test]
    fn rect_default_is_zero() {
        let r = Rect::default();
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (0, 0, 0, 0));
    }

    #[test]
    fn rect_builders_chain() {
        let r = Rect::default()
            .with_x(1)
            .with_y(2)
            .with_width(3)
            .with_height(4);
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (1, 2, 3, 4));
    }

    #[test]
    fn rect_setters_chain() {
        let mut r = Rect::default();
        r.set_x(5).set_y(6).set_width(7).set_height(8);
        assert_eq!((r.x(), r.y(), r.width(), r.height()), (5, 6, 7, 8));
    }

    #[test]
    fn rect_const_construction() {
        const R: Rect = Rect::new(0, 0, 1920, 1080);
        assert_eq!(R.width(), 1920);
    }

    #[test]
    fn plane_construct_and_access_borrowed() {
        let buf: [u8; 4] = [1, 2, 3, 4];
        let p: Plane<&[u8]> = Plane::new(&buf, 4);
        assert_eq!(p.stride(), 4);
        assert_eq!(p.data(), &&buf[..]);
    }

    #[test]
    fn plane_with_and_set_stride() {
        let buf: [u8; 0] = [];
        let p = Plane::new(&buf[..], 16).with_stride(32);
        assert_eq!(p.stride(), 32);
        let mut p2 = p;
        p2.set_stride(64);
        assert_eq!(p2.stride(), 64);
    }

    #[test]
    fn plane_into_data() {
        let buf: [u8; 4] = [1, 2, 3, 4];
        let p: Plane<&[u8]> = Plane::new(&buf, 4);
        let recovered = p.into_data();
        assert_eq!(recovered, &buf[..]);
    }
}
