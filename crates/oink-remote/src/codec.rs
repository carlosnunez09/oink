//! Tile video. The host compares each captured picture with the last one it
//! sent, in tiles of 64 by 64 pixels, joins the changed tiles into rectangles
//! and sends each rectangle as a JPEG. While the picture moves the JPEGs use
//! the viewer's chosen quality; once it rests, the tiles sent that way are
//! sent again at high quality so text is sharp.

use crate::proto::EncodedRect;
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};

pub const TILE: usize = 64;
/// Rectangles are at most this many tiles tall, so a large change is encoded
/// on several processor cores.
const MAX_RECT_TILES: usize = 4;
const REFINE_QUALITY: u8 = 92;

/// A picture in BGRA byte order, rows top to bottom without padding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }
    /// Change the size, keeping the allocation where possible. Contents are unspecified.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.data.resize(width as usize * height as usize * 4, 0);
    }
}

/// A rectangle in tiles: columns `c0..c1`, rows `r0..r1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TileRect {
    c0: usize,
    c1: usize,
    r0: usize,
    r1: usize,
}

/// Join marked tiles into rectangles: runs along each row, then runs with the
/// same columns in consecutive rows, up to [`MAX_RECT_TILES`] rows.
fn merge(marked: &[bool], cols: usize, rows: usize) -> Vec<TileRect> {
    let mut rects: Vec<TileRect> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for row in 0..rows {
        let mut next_open = Vec::new();
        let mut col = 0;
        while col < cols {
            if !marked[row * cols + col] {
                col += 1;
                continue;
            }
            let start = col;
            while col < cols && marked[row * cols + col] {
                col += 1;
            }
            let above = open.iter().copied().find(|&index| {
                let rect = rects[index];
                rect.c0 == start && rect.c1 == col && rect.r1 == row && rect.r1 - rect.r0 < MAX_RECT_TILES
            });
            match above {
                Some(index) => {
                    rects[index].r1 = row + 1;
                    next_open.push(index);
                }
                None => {
                    rects.push(TileRect {
                        c0: start,
                        c1: col,
                        r0: row,
                        r1: row + 1,
                    });
                    next_open.push(rects.len() - 1);
                }
            }
        }
        open = next_open;
    }
    rects
}

fn encode_rect(frame: &Frame, rect: TileRect, quality: u8) -> EncodedRect {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let (x, y) = (rect.c0 * TILE, rect.r0 * TILE);
    let (w, h) = ((rect.c1 * TILE).min(width) - x, (rect.r1 * TILE).min(height) - y);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for row in y..y + h {
        let start = (row * width + x) * 4;
        for bgra in frame.data[start..start + w * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[bgra[2], bgra[1], bgra[0]]);
        }
    }
    let mut data = Vec::with_capacity(w * h / 4 + 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality)
        .encode(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)
        .expect("encoding a JPEG into memory cannot fail for a valid size");
    EncodedRect {
        x: x as u16,
        y: y as u16,
        w: w as u16,
        h: h as u16,
        data,
    }
}

fn encode_all(frame: &Frame, rects: &[TileRect], quality: u8) -> Vec<EncodedRect> {
    let workers = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(8)
        .min(rects.len());
    if workers <= 1 {
        return rects.iter().map(|&rect| encode_rect(frame, rect, quality)).collect();
    }
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, EncodedRect)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&rect) = rects.get(index) else { break mine };
                        mine.push((index, encode_rect(frame, rect, quality)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("an encoder thread panicked"))
            .collect()
    });
    done.sort_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, rect)| rect).collect()
}

/// Host side: remembers what the viewer has and produces the difference.
#[derive(Debug, Default)]
pub struct Encoder {
    sent: Frame,
    cols: usize,
    rows: usize,
    /// Tiles the viewer has only in moving-picture quality.
    lossy: Vec<bool>,
    lossy_count: usize,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget what the viewer has: the next picture is sent whole.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The rectangles of `frame` that differ from what the viewer has; empty if none.
    pub fn encode(&mut self, frame: &Frame, quality: u8) -> Vec<EncodedRect> {
        let (width, height) = (frame.width as usize, frame.height as usize);
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let whole = self.sent.width != frame.width || self.sent.height != frame.height;
        if whole {
            self.cols = width.div_ceil(TILE);
            self.rows = height.div_ceil(TILE);
            self.lossy = vec![false; self.cols * self.rows];
            self.lossy_count = 0;
        }
        let mut changed = vec![whole; self.cols * self.rows];
        if !whole {
            let stride = width * 4;
            for y in 0..height {
                let (new, old) = (
                    &frame.data[y * stride..][..stride],
                    &self.sent.data[y * stride..][..stride],
                );
                if new == old {
                    continue;
                }
                let row = y / TILE * self.cols;
                for col in 0..self.cols {
                    let (start, end) = (col * TILE * 4, ((col + 1) * TILE * 4).min(stride));
                    if !changed[row + col] && new[start..end] != old[start..end] {
                        changed[row + col] = true;
                    }
                }
            }
        }
        let rects = merge(&changed, self.cols, self.rows);
        if rects.is_empty() {
            return Vec::new();
        }
        self.sent.clone_from(frame);
        for (tile, _) in changed.iter().enumerate().filter(|(_, changed)| **changed) {
            if !std::mem::replace(&mut self.lossy[tile], true) {
                self.lossy_count += 1;
            }
        }
        encode_all(frame, &rects, quality.clamp(10, 95))
    }

