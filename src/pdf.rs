//! PDF scrubbing: read the text layer + OCR every page, detect names, then rebuild each page
//! as a flattened image with burned-in black boxes (so nothing redacted can be recovered).

use crate::dict;
use crate::imaging::{self, BoxLine, Image};
use crate::names::{self, Detection, Hit, Line};
use crate::ocr::Ocr;
use crate::pdfwrite;
use crate::pipeline::Progress;
use anyhow::{anyhow, Context, Result};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};

/// OCR passes per page. Windows OCR reads small or faxed text very differently depending on
/// scale, so each page is read at several resolutions and names found in any pass are boxed.
/// The first pass also feeds the invisible text layer.
const OCR_DPIS: &[f32] = &[300.0, 200.0];
const OUT_DPI: f32 = 200.0;
const JPEG_QUALITY: u8 = 82;

static PDFIUM_DLL: &[u8] = include_bytes!("../vendor/pdfium/pdfium.dll");

/// Bind the PDFium library embedded in the exe (extracted once to %LOCALAPPDATA%).
pub fn pdfium() -> Result<&'static Pdfium> {
    static P: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();
    P.get_or_init(|| {
        let path = dll_path().map_err(|e| e.to_string())?;
        Pdfium::bind_to_library(&path)
            .map(Pdfium::new)
            .map_err(|e| format!("Could not load PDF engine: {e:?}"))
    })
    .as_ref()
    .map_err(|e| anyhow!("{e}"))
}

fn dll_path() -> Result<PathBuf> {
    if let Some(local) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("pdfium.dll"))) {
        if local.exists() {
            return Ok(local);
        }
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("ScrubDaddy");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("pdfium-7881-{}.dll", PDFIUM_DLL.len()));
    let ok = std::fs::metadata(&path).is_ok_and(|m| m.len() == PDFIUM_DLL.len() as u64);
    if !ok {
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, PDFIUM_DLL)?;
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(path)
}

struct PageData {
    w_pt: f32,
    h_pt: f32,
    /// Text-layer lines, then first-pass OCR lines, then extra-pass OCR lines.
    lines: Vec<BoxLine>,
    /// Lines before this index make up the output's invisible text layer.
    layer_end: usize,
}

