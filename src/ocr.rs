//! Text recognition with the OCR engine built into Windows 10/11 (Windows.Media.Ocr).
//! No external OCR install needed; runs fully offline.

use anyhow::{anyhow, Result};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// A recognized word; the box is in pixels of the image passed to `recognize`.
pub struct Word {
    pub text: String,
    pub rect: [f32; 4], // x0, y0, x1, y1
}

pub struct Ocr {
    engine: OcrEngine,
}

impl Ocr {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .or_else(|_| {
                OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&HSTRING::from("en-US"))?)
            })
            .map_err(|_| {
                anyhow!("Windows OCR is not available. Install an OCR language pack in Settings > Time & language > Language.")
            })?;
        Ok(Ocr { engine })
    }

    pub fn max_dimension() -> u32 {
        OcrEngine::MaxImageDimension().unwrap_or(4096)
    }

    /// Recognize an 8-bit grayscale image (tightly packed). Returns lines of words.
    pub fn recognize(&self, gray: &[u8], w: u32, h: u32) -> Result<Vec<Vec<Word>>> {
        let lines = self.recognize_once(gray, w, h)?;
        let count: usize = lines.iter().map(|l| l.len()).sum();
        if count >= 12 {
            return Ok(lines);
        }
        // Sideways scans: try the page rotated both ways and keep whichever reads best.
        let mut best = (count, lines);
        for cw in [true, false] {
            let (rot, rw, rh) = rotate(gray, w, h, cw);
            let mut rl = self.recognize_once(&rot, rw, rh)?;
            let n: usize = rl.iter().map(|l| l.len()).sum();
            if n > best.0 * 2 && n >= 8 {
                for line in &mut rl {
                    for word in line.iter_mut() {
                        word.rect = unrotate(word.rect, w, h, cw);
                    }
                }
                best = (n, rl);
            }
        }
        Ok(best.1)
    }

    fn recognize_once(&self, gray: &[u8], w: u32, h: u32) -> Result<Vec<Vec<Word>>> {
        let buf = CryptographicBuffer::CreateFromByteArray(gray)?;
        let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Gray8, w as i32, h as i32)?;
        let result = self.engine.RecognizeAsync(&bmp)?.join()?;
        let mut out = Vec::new();
        for line in result.Lines()? {
            let mut words = Vec::new();
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                words.push(Word {
                    text: word.Text()?.to_string_lossy(),
                    rect: [r.X, r.Y, r.X + r.Width, r.Y + r.Height],
                });
            }
            if !words.is_empty() {
                out.push(words);
            }
        }
        Ok(out)
    }
}

fn rotate(src: &[u8], w: u32, h: u32, clockwise: bool) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let mut dst = vec![0u8; w * h];
    // Rotated image is h wide, w tall.
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for (x, &p) in row.iter().enumerate() {
            let (nx, ny) = if clockwise { (h - 1 - y, x) } else { (y, w - 1 - x) };
            dst[ny * h + nx] = p;
        }
    }
    (dst, h as u32, w as u32)
}

/// Map a rectangle from the rotated image back to the original image.
fn unrotate(r: [f32; 4], w: u32, h: u32, clockwise: bool) -> [f32; 4] {
    let (w, h) = (w as f32, h as f32);
    let [x0, y0, x1, y1] = r;
    if clockwise {
        // rotated (nx, ny) = (h-1-y, x)  =>  x = ny, y = h-1-nx
        [y0, h - x1, y1, h - x0]
    } else {
        // rotated (nx, ny) = (y, w-1-x)  =>  x = w-1-ny, y = nx
        [w - y1, x0, w - y0, x1]
    }
}
