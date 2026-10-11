//! Pixel helpers: positioned text lines, redaction boxes, image codecs.

use crate::names::{Hit, Line};
use crate::ocr;
use anyhow::{anyhow, Result};

/// A line of text where each character may carry a box (normalized 0..1, y down).
#[derive(Default, Clone)]
pub struct BoxLine {
    pub text: Vec<char>,
    pub boxes: Vec<Option<[f32; 4]>>,
}

impl BoxLine {
    pub fn push(&mut self, c: char, b: Option<[f32; 4]>) {
        self.text.push(c);
        self.boxes.push(b);
    }

    pub fn to_line(&self, para_start: bool, ocr: bool) -> Line {
        Line { text: self.text.clone(), para_start, ocr }
    }

    /// Bounding box of characters `start..end`.
    pub fn span_box(&self, start: usize, end: usize) -> Option<[f32; 4]> {
        let mut acc: Option<[f32; 4]> = None;
        for b in self.boxes[start..end].iter().flatten() {
            acc = Some(match acc {
                None => *b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        }
        acc
    }

    /// Whitespace-separated words with their boxes.
    pub fn words(&self) -> Vec<(String, [f32; 4])> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.text.len() {
            while i < self.text.len() && self.text[i].is_whitespace() {
                i += 1;
            }
            let s = i;
            while i < self.text.len() && !self.text[i].is_whitespace() {
                i += 1;
            }
            if s < i {
                if let Some(b) = self.span_box(s, i) {
                    out.push((self.text[s..i].iter().collect(), b));
                }
            }
        }
        out
    }
}

/// Convert OCR output (pixel boxes in a `w`x`h` image) to normalized box lines.
pub fn ocr_to_lines(lines: Vec<Vec<ocr::Word>>, w: f32, h: f32) -> Vec<BoxLine> {
    lines
        .into_iter()
        .map(|words| {
            let mut bl = BoxLine::default();
            for (k, word) in words.into_iter().enumerate() {
                if k > 0 {
                    bl.push(' ', None);
                }
                let [x0, y0, x1, y1] = word.rect;
                let chars: Vec<char> = word.text.chars().collect();
                let n = chars.len().max(1) as f32;
                let horizontal = (x1 - x0) >= (y1 - y0) * 0.6 || chars.len() <= 1;
                for (ci, c) in chars.into_iter().enumerate() {
                    let b = if horizontal {
                        let cw = (x1 - x0) / n;
                        [x0 + cw * ci as f32, y0, x0 + cw * (ci as f32 + 1.0), y1]
                    } else {
                        [x0, y0, x1, y1]
                    };
                    bl.push(c, Some([b[0] / w, b[1] / h, b[2] / w, b[3] / h]));
                }
            }
            bl
        })
        .collect()
}

/// Drop OCR words that duplicate words already present in the PDF text layer.
pub fn dedupe_ocr(text_lines: &[BoxLine], ocr_lines: Vec<BoxLine>) -> Vec<BoxLine> {
    if text_lines.is_empty() {
        return ocr_lines;
    }
    let existing: Vec<[f32; 4]> = text_lines.iter().flat_map(|l| l.words().into_iter().map(|w| w.1)).collect();
    let overlaps = |b: [f32; 4]| {
        existing.iter().any(|e| {
            let ix = (b[2].min(e[2]) - b[0].max(e[0])).max(0.0);
            let iy = (b[3].min(e[3]) - b[1].max(e[1])).max(0.0);
            let inter = ix * iy;
            let area = ((b[2] - b[0]) * (b[3] - b[1])).max(1e-9);
            inter / area > 0.4
        })
    };
    ocr_lines
        .into_iter()
        .filter_map(|line| {
            let mut out = BoxLine::default();
            let mut i = 0;
            while i < line.text.len() {
                let s = i;
                while i < line.text.len() && !line.text[i].is_whitespace() {
                    i += 1;
                }
                if s < i {
                    let keep = line.span_box(s, i).is_some_and(|b| !overlaps(b));
                    if keep {
                        if !out.text.is_empty() {
                            out.push(' ', None);
                        }
                        for k in s..i {
                            out.push(line.text[k], line.boxes[k]);
                        }
                    }
                }
                i += 1;
            }
            (!out.text.is_empty()).then_some(out)
        })
        .collect()
}

/// Normalized redaction rectangles for the hits that fall on `line`. Neighbouring hits
/// ("Gary Skinner") become one box so fragments OCR missed between them are covered too.
pub fn hit_boxes(line: &BoxLine, hits: &[&Hit]) -> Vec<[f32; 4]> {
    let mut sorted: Vec<&&Hit> = hits.iter().collect();
    sorted.sort_by_key(|h| h.start);
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for h in sorted {
        if let Some(g) = groups.last_mut() {
            let between = &line.text[g.1.min(h.start)..h.start];
            if between.iter().all(|c| !c.is_alphanumeric()) {
                g.1 = g.1.max(h.end);
                continue;
            }
        }
        groups.push((h.start, h.end));
    }
    groups.into_iter().filter_map(|(s, e)| line.span_box(s, e)).collect()
}

/// True when at least a quarter of `w` lies under one of the boxes.
pub fn mostly_covered(w: [f32; 4], boxes: &[[f32; 4]]) -> bool {
    let area = ((w[2] - w[0]) * (w[3] - w[1])).max(1e-9);
    boxes.iter().any(|b| {
        let ix = (w[2].min(b[2]) - w[0].max(b[0])).max(0.0);
        let iy = (w[3].min(b[3]) - w[1].max(b[1])).max(0.0);
        ix * iy / area >= 0.25
    })
}

/// Paint solid black boxes (normalized coords) into a pixel buffer with `bpp` bytes per pixel.
pub fn paint_boxes(px: &mut [u8], w: usize, h: usize, stride: usize, bpp: usize, boxes: &[[f32; 4]]) {
    for b in boxes {
        let bh = (b[3] - b[1]) * h as f32;
        let pad_y = (bh * 0.12).max(1.5);
        let pad_x = (bh * 0.3).max(2.0);
        let x0 = ((b[0] * w as f32 - pad_x).floor().max(0.0)) as usize;
        let x1 = ((b[2] * w as f32 + pad_x).ceil() as usize).min(w);
        let y0 = ((b[1] * h as f32 - pad_y).floor().max(0.0)) as usize;
        let y1 = ((b[3] * h as f32 + pad_y).ceil() as usize).min(h);
        for y in y0..y1 {
            let row = &mut px[y * stride..y * stride + w * bpp];
            for x in x0..x1 {
                for c in 0..bpp {
                    // Keep alpha opaque in 4-channel buffers.
                    row[x * bpp + c] = if bpp == 4 && c == 3 { 255 } else { 0 };
                }
            }
        }
    }
}

pub struct Image {
    pub w: usize,
    pub h: usize,
    /// RGBA8
    pub px: Vec<u8>,
}

impl Image {
    pub fn gray(&self) -> Vec<u8> {
        self.px
            .chunks_exact(4)
            .map(|p| {
                // Composite transparent pixels on white.
                let a = p[3] as u32;
                let l = (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8;
                ((l * a + 255 * (255 - a)) / 255) as u8
            })
            .collect()
    }
}

pub fn decode_png(data: &[u8]) -> Result<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| anyhow!("png too large"))?];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let src = &buf[..info.buffer_size()];
    let px = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => src.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Indexed => return Err(anyhow!("unexpanded palette")),
    };
    Ok(Image { w, h, px })
}

