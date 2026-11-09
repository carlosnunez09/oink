//! Fit the picture into the window: keep the proportions, centre it, fill the
//! rest with black and smooth when the sizes differ.

use std::thread;

/// Where a `sw` by `sh` picture goes in a `dw` by `dh` window: `(x, y, width, height)`.
pub fn fit(sw: usize, sh: usize, dw: usize, dh: usize) -> (usize, usize, usize, usize) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0, 0, 0);
    }
    let (mut w, mut h) = (dw, (sh as u64 * dw as u64 / sw as u64) as usize);
    if h > dh {
        (w, h) = ((sw as u64 * dh as u64 / sh as u64) as usize, dh);
    }
    let (w, h) = (w.clamp(1, dw), h.clamp(1, dh));
    ((dw - w) / 2, (dh - h) / 2, w, h)
}

/// For each target position: the two source positions around it and the weight
/// (0..=256) of the second.
fn taps(source: usize, target: usize) -> Vec<(u32, u32, u32)> {
    (0..target)
        .map(|index| {
            // Pixel centres: source position = (index + 0.5) * source / target - 0.5, in 1/256.
            let position = (((2 * index as u64 + 1) * source as u64 * 256) / (2 * target as u64)).saturating_sub(128);
            let first = ((position >> 8) as usize).min(source - 1);
            (
                first as u32,
                (first + 1).min(source - 1) as u32,
                (position & 255) as u32,
            )
        })
        .collect()
}

#[inline]
fn mix(a: u32, b: u32, weight: u32) -> u32 {
    let keep = 256 - weight;
    let red_blue = ((a & 0x00FF_00FF) * keep + (b & 0x00FF_00FF) * weight) >> 8 & 0x00FF_00FF;
    let green = ((a & 0x0000_FF00) * keep + (b & 0x0000_FF00) * weight) >> 8 & 0x0000_FF00;
    red_blue | green
}

/// Draw the whole `source` picture into `target` (both `0x00RRGGBB`, row by row).
pub fn blit(source: &[u32], sw: usize, sh: usize, target: &mut [u32], dw: usize, dh: usize) {
    assert!(source.len() >= sw * sh && target.len() >= dw * dh);
    let (left, top, width, height) = fit(sw, sh, dw, dh);
    if width == 0 {
        target.fill(0);
        return;
    }
    let (columns, lines) = (taps(sw, width), taps(sh, height));
    let same_size = width == sw && height == sh;
    let draw_row = |y: usize, row: &mut [u32]| {
        if y < top || y >= top + height {
            row.fill(0);
            return;
        }
        row[..left].fill(0);
        row[left + width..].fill(0);
        let row = &mut row[left..left + width];
        if same_size {
            row.copy_from_slice(&source[(y - top) * sw..][..sw]);
            return;
        }
        let (first, second, weight) = lines[y - top];
        let (upper, lower) = (
            &source[first as usize * sw..][..sw],
            &source[second as usize * sw..][..sw],
        );
        for (pixel, &(a, b, across)) in row.iter_mut().zip(&columns) {
            let (a, b) = (a as usize, b as usize);
            *pixel = mix(mix(upper[a], upper[b], across), mix(lower[a], lower[b], across), weight);
        }
    };
    let workers = thread::available_parallelism().map_or(1, |count| count.get()).min(8);
    let band = dh.div_ceil(workers).max(1);
    thread::scope(|scope| {
        for (index, rows) in target[..dw * dh].chunks_mut(band * dw).enumerate() {
            let draw_row = &draw_row;
            scope.spawn(move || {
                for (offset, row) in rows.chunks_mut(dw).enumerate() {
                    draw_row(index * band + offset, row);
                }
            });
        }
    });
}

/// The picture position (each axis 0..=65535) under a window pixel, or `None`
/// outside the picture.
pub fn locate(x: f64, y: f64, sw: usize, sh: usize, dw: usize, dh: usize) -> Option<(u16, u16)> {
    let (left, top, width, height) = fit(sw, sh, dw, dh);
    let (x, y) = (x - left as f64, y - top as f64);
    if width == 0 || x < 0.0 || y < 0.0 || x >= width as f64 || y >= height as f64 {
        return None;
    }
    let along = |position: f64, size: usize| (position / size as f64 * 65536.0).clamp(0.0, 65535.0) as u16;
    Some((along(x, width), along(y, height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_proportions_and_centres() {
        assert_eq!(fit(1920, 1080, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(fit(1920, 1080, 960, 1000), (0, 230, 960, 540));
        assert_eq!(fit(1000, 1000, 3000, 2000), (500, 0, 2000, 2000));
        assert_eq!(fit(0, 10, 100, 100), (0, 0, 0, 0));
    }

    #[test]
    fn same_size_is_an_exact_copy() {
        let source: Vec<u32> = (0..12).collect();
        let mut target = vec![9; 12];
        blit(&source, 4, 3, &mut target, 4, 3);
        assert_eq!(target, source);
    }

    #[test]
    fn letterbox_is_black_and_colours_survive_scaling() {
        let source = vec![0x00FF_8040; 8 * 4];
        let mut target = vec![7; 20 * 20];
        blit(&source, 8, 4, &mut target, 20, 20);
        for y in 0..20 {
            for x in 0..20 {
                let expected = if (5..15).contains(&y) { 0x00FF_8040 } else { 0 };
                assert_eq!(target[y * 20 + x], expected, "{x},{y}");
            }
        }
    }

    #[test]
    fn scaling_down_blends_neighbours() {
        // Black and white columns, halved: every target pixel is their average.
        let source: Vec<u32> = (0..8 * 2)
            .map(|index| if index % 2 == 0 { 0 } else { 0x00FF_FFFF })
            .collect();
        let mut target = vec![0; 4];
        blit(&source, 8, 2, &mut target, 4, 1);
        for pixel in target {
            assert!((0x7E..=0x81).contains(&(pixel & 0xFF)), "{pixel:#x}");
        }
    }

    #[test]
    fn window_pixels_map_back_to_the_picture() {
        assert_eq!(locate(0.0, 230.0, 1920, 1080, 960, 1000), Some((0, 0)));
        assert_eq!(locate(959.9, 769.9, 1920, 1080, 960, 1000), Some((65529, 65523)));
        assert_eq!(locate(10.0, 229.0, 1920, 1080, 960, 1000), None);
        assert_eq!(locate(480.0, 500.0, 1920, 1080, 960, 1000), Some((32768, 32768)));
    }
}