pub fn scrub(input: &Path, name_line: Line, progress: Progress) -> Result<(Vec<u8>, Detection, Vec<Hit>)> {
    let pdfium = pdfium()?;
    progress(0.01, "Opening PDF…");
    let data = std::fs::read(input).context("Could not read the file")?;
    let doc = pdfium.load_pdf_from_byte_slice(&data, None).map_err(|e| match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            anyhow!("This PDF is password protected. Remove the password and try again.")
        }
        e => anyhow!("Could not open PDF: {e:?}"),
    })?;
    let n = doc.pages().len() as usize;
    if n == 0 {
        return Err(anyhow!("The PDF has no pages."));
    }

    // ---- pass 1: text layer + OCR (OCR runs on a worker pool while PDFium renders) ----
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4).min(n);
    let (job_tx, job_rx) = sync_channel::<((usize, usize), Vec<u8>, u32, u32)>(workers);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = channel::<((usize, usize), Result<Vec<BoxLine>>)>();
    let mut handles = Vec::new();
    for _ in 0..workers {
        let rx = job_rx.clone();
        let tx = res_tx.clone();
        handles.push(std::thread::spawn(move || {
            let ocr = Ocr::new();
            loop {
                let job = rx.lock().unwrap().recv();
                let Ok((i, gray, w, h)) = job else { break };
                let r = match &ocr {
                    Ok(o) => o.recognize(&gray, w, h).map(|l| imaging::ocr_to_lines(l, w as f32, h as f32)),
                    Err(e) => Err(anyhow!("{e}")),
                };
                if tx.send((i, r)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(res_tx);

    let max_dim = Ocr::max_dimension() as f32;
    // Developer aid for tuning: SCRUBDADDY_OCR_DPI="300,200" overrides the OCR passes.
    let dpis: Vec<f32> = std::env::var("SCRUBDADDY_OCR_DPI")
        .ok()
        .map(|v| v.split(',').filter_map(|d| d.trim().parse().ok()).collect::<Vec<f32>>())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| OCR_DPIS.to_vec());
    let passes = dpis.len();
    let mut pages: Vec<PageData> = Vec::with_capacity(n);
    let mut ocr_out: Vec<Vec<Vec<BoxLine>>> = (0..n).map(|_| vec![Vec::new(); passes]).collect();
    let mut done = 0usize;
    let mut first_err: Option<anyhow::Error> = None;
    let total = n * passes;
    let mut collect = |r: ((usize, usize), Result<Vec<BoxLine>>), done: &mut usize| {
        *done += 1;
        let ((page, pass), res) = r;
        match res {
            Ok(l) => ocr_out[page][pass] = l,
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
        let shown = (*done * n).div_ceil(total).max(1);
        progress(0.02 + 0.6 * *done as f32 / total as f32, &format!("Reading page {shown} of {n}…"));
    };
    for i in 0..n {
        let page = doc.pages().get(i as u16 as PdfPageIndex).map_err(|e| anyhow!("page {}: {e:?}", i + 1))?;
        let (w_pt, h_pt) = (page.width().value, page.height().value);
        let lines = text_lines(&page, w_pt, h_pt);
        let layer_end = lines.len();
        pages.push(PageData { w_pt, h_pt, lines, layer_end });
        for (pass, dpi) in dpis.iter().enumerate() {
            let scale = (dpi / 72.0).min(max_dim / w_pt.max(h_pt));
            let (gray, bw, bh) = render_gray(&page, scale)?;
            job_tx.send(((i, pass), gray, bw, bh)).map_err(|_| anyhow!("OCR worker stopped"))?;
            while let Ok(r) = res_rx.try_recv() {
                collect(r, &mut done);
            }
        }
    }
    drop(job_tx);
    for r in res_rx.iter() {
        collect(r, &mut done);
    }
    for h in handles {
        let _ = h.join();
    }
    if let Some(e) = first_err {
        // OCR failing on every page means we cannot see scanned text; refuse rather than
        // silently producing an unredacted document.
        if pages.iter().all(|p| p.lines.is_empty()) {
            return Err(e);
        }
    }

    // ---- detect ----
    progress(0.63, "Finding names…");
    let mut lines: Vec<Line> = vec![name_line];
    let mut owners: Vec<(usize, usize)> = vec![(usize::MAX, 0)]; // (page, line index in page)
    for (pi, p) in pages.iter_mut().enumerate() {
        let text_count = p.lines.len();
        let text_layer = p.lines.clone();
        for (pass, found) in std::mem::take(&mut ocr_out[pi]).into_iter().enumerate() {
            p.lines.extend(imaging::dedupe_ocr(&text_layer, found));
            if pass == 0 {
                p.layer_end = p.lines.len();
            }
        }
        for (li, bl) in p.lines.iter().enumerate() {
            lines.push(bl.to_line(false, li >= text_count));
            owners.push((pi, li));
        }
    }
    let mut det = names::detect(&lines);
    names::debug_dump(&lines, &det);
    let mut page_hits: Vec<Vec<Vec<&Hit>>> = pages.iter().map(|p| vec![Vec::new(); p.lines.len()]).collect();
    let mut name_hits = Vec::new();
    for h in &det.hits {
        let (pi, li) = owners[h.line];
        if pi == usize::MAX {
            name_hits.push(h.clone());
        } else {
            page_hits[pi][li].push(h);
        }
    }
    // The OCR passes see the same names; count each redacted spot on the page once.
    let (mut names_n, mut cases_n) = (0, 0);
    for (pi, p) in pages.iter().enumerate() {
        let mut seen: Vec<[f32; 4]> = Vec::new();
        for (li, bl) in p.lines.iter().enumerate() {
            for h in &page_hits[pi][li] {
                let Some(b) = bl.span_box(h.start, h.end) else { continue };
                if imaging::mostly_covered(b, &seen) {
                    continue;
                }
                seen.push(b);
                match h.kind {
                    names::Kind::CaseNumber => cases_n += 1,
                    names::Kind::Name(..) => names_n += 1,
                    names::Kind::Email => {}
                }
            }
        }
    }
    det.names = names_n;
    det.case_numbers = cases_n;

    // ---- pass 2: render, burn in boxes, encode (encoding in parallel) ----
    let (enc_tx, enc_rx) = sync_channel::<(usize, Image, Vec<[f32; 4]>)>(workers);
    let enc_rx = Arc::new(Mutex::new(enc_rx));
    let (out_tx, out_rx) = channel::<(usize, Result<(Vec<u8>, bool, usize, usize)>)>();
    let mut handles = Vec::new();
    for _ in 0..workers {
        let rx = enc_rx.clone();
        let tx = out_tx.clone();
        handles.push(std::thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            let Ok((i, mut img, boxes)) = job else { break };
            let (w, h) = (img.w, img.h);
            imaging::paint_boxes(&mut img.px, w, h, w * 4, 4, &boxes);
            let r = imaging::encode_jpeg_rgba(&img, JPEG_QUALITY).map(|(j, g)| (j, g, w, h));
            if tx.send((i, r)).is_err() {
                break;
            }
        }));
    }
    drop(out_tx);
    let mut out_pages: Vec<Option<pdfwrite::Page>> = (0..n).map(|_| None).collect();
    let mut pending: Vec<Vec<(String, [f32; 4])>> = Vec::with_capacity(n);
    let mut written = 0usize;
    let mut finish = |r: (usize, Result<(Vec<u8>, bool, usize, usize)>),
                      pending: &mut Vec<Vec<(String, [f32; 4])>>,
                      written: &mut usize|
     -> Result<()> {
        let (i, res) = r;
        let (jpeg, gray, px_w, px_h) = res?;
        let words = std::mem::take(&mut pending[i]);
        out_pages[i] = Some(pdfwrite::Page {
            w_pt: pages[i].w_pt,
            h_pt: pages[i].h_pt,
            jpeg,
            px_w,
            px_h,
            gray,
            words,
        });
        *written += 1;
        progress(0.65 + 0.33 * *written as f32 / n as f32, &format!("Scrubbing page {} of {n}…", *written));
        Ok(())
    };
    for i in 0..n {
        let page = doc.pages().get(i as u16 as PdfPageIndex).map_err(|e| anyhow!("page {}: {e:?}", i + 1))?;
        let img = render_rgba(&page, OUT_DPI / 72.0)?;
        let mut boxes = Vec::new();
        // Handwritten values in name/claim fields that OCR could not read.
        for region in handwriting_fields(&pages[i].lines) {
            if let Some(b) = imaging::ink_box(&img, region) {
                if std::env::var_os("SCRUBDADDY_DEBUG").is_some() {
                    eprintln!("handwriting box on page {}: {:?}", i + 1, b.map(|v| (v * 100.0).round() / 100.0));
                }
                boxes.push(b);
            }
        }
        let mut words = Vec::new();
        for (li, bl) in pages[i].lines.iter().enumerate() {
            let hits = &page_hits[i][li];
            boxes.extend(imaging::hit_boxes(bl, hits));
            if li < pages[i].layer_end {
                words.extend(redacted_words(bl, hits));
            }
        }
        // A word sitting under any black box (e.g. a misread name another pass caught) never
        // makes it into the text layer.
        words.retain(|(_, w)| !imaging::mostly_covered(*w, &boxes));
        pending.push(words);
        enc_tx.send((i, img, boxes)).map_err(|_| anyhow!("encoder stopped"))?;
        while let Ok(r) = out_rx.try_recv() {
            finish(r, &mut pending, &mut written)?;
        }
    }
    drop(enc_tx);
    for r in out_rx.iter() {
        finish(r, &mut pending, &mut written)?;
    }
    for h in handles {
        let _ = h.join();
    }
    progress(0.99, "Saving…");
    let out_pages: Vec<pdfwrite::Page> = out_pages.into_iter().map(|p| p.expect("page encoded")).collect();
    Ok((pdfwrite::write(&out_pages), det, name_hits))
}

/// Field areas next to printed name/claim labels ("PATIENT NAME:", "Claim #", "Purpose for
/// faxing patient information:") whose value OCR could not read, typically handwriting. The
/// area runs from the label to the next printed label on the same row.
fn handwriting_fields(lines: &[BoxLine]) -> Vec<[f32; 4]> {
    const LABELS: &[&str] = &[
        "patient name", "name:", "claimant", "claim #", "claim#", "claim no", "claim number", "claim:",
        "for faxing",
    ];
    // Labels for people who stay visible, or for organizations.
    const NOT: &[&str] = &[
        "physician", "doctor", "provider", "counselor", "vrc", "evaluator", "analyst", "therapist",
        "attorney", "employer", "company", "facility", "clinic", "contact", "sender", "from:",
    ];
    let words: Vec<(String, [f32; 4])> = lines.iter().flat_map(|l| l.words()).collect();
    let typed_value = |after: &[char]| {
        let after: String = after.iter().collect();
        after.split_whitespace().any(|t| {
            let letters: String = t.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
            let digits = t.chars().filter(char::is_ascii_digit).count();
            let wordlike = letters.chars().count() >= 3
                && t.chars().all(|c| c.is_alphabetic() || ",.'-".contains(c))
                && (dict::common_rank(&letters).is_some() || dict::first(&letters) >= 2 || dict::sur(&letters) >= 2);
            wordlike || digits >= 4
        })
    };
    let mut out = Vec::new();
    for bl in lines {
        let lower: Vec<char> = bl.text.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        let line: String = lower.iter().collect();
        if NOT.iter().any(|w| line.contains(w)) {
            continue;
        }
        for pat in LABELS {
            let pc: Vec<char> = pat.chars().collect();
            if lower.len() < pc.len() {
                continue;
            }
            let Some(start) = (0..=lower.len() - pc.len()).find(|&i| lower[i..i + pc.len()] == pc[..]) else { continue };
            // The label runs to its colon if there is one nearby ("...patient information:"),
            // otherwise to the end of the word (or line, for the wordy "purpose" labels).
            let mut end = start + pc.len();
            if let Some(c) = lower[end..].iter().take(40).position(|&c| c == ':') {
                end += c + 1;
            } else if *pat == "for faxing" {
                end = lower.len();
            } else {
                while end < lower.len() && !lower[end].is_whitespace() {
                    end += 1;
                }
            }
            if typed_value(&bl.text[end..]) {
                break; // a printed/typed value is there; text detection handles it
            }
            let Some(lb) = bl.span_box(start, end) else { break };
            let lh = lb[3] - lb[1];
            // Handwriting is taller than print and often sits above or below the label baseline.
            let mut region = [lb[2] + 0.004, lb[1] - 1.6 * lh, (lb[2] + 0.55).min(0.98), lb[3] + 1.3 * lh];
            // Stop at the next printed label or ordinary printed word on the same row
            // ("*MRN:", "Evaluation Date(s):"). Handwriting never OCRs as real words.
            for (t, b) in &words {
                let cy = (b[1] + b[3]) / 2.0;
                let letters: String = t.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
                let printed = letters.chars().count() >= 4 && dict::common_rank(&letters).is_some();
                if (t.contains(':') || printed) && cy > lb[1] - 0.5 * lh && cy < lb[3] + 0.5 * lh && b[0] > region[0] + 0.03 {
                    region[2] = region[2].min(b[0] - 0.004);
                }
            }
            if region[2] > region[0] + 0.02 {
                out.push(region);
            }
            break;
        }
    }
    out
}

/// Words of a line for the invisible text layer, with redacted characters replaced by labels.
fn redacted_words(bl: &BoxLine, hits: &[&Hit]) -> Vec<(String, [f32; 4])> {
    if hits.is_empty() {
        return bl.words();
    }
    // Redacted characters are dropped from the text layer entirely.
    let mut masked = bl.clone();
    for h in hits {
        masked.text[h.start..h.end].fill('\u{0}');
    }
    masked
        .words()
        .into_iter()
        .filter_map(|(w, b)| {
            let s: String = w.chars().filter(|&c| c != '\u{0}').collect();
            (!s.trim().is_empty()).then_some((s, b))
        })
        .collect()
}

/// Characters of the PDF text layer, grouped into lines with normalized boxes.
fn text_lines(page: &PdfPage, w_pt: f32, h_pt: f32) -> Vec<BoxLine> {
    let Ok(text) = page.text() else { return Vec::new() };
    let cfg = PdfRenderConfig::new().scale_page_by_factor(10.0);
    let map = |x: f32, y: f32| page.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &cfg).ok();
    let (Some(o), Some(ax), Some(ay)) = (map(0.0, 0.0), map(1000.0, 0.0), map(0.0, 1000.0)) else {
        return Vec::new();
    };
    // Affine page->device transform, normalized by the rendered size.
    let (dw, dh) = (w_pt * 10.0, h_pt * 10.0);
    let a = ((ax.0 - o.0) as f32 / 1000.0, (ax.1 - o.1) as f32 / 1000.0);
    let b = ((ay.0 - o.0) as f32 / 1000.0, (ay.1 - o.1) as f32 / 1000.0);
    let tf = |x: f32, y: f32| ((o.0 as f32 + a.0 * x + b.0 * y) / dw, (o.1 as f32 + a.1 * x + b.1 * y) / dh);

    let mut lines = Vec::new();
    let mut cur = BoxLine::default();
    let mut prev: Option<[f32; 4]> = None;
    for ch in text.chars().iter() {
        let c = ch.unicode_char().unwrap_or(' ');
        if c == '\r' || c == '\n' {
            if !cur.text.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        if c.is_whitespace() || c.is_control() || ch.is_generated().unwrap_or(false) && c == ' ' {
            if cur.text.last().is_some_and(|l| *l != ' ') {
                cur.push(' ', None);
            }
            continue;
        }
        let Ok(r) = ch.loose_bounds() else { continue };
        let (x0, y0) = tf(r.left().value, r.top().value);
        let (x1, y1) = tf(r.right().value, r.bottom().value);
        let nb = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
        if let Some(p) = prev {
            let ph = (p[3] - p[1]).max(1e-4);
            let new_line = ((nb[1] + nb[3]) / 2.0 - (p[1] + p[3]) / 2.0).abs() > ph * 0.7;
            let gap = nb[0] - p[2];
            if new_line || gap < -ph * 2.0 {
                lines.push(std::mem::take(&mut cur));
            } else if gap > ph * 0.35 && cur.text.last().is_some_and(|l| *l != ' ') {
                cur.push(' ', None);
            }
        }
        cur.push(c, Some(nb));
        prev = Some(nb);
    }
    if !cur.text.is_empty() {
        lines.push(cur);
    }
    lines.retain(|l| l.text.iter().any(|c| !c.is_whitespace()));
    lines
}

fn render(page: &PdfPage, scale: f32) -> Result<(Vec<u8>, usize, usize)> {
    let cfg = PdfRenderConfig::new()
        .scale_page_by_factor(scale)
        .render_form_data(true)
        .render_annotations(true)
        .set_clear_color(PdfColor::WHITE);
    let bmp = page.render_with_config(&cfg).map_err(|e| anyhow!("render failed: {e:?}"))?;
    let (w, h) = (bmp.width() as usize, bmp.height() as usize);
    let raw = bmp.as_raw_bytes();
    let stride = raw.len() / h.max(1);
    // BGRA -> RGBA, tightly packed.
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for p in raw[y * stride..y * stride + w * 4].chunks_exact(4) {
            px.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    Ok((px, w, h))
}

fn render_rgba(page: &PdfPage, scale: f32) -> Result<Image> {
    let (px, w, h) = render(page, scale)?;
    Ok(Image { w, h, px })
}

fn render_gray(page: &PdfPage, scale: f32) -> Result<(Vec<u8>, u32, u32)> {
    let img = render_rgba(page, scale)?;
    Ok((img.gray(), img.w as u32, img.h as u32))
}