pub fn encode_png(img: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w as u32, img.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header()?;
        wr.write_image_data(&img.px)?;
    }
    Ok(out)
}

pub fn decode_jpeg(data: &[u8]) -> Result<Image> {
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    let pixels = dec.decode()?;
    let info = dec.info().ok_or_else(|| anyhow!("jpeg info"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        jpeg_decoder::PixelFormat::L16 => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], 255]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => pixels
            .chunks_exact(4)
            .flat_map(|p| {
                let k = 255 - p[3] as u32;
                let f = |c: u8| ((255 - c as u32) * k / 255) as u8;
                [f(p[0]), f(p[1]), f(p[2]), 255]
            })
            .collect(),
    };
    Ok(Image { w, h, px })
}

/// Encode as JPEG. Uses grayscale when the image has no color (smaller scans).
pub fn encode_jpeg_rgba(img: &Image, quality: u8) -> Result<(Vec<u8>, bool)> {
    let gray = img.px.chunks_exact(4).all(|p| {
        let (r, g, b) = (p[0] as i16, p[1] as i16, p[2] as i16);
        (r - g).abs() <= 6 && (g - b).abs() <= 6
    });
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, quality);
    if gray {
        let l: Vec<u8> = img.px.chunks_exact(4).map(|p| p[1]).collect();
        enc.encode(&l, img.w as u16, img.h as u16, jpeg_encoder::ColorType::Luma)?;
    } else {
        enc.encode(&img.px, img.w as u16, img.h as u16, jpeg_encoder::ColorType::Rgba)?;
    }
    Ok((out, gray))
}