    /// Whether some tiles are still waiting for their high-quality version.
    pub fn needs_refine(&self) -> bool {
        self.lossy_count > 0
    }

    /// The high-quality version of every tile that was sent in moving-picture quality.
    pub fn refine(&mut self) -> Vec<EncodedRect> {
        if self.lossy_count == 0 {
            return Vec::new();
        }
        let rects = merge(&self.lossy, self.cols, self.rows);
        self.lossy.fill(false);
        self.lossy_count = 0;
        encode_all(&self.sent, &rects, REFINE_QUALITY)
    }
}

/// Viewer side: the picture as `0x00RRGGBB` pixels, ready for the window.
#[derive(Clone, Debug, Default)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// A decoded rectangle that has not been drawn into the picture yet.
#[derive(Debug)]
pub struct DecodedRect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    bgra: Vec<u8>,
}

fn invalid(text: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text)
}

/// Decode one rectangle. Done without holding the picture, which the window may be drawing.
pub fn decode(rect: &EncodedRect) -> io::Result<DecodedRect> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let (w, h) = (rect.w as usize, rect.h as usize);
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::BGRA)
        .set_max_width(w)
        .set_max_height(h);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&rect.data), options);
    let bgra = decoder
        .decode()
        .map_err(|error| invalid(format!("bad picture data: {error:?}")))?;
    if decoder.dimensions() != Some((w, h)) || bgra.len() != w * h * 4 {
        return Err(invalid("picture data does not match its rectangle".to_owned()));
    }
    Ok(DecodedRect {
        x: rect.x as usize,
        y: rect.y as usize,
        w,
        h,
        bgra,
    })
}

/// Decode the rectangles of one frame, on several processor cores when there are several.
pub fn decode_all(rects: &[EncodedRect]) -> io::Result<Vec<DecodedRect>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(8)
        .min(rects.len());
    if workers <= 1 {
        return rects.iter().map(decode).collect();
    }
    let share = rects.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = rects
            .chunks(share)
            .map(|part| scope.spawn(move || part.iter().map(decode).collect::<io::Result<Vec<_>>>()))
            .collect();
        let mut decoded = Vec::with_capacity(rects.len());
        for handle in handles {
            decoded.extend(handle.join().expect("a decoder thread panicked")?);
        }
        Ok(decoded)
    })
}

