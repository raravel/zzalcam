//! Fast change detection between RGBA8 canvases.

use crate::Rect;

#[inline]
fn px(buf: &[u8], i: usize) -> [u8; 4] {
    [buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]]
}

/// True if two pixels differ by more than `tolerance` in any channel.
#[inline]
pub fn pixel_changed(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    if tolerance == 0 {
        return a != b;
    }
    a.iter()
        .zip(b.iter())
        .any(|(x, y)| x.abs_diff(*y) > tolerance)
}

fn row_changed(prev: &[u8], cur: &[u8], tolerance: u8) -> bool {
    if tolerance == 0 {
        return prev != cur;
    }
    prev.as_chunks::<4>()
        .0
        .iter()
        .zip(cur.as_chunks::<4>().0.iter())
        .any(|(a, b)| a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > tolerance))
}

/// Bounding box of all pixels that differ between `prev` and `cur`, or `None` if identical.
pub fn changed_bbox(
    prev: &[u8],
    cur: &[u8],
    width: u32,
    height: u32,
    tolerance: u8,
) -> Option<Rect> {
    let (w, h) = (width as usize, height as usize);
    let stride = w * 4;
    debug_assert_eq!(prev.len(), stride * h);
    debug_assert_eq!(cur.len(), stride * h);

    let row = |y: usize| {
        row_changed(
            &prev[y * stride..][..stride],
            &cur[y * stride..][..stride],
            tolerance,
        )
    };
    let top = (0..h).find(|&y| row(y))?;
    let bottom = (top..h).rev().find(|&y| row(y)).unwrap_or(top);

    let mut left = w;
    let mut right = 0;
    for y in top..=bottom {
        let base = y * w;
        if let Some(x) =
            (0..left).find(|&x| pixel_changed(px(prev, base + x), px(cur, base + x), tolerance))
        {
            left = x;
        }
        if let Some(x) = (right..w)
            .rev()
            .find(|&x| pixel_changed(px(prev, base + x), px(cur, base + x), tolerance))
        {
            right = right.max(x);
        }
    }
    if left > right {
        return None;
    }
    Some(Rect::new(
        left as u32,
        top as u32,
        (right - left + 1) as u32,
        (bottom - top + 1) as u32,
    ))
}

/// Copy `rect` out of a full canvas into a tightly packed buffer.
pub fn crop(canvas: &[u8], width: u32, rect: Rect) -> Vec<u8> {
    let stride = width as usize * 4;
    let row_len = rect.w as usize * 4;
    let mut out = Vec::with_capacity(row_len * rect.h as usize);
    for y in rect.y as usize..(rect.y + rect.h) as usize {
        let start = y * stride + rect.x as usize * 4;
        out.extend_from_slice(&canvas[start..start + row_len]);
    }
    out
}

/// Paste a tightly packed `rect`-sized buffer into a full canvas.
pub fn blit(canvas: &mut [u8], width: u32, rect: Rect, data: &[u8]) {
    let stride = width as usize * 4;
    let row_len = rect.w as usize * 4;
    for (i, y) in (rect.y as usize..(rect.y + rect.h) as usize).enumerate() {
        let start = y * stride + rect.x as usize * 4;
        canvas[start..start + row_len].copy_from_slice(&data[i * row_len..][..row_len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_none() {
        let a = vec![7u8; 4 * 4 * 4];
        assert_eq!(changed_bbox(&a, &a, 4, 4, 0), None);
    }

    #[test]
    fn finds_bbox() {
        let a = vec![0u8; 8 * 6 * 4];
        let mut b = a.clone();
        b[(2 * 8 + 3) * 4] = 255; // (3,2)
        b[(4 * 8 + 6) * 4 + 1] = 1; // (6,4)
        assert_eq!(changed_bbox(&a, &b, 8, 6, 0), Some(Rect::new(3, 2, 4, 3)));
    }

    #[test]
    fn tolerance_ignores_small_changes() {
        let a = vec![100u8; 4 * 4 * 4];
        let mut b = a.clone();
        b[5] = 103;
        assert_eq!(changed_bbox(&a, &b, 4, 4, 3), None);
        assert!(changed_bbox(&a, &b, 4, 4, 2).is_some());
    }

    #[test]
    fn crop_blit_roundtrip() {
        let canvas: Vec<u8> = (0..5 * 5 * 4).map(|i| i as u8).collect();
        let r = Rect::new(1, 2, 3, 2);
        let part = crop(&canvas, 5, r);
        let mut out = vec![0u8; canvas.len()];
        blit(&mut out, 5, r, &part);
        assert_eq!(crop(&out, 5, r), part);
    }
}
