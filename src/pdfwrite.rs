//! Minimal PDF writer: one JPEG image per page (redactions are already burned into its
//! pixels) plus an invisible text layer of the remaining words, so the scrubbed document stays
//! searchable/readable by AI tools. Nothing from the original file is copied: no metadata,
//! no hidden text, no attachments, no vector content that could sit under a box.

use flate2::{write::ZlibEncoder, Compression};
use std::fmt::Write as _;
use std::io::Write;

pub struct Page {
    pub w_pt: f32,
    pub h_pt: f32,
    pub jpeg: Vec<u8>,
    pub px_w: usize,
    pub px_h: usize,
    pub gray: bool,
    /// Invisible text: (text, normalized box). Redacted words are not included.
    pub words: Vec<(String, [f32; 4])>,
}

/// Helvetica advance widths (1/1000 em) for ASCII 32..=126.
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
    722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
    667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
    556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
    500, 334, 260, 334, 584,
];

fn win_ansi(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        '\u{a0}'..='\u{ff}' => c as u32 as u8,
        '€' => 0x80,
        '‚' => 0x82,
        '„' => 0x84,
        '…' => 0x85,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '™' => 0x99,
        _ => b'?',
    }
}

fn text_width(bytes: &[u8]) -> f32 {
    bytes
        .iter()
        .map(|&b| if (32..=126).contains(&b) { HELV[(b - 32) as usize] } else { 556 } as f32)
        .sum::<f32>()
        / 1000.0
}

fn pdf_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(b as char);
            }
            32..=126 => s.push(b as char),
            _ => {
                let _ = write!(s, "\\{:03o}", b);
            }
        }
    }
    s.push(')');
    s
}

fn content_stream(p: &Page) -> Vec<u8> {
    let (w, h) = (p.w_pt, p.h_pt);
    let mut s = String::new();
    let _ = writeln!(s, "q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q");
    let put = |s: &mut String, text: &str, b: [f32; 4]| {
        let bytes: Vec<u8> = text.chars().map(win_ansi).collect();
        let tw = text_width(&bytes);
        if tw <= 0.0 {
            return;
        }
        let bx = b[0] * w;
        let bw = (b[2] - b[0]) * w;
        let bh = (b[3] - b[1]) * h;
        let by = (1.0 - b[3]) * h;
        let size = (bh * 0.85).max(1.0);
        let tz = (100.0 * bw / (tw * size)).clamp(5.0, 1000.0);
        let y = by + bh * 0.22;
        // Render mode 3 = invisible text.
        let _ = writeln!(s, "BT 3 Tr /F1 {size:.2} Tf {tz:.1} Tz 1 0 0 1 {bx:.2} {y:.2} Tm {} Tj ET", pdf_string(&bytes));
    };
    for (t, b) in &p.words {
        put(&mut s, t, *b);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = z.write_all(s.as_bytes());
    z.finish().unwrap_or_default()
}

pub fn write(pages: &[Page]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    // Object numbering: 1 catalog, 2 pages, 3 font, 4 info, then 3 per page.
    let page_obj = |i: usize| 5 + i * 3;
    let obj = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, num: usize, body: &[u8]| {
        if offsets.len() < num {
            offsets.resize(num, 0);
        }
        offsets[num - 1] = out.len();
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, &mut offsets, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..pages.len()).map(|i| format!("{} 0 R ", page_obj(i))).collect();
    obj(&mut out, &mut offsets, 2, format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", pages.len()).as_bytes());
    obj(
        &mut out,
        &mut offsets,
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    obj(&mut out, &mut offsets, 4, b"<< /Producer (ScrubDaddy) >>");
    for (i, p) in pages.iter().enumerate() {
        let n = page_obj(i);
        let content = content_stream(p);
        obj(
            &mut out,
            &mut offsets,
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /Font << /F1 3 0 R >> /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>",
                p.w_pt, p.h_pt, n + 2, n + 1
            )
            .as_bytes(),
        );
        let mut body = format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", content.len()).into_bytes();
        body.extend_from_slice(&content);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offsets, n + 1, &body);
        let mut body = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{} /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            p.px_w,
            p.px_h,
            if p.gray { "DeviceGray" } else { "DeviceRGB" },
            p.jpeg.len()
        )
        .into_bytes();
        body.extend_from_slice(&p.jpeg);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, &mut offsets, n + 2, &body);
    }
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for o in &offsets {
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(
        x,
        "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        offsets.len() + 1
    );
    out.extend_from_slice(x.as_bytes());
    out
}