impl Picture {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    pub fn draw(&mut self, rect: &DecodedRect) -> io::Result<()> {
        if rect.x + rect.w > self.width || rect.y + rect.h > self.height {
            return Err(invalid("rectangle outside the picture".to_owned()));
        }
        for row in 0..rect.h {
            let target = &mut self.pixels[(rect.y + row) * self.width + rect.x..][..rect.w];
            let source = &rect.bgra[row * rect.w * 4..][..rect.w * 4];
            for (pixel, bgra) in target.iter_mut().zip(source.as_chunks::<4>().0) {
                *pixel = u32::from_le_bytes([bgra[0], bgra[1], bgra[2], 0]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth picture, which JPEG reproduces closely.
    fn gradient(width: u32, height: u32) -> Frame {
        let mut frame = Frame::new(width, height);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let pixel = [
                    (x * 255 / width as usize) as u8,
                    (y * 255 / height as usize) as u8,
                    128,
                    255,
                ];
                frame.data[(y * width as usize + x) * 4..][..4].copy_from_slice(&pixel);
            }
        }
        frame
    }

    fn fill(frame: &mut Frame, x: usize, y: usize, w: usize, h: usize, bgra: [u8; 4]) {
        for row in y..y + h {
            for col in x..x + w {
                frame.data[(row * frame.width as usize + col) * 4..][..4].copy_from_slice(&bgra);
            }
        }
    }

    fn apply(picture: &mut Picture, rects: &[EncodedRect]) {
        for rect in decode_all(rects).unwrap() {
            picture.draw(&rect).unwrap();
        }
    }

    fn worst_error(picture: &Picture, frame: &Frame) -> u8 {
        picture
            .pixels
            .iter()
            .zip(frame.data.as_chunks::<4>().0)
            .flat_map(|(pixel, bgra)| {
                let got = pixel.to_le_bytes();
                (0..3).map(move |channel| got[channel].abs_diff(bgra[channel]))
            })
            .max()
            .unwrap()
    }

    #[test]
    fn merge_joins_runs_and_caps_height() {
        let (cols, rows) = (4, 6);
        let mut marked = vec![false; cols * rows];
        for row in 0..rows {
            marked[row * cols + 1] = true;
            marked[row * cols + 2] = true;
        }
        marked[3] = true;
        let rects = merge(&marked, cols, rows);
        assert_eq!(
            rects,
            vec![
                TileRect {
                    c0: 1,
                    c1: 4,
                    r0: 0,
                    r1: 1
                },
                TileRect {
                    c0: 1,
                    c1: 3,
                    r0: 1,
                    r1: 5
                },
                TileRect {
                    c0: 1,
                    c1: 3,
                    r0: 5,
                    r1: 6
                },
            ]
        );
    }

    #[test]
    fn first_picture_is_sent_whole_and_reproduced() {
        // A size that is not a multiple of the tile size.
        let frame = gradient(300, 170);
        let mut encoder = Encoder::new();
        let rects = encoder.encode(&frame, 80);
        let area: usize = rects.iter().map(|rect| rect.w as usize * rect.h as usize).sum();
        assert_eq!(area, 300 * 170);
        let mut picture = Picture::new(300, 170);
        apply(&mut picture, &rects);
        assert!(worst_error(&picture, &frame) <= 12, "{}", worst_error(&picture, &frame));
        assert!(
            encoder.encode(&frame, 80).is_empty(),
            "an unchanged picture sends nothing"
        );
    }

    #[test]
    fn only_changed_tiles_are_sent() {
        let mut frame = gradient(640, 480);
        let mut encoder = Encoder::new();
        let mut picture = Picture::new(640, 480);
        apply(&mut picture, &encoder.encode(&frame, 80));
        // One pixel in tile (2, 1) and a block across tiles (8..10, 6).
        fill(&mut frame, 130, 70, 1, 1, [255, 255, 255, 255]);
        fill(&mut frame, 8 * 64 + 10, 6 * 64 + 5, 70, 10, [0, 0, 0, 255]);
        let rects = encoder.encode(&frame, 80);
        let mut bounds: Vec<_> = rects.iter().map(|rect| (rect.x, rect.y, rect.w, rect.h)).collect();
        bounds.sort();
        assert_eq!(bounds, vec![(128, 64, 64, 64), (512, 384, 128, 64)]);
        apply(&mut picture, &rects);
        assert_eq!(
            picture.pixels[70 * 640 + 300] & 0xFF_0000,
            128 << 16,
            "untouched area is kept"
        );
    }

    #[test]
    fn refine_resends_moving_tiles_sharply_once() {
        let mut frame = gradient(256, 128);
        // Fine detail, which a low quality smears.
        for x in (0..64).step_by(2) {
            fill(&mut frame, x, 0, 1, 64, [0, 0, 255, 255]);
            fill(&mut frame, x + 1, 0, 1, 64, [255, 0, 0, 255]);
        }
        let mut encoder = Encoder::new();
        let mut picture = Picture::new(256, 128);
        apply(&mut picture, &encoder.encode(&frame, 30));
        let rough = worst_error(&picture, &frame);
        assert!(encoder.needs_refine());
        apply(&mut picture, &encoder.refine());
        let sharp = worst_error(&picture, &frame);
        assert!(sharp < rough, "refined {sharp} should beat moving {rough}");
        assert!(sharp <= 40, "{sharp}");
        assert!(!encoder.needs_refine());
        assert!(encoder.refine().is_empty());
    }

    #[test]
    fn a_new_size_sends_the_whole_picture_again() {
        let mut encoder = Encoder::new();
        encoder.encode(&gradient(128, 128), 80);
        let rects = encoder.encode(&gradient(192, 64), 80);
        let area: usize = rects.iter().map(|rect| rect.w as usize * rect.h as usize).sum();
        assert_eq!(area, 192 * 64);
    }

    #[test]
    fn bad_rectangles_are_rejected() {
        let mut encoder = Encoder::new();
        let mut rects = encoder.encode(&gradient(128, 64), 80);
        let mut picture = Picture::new(100, 64);
        assert!(
            picture.draw(&decode(&rects[0]).unwrap()).is_err(),
            "outside the picture"
        );
        rects[0].w = 64;
        assert!(decode(&rects[0]).is_err(), "size differs from the JPEG's own");
        rects[0].data = vec![1, 2, 3];
        assert!(decode(&rects[0]).is_err(), "not a JPEG");
    }
}
