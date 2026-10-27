//! Pixel format conversion.

/// Copy BGRA rows from a mapped texture into an RGBA canvas, forcing alpha opaque.
///
/// `src` points at the first source pixel, `src_pitch` is the texture row pitch in bytes;
/// `w`×`h` pixels land at (`dx`, `dy`) in a canvas `canvas_w` pixels wide.
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_rgba(
    src: &[u8],
    src_pitch: usize,
    w: usize,
    h: usize,
    canvas: &mut [u8],
    canvas_w: usize,
    dx: usize,
    dy: usize,
) {
    for y in 0..h {
        let s = &src[y * src_pitch..][..w * 4];
        let d = &mut canvas[((dy + y) * canvas_w + dx) * 4..][..w * 4];
        for (s, d) in s.chunks_exact(4).zip(d.chunks_exact_mut(4)) {
            let v = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            let o = ((v >> 16) & 0xff) | (v & 0xff00) | ((v & 0xff) << 16) | 0xff00_0000;
            d.copy_from_slice(&o.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn swizzles_and_offsets() {
        let src = [
            1, 2, 3, 0, 4, 5, 6, 0, /* pad */ 9, 9, /* row 2 */ 7, 8, 9, 0, 10, 11, 12,
            0, 9, 9,
        ];
        let mut canvas = vec![0u8; 3 * 3 * 4];
        super::bgra_to_rgba(&src, 10, 2, 2, &mut canvas, 3, 1, 1);
        assert_eq!(&canvas[16..24], &[3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(&canvas[28..36], &[9, 8, 7, 255, 12, 11, 10, 255]);
        assert_eq!(&canvas[0..4], &[0, 0, 0, 0]);
    }
}
