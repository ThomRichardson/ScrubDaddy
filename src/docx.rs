//! DOCX scrubbing: names are replaced in the text itself (body, tables, headers, footers,
//! footnotes, comments, tracked changes, text boxes), author metadata is wiped, embedded
//! images are OCR'd and boxed, and leftover junk (the [trash] folder, thumbnails) is removed.

use crate::imaging::{self, BoxLine, Image};
use crate::names::{self, Detection, Hit, Line};
use crate::ocr::Ocr;
use crate::pipeline::Progress;
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;

struct Entry {
    name: String,
    data: Vec<u8>,
    deflate: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// WordprocessingML/DrawingML parts: text in w:t / a:t, grouped by paragraph.
    Word,
    /// Property parts (docProps, customXml): every element's text is its own line.
    Props,
}

struct TextNode {
    /// Byte range of the start tag and of the escaped content.
    tag: (usize, usize),
    content: (usize, usize),
    name: String,
    chars: Vec<char>,
}

/// Where each character of a detection line lives: (part index, node index, char index).
type CharRef = Option<(usize, usize, usize)>;

struct XmlPart {
    entry: usize,
    xml: String,
    nodes: Vec<TextNode>,
}

enum Src {
    Name,
    Xml(Vec<CharRef>),
    Img(usize, BoxLine),
}

const WORD_SKIP: &[&str] = &[
    "word/styles.xml",
    "word/stylesWithEffects.xml",
    "word/fontTable.xml",
    "word/settings.xml",
    "word/webSettings.xml",
    "word/numbering.xml",
];

const FORCE_REDACT: &[&str] = &["dc:creator", "cp:lastModifiedBy", "Manager", "cp:lastPrinted"];

pub fn scrub(input: &Path, name_line: Line, progress: Progress) -> Result<(Vec<u8>, Detection, Vec<Hit>)> {
    progress(0.02, "Opening document…");
    let file = std::fs::File::open(input).context("Could not read the file")?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| anyhow!("This .docx file is damaged or not a Word document."))?;
    let mut entries = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name()?.to_string();
        if f.encrypted() {
            return Err(anyhow!("This document is password protected."));
        }
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        let deflate = f.compression() != zip::CompressionMethod::Stored;
        entries.push(Entry { name, data, deflate });
    }
    if !entries.iter().any(|e| e.name == "word/document.xml") {
        return Err(anyhow!("This file does not look like a Word .docx document."));
    }
    // Old content Word keeps around for undo/autorecover is not needed and may hold private text.
    entries.retain(|e| !e.name.starts_with("[trash]/"));

    let mut lines: Vec<Line> = vec![name_line];
    let mut srcs: Vec<Src> = vec![Src::Name];
    let mut parts: Vec<XmlPart> = Vec::new();

    for (ei, e) in entries.iter().enumerate() {
        let lname = e.name.to_ascii_lowercase();
        let mode = if lname.starts_with("word/") && lname.ends_with(".xml") && !lname.starts_with("word/theme/") && !WORD_SKIP.contains(&e.name.as_str()) {
            Mode::Word
        } else if (lname.starts_with("docprops/") || lname.starts_with("customxml/")) && lname.ends_with(".xml") {
            Mode::Props
        } else {
            continue;
        };
        let Ok(xml) = String::from_utf8(e.data.clone()) else { continue };
        let pi = parts.len();
        let (nodes, plines) = scan(&xml, mode, pi);
        for (text, map) in plines {
            lines.push(Line { text, para_start: true, ocr: false });
            srcs.push(Src::Xml(map));
        }
        parts.push(XmlPart { entry: ei, xml, nodes });
    }

    // OCR embedded pictures (letterheads, scanned inserts, screenshots).
    let media: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let n = e.name.to_ascii_lowercase();
            n.starts_with("word/media/") && (n.ends_with(".png") || n.ends_with(".jpg") || n.ends_with(".jpeg"))
        })
        .map(|(i, _)| i)
        .collect();
    let mut images: HashMap<usize, Image> = HashMap::new();
    if !media.is_empty() {
        let ocr = Ocr::new()?;
        let max = Ocr::max_dimension() as usize;
        for (k, &ei) in media.iter().enumerate() {
            progress(0.05 + 0.55 * k as f32 / media.len() as f32, &format!("Reading picture {} of {}…", k + 1, media.len()));
            let e = &entries[ei];
            let img = if e.name.to_ascii_lowercase().ends_with(".png") { imaging::decode_png(&e.data) } else { imaging::decode_jpeg(&e.data) };
            let Ok(img) = img else { continue };
            if img.w < 24 || img.h < 12 || img.w > max || img.h > max {
                continue;
            }
            // Upscale small images so the OCR engine can read them.
            let (gray, gw, gh) = upscale_gray(&img);
            let Ok(found) = ocr.recognize(&gray, gw as u32, gh as u32) else { continue };
            for bl in imaging::ocr_to_lines(found, gw as f32, gh as f32) {
                lines.push(bl.to_line(true, true));
                srcs.push(Src::Img(ei, bl));
            }
            images.insert(ei, img);
        }
    }

    progress(0.65, "Finding names…");
    let det = names::detect(&lines);
    names::debug_dump(&lines, &det);

    progress(0.75, "Scrubbing text…");
    // Per text node: positions to delete and labels to insert.
    let mut edits: HashMap<(usize, usize), (HashSet<usize>, HashMap<usize, String>)> = HashMap::new();
    let mut img_boxes: HashMap<usize, Vec<[f32; 4]>> = HashMap::new();
    let mut name_hits = Vec::new();
    for h in &det.hits {
        match &srcs[h.line] {
            Src::Name => name_hits.push(h.clone()),
            Src::Xml(map) => {
                let mut first = true;
                for cref in map[h.start..h.end].iter().flatten() {
                    let (pi, ni, ci) = *cref;
                    let ed = edits.entry((pi, ni)).or_default();
                    ed.0.insert(ci);
                    if first {
                        ed.1.insert(ci, h.label());
                        first = false;
                    }
                }
            }
            Src::Img(ei, bl) => {
                if let Some(b) = bl.span_box(h.start, h.end) {
                    img_boxes.entry(*ei).or_default().push(b);
                }
            }
        }
    }

    // Rewrite XML parts.
    let mut new_data: HashMap<usize, Vec<u8>> = HashMap::new();
    for (pi, part) in parts.iter().enumerate() {
        let mut out = String::with_capacity(part.xml.len());
        let mut pos = 0;
        for (ni, node) in part.nodes.iter().enumerate() {
            let forced = FORCE_REDACT.contains(&node.name.as_str());
            let ed = edits.get(&(pi, ni));
            if ed.is_none() && !forced {
                continue;
            }
            let text: String = if forced {
                "Redacted".into()
            } else {
                let (del, ins) = ed.unwrap();
                let mut s = String::new();
                for (ci, c) in node.chars.iter().enumerate() {
                    if let Some(l) = ins.get(&ci) {
                        s.push_str(l);
                    }
                    if !del.contains(&ci) {
                        s.push(*c);
                    }
                }
                s
            };
            out.push_str(&part.xml[pos..node.tag.0]);
            let tag = &part.xml[node.tag.0..node.tag.1];
            if matches!(node.name.as_str(), "w:t" | "w:delText" | "w:instrText") && !tag.contains("xml:space") {
                let close = if tag.ends_with("/>") { tag.len() - 2 } else { tag.len() - 1 };
                out.push_str(&tag[..close]);
                out.push_str(" xml:space=\"preserve\"");
                out.push_str(&tag[close..]);
            } else {
                out.push_str(tag);
            }
            out.push_str(&escape(&text));
            pos = node.content.1;
        }
        out.push_str(&part.xml[pos..]);
        let out = scrub_attrs(&out);
        new_data.insert(part.entry, out.into_bytes());
    }

    // Burn boxes into pictures that had names.
    for (ei, boxes) in &img_boxes {
        if let Some(mut img) = images.remove(ei) {
            let (w, h) = (img.w, img.h);
            imaging::paint_boxes(&mut img.px, w, h, w * 4, 4, boxes);
            let png = entries[*ei].name.to_ascii_lowercase().ends_with(".png");
            let bytes = if png { imaging::encode_png(&img)? } else { imaging::encode_jpeg_rgba(&img, 90)?.0 };
            new_data.insert(*ei, bytes);
        }
    }

    // External links (mailto:, profile URLs) in relationship files.
    let link_parts: Vec<String> = name_parts(&det, &lines);
    for (ei, e) in entries.iter().enumerate() {
        if e.name.ends_with(".rels") {
            if let Ok(s) = std::str::from_utf8(&e.data) {
                let out = scrub_targets(s, &link_parts);
                if out != s {
                    new_data.insert(ei, out.into_bytes());
                }
            }
        }
        if e.name.eq_ignore_ascii_case("docProps/thumbnail.jpeg") || e.name.eq_ignore_ascii_case("docProps/thumbnail.jpg") {
            // The thumbnail is a picture of page one; replace it with a blank page.
            let blank = Image { w: 64, h: 82, px: vec![255; 64 * 82 * 4] };
            new_data.insert(ei, imaging::encode_jpeg_rgba(&blank, 80)?.0);
        }
    }

    progress(0.95, "Saving…");
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        for (ei, e) in entries.iter().enumerate() {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(if e.deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored })
                .last_modified_time(zip::DateTime::default());
            zw.start_file(e.name.as_str(), opts)?;
            zw.write_all(new_data.get(&ei).unwrap_or(&e.data))?;
        }
        zw.finish()?;
    }
    Ok((buf.into_inner(), det, name_hits))
}

fn upscale_gray(img: &Image) -> (Vec<u8>, usize, usize) {
    let g = img.gray();
    let f = if img.h < 600 { (1200 / img.h.max(1)).clamp(1, 4) } else { 1 };
    let max = Ocr::max_dimension() as usize;
    let f = f.min((max / img.w.max(img.h)).max(1));
    if f == 1 {
        return (g, img.w, img.h);
    }
    let (w, h) = (img.w * f, img.h * f);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let sy = y / f;
        for x in 0..w {
            out[y * w + x] = g[sy * img.w + x / f];
        }
    }
    (out, w, h)
}

/// All name words that were redacted, for scrubbing hyperlinks.
fn name_parts(det: &Detection, lines: &[Line]) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    for h in &det.hits {
        if matches!(h.kind, names::Kind::Name(..)) {
            let s: String = lines[h.line].text[h.start..h.end].iter().collect::<String>().to_lowercase();
            if s.chars().count() >= 3 {
                set.insert(s);
            }
        }
    }
    set.into_iter().collect()
}

fn scrub_targets(xml: &str, parts: &[String]) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("Target=\"") {
        let start = i + 8;
        out.push_str(&rest[..start]);
        let end = rest[start..].find('"').map_or(rest.len(), |e| start + e);
        let target = &rest[start..end];
        let lower = target.to_lowercase();
        let external = lower.starts_with("mailto:") || lower.starts_with("http") || lower.contains('@');
        if external && (lower.starts_with("mailto:") || parts.iter().any(|p| lower.contains(p.as_str()))) {
            out.push_str(if lower.starts_with("mailto:") { "mailto:redacted@example.invalid" } else { "https://redacted.invalid/" });
        } else {
            out.push_str(target);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Comment/revision authors and their initials.
fn scrub_attrs(xml: &str) -> String {
    const ATTRS: &[(&str, &str)] = &[
        (" w:author=\"", "Redacted"),
        (" w:initials=\"", "R"),
        (" w15:author=\"", "Redacted"),
        (" w15:userId=\"", "Redacted"),
        (" w15:providerId=\"", "None"),
        (" author=\"", "Redacted"),
        (" initials=\"", "R"),
    ];
    let mut s = xml.to_string();
    for (attr, val) in ATTRS {
        if !s.contains(attr) {
            continue;
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s.as_str();
        while let Some(i) = rest.find(attr) {
            let start = i + attr.len();
            out.push_str(&rest[..start]);
            out.push_str(val);
            let end = rest[start..].find('"').map_or(rest.len(), |e| start + e);
            rest = &rest[end..];
        }
        out.push_str(rest);
        s = out;
    }
    s
}

fn unescape(s: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let Some(end) = s[i..].find(';').map(|e| i + e) else {
            out.push(c);
            continue;
        };
        let ent = &s[i + 1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(ch) => {
                out.push(ch);
                while it.peek().is_some_and(|(j, _)| *j <= end) {
                    it.next();
                }
            }
            None => out.push(c),
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Find text nodes and group their characters into lines (paragraphs).
fn scan(xml: &str, mode: Mode, part: usize) -> (Vec<TextNode>, Vec<(Vec<char>, Vec<CharRef>)>) {
    let b = xml.as_bytes();
    let mut nodes: Vec<TextNode> = Vec::new();
    let mut lines = Vec::new();
    let mut stack: Vec<(Vec<char>, Vec<CharRef>)> = Vec::new();
    let mut open_text: Option<(String, usize, usize)> = None; // name, tag start, content start
    let mut last_open: Option<(String, usize, usize)> = None; // for Props mode
    let mut i = 0;
    while let Some(off) = xml[i..].find('<') {
        let lt = i + off;
        if xml[lt..].starts_with("<!--") {
            i = xml[lt..].find("-->").map_or(b.len(), |e| lt + e + 3);
            continue;
        }
        if xml[lt..].starts_with("<?") {
            i = xml[lt..].find("?>").map_or(b.len(), |e| lt + e + 2);
            continue;
        }
        if xml[lt..].starts_with("<![CDATA[") {
            i = xml[lt..].find("]]>").map_or(b.len(), |e| lt + e + 3);
            continue;
        }
        let Some(gt) = xml[lt..].find('>').map(|e| lt + e) else { break };
        let tag = &xml[lt + 1..gt];
        let closing = tag.starts_with('/');
        let self_close = tag.ends_with('/');
        let name: String = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_string();
        let mut push_node = |name: String, tag: (usize, usize), content: (usize, usize), stack: &mut Vec<(Vec<char>, Vec<CharRef>)>, lines: &mut Vec<(Vec<char>, Vec<CharRef>)>| {
            let chars = unescape(&xml[content.0..content.1]);
            let ni = nodes.len();
            let refs: Vec<CharRef> = (0..chars.len()).map(|ci| Some((part, ni, ci))).collect();
            match stack.last_mut() {
                Some(top) if mode == Mode::Word => {
                    top.0.extend(&chars);
                    top.1.extend(refs);
                }
                _ => {
                    if chars.iter().any(|c| !c.is_whitespace()) {
                        lines.push((chars.clone(), refs));
                    }
                }
            }
            nodes.push(TextNode { tag, content, name, chars });
        };
        match mode {
            Mode::Word => {
                let is_p = name == "w:p" || name == "a:p";
                let is_t = matches!(name.as_str(), "w:t" | "w:delText" | "w:instrText" | "a:t");
                if is_p && !closing && !self_close {
                    stack.push((Vec::new(), Vec::new()));
                } else if is_p && closing {
                    if let Some(p) = stack.pop() {
                        if p.0.iter().any(|c| !c.is_whitespace()) {
                            lines.push(p);
                        }
                    }
                } else if is_t && !closing && !self_close {
                    open_text = Some((name.clone(), lt, gt + 1));
                } else if is_t && closing {
                    if let Some((n, ts, cs)) = open_text.take() {
                        push_node(n, (ts, cs), (cs, lt), &mut stack, &mut lines);
                    }
                } else if !closing && matches!(name.as_str(), "w:tab" | "w:br" | "w:cr" | "a:br" | "w:noBreakHyphen") {
                    if let Some(top) = stack.last_mut() {
                        top.0.push(if name == "w:noBreakHyphen" { '-' } else { ' ' });
                        top.1.push(None);
                    }
                }
            }
            Mode::Props => {
                if !closing && !self_close {
                    last_open = Some((name.clone(), lt, gt + 1));
                } else if closing {
                    if let Some((n, ts, cs)) = last_open.take() {
                        if n == name && !xml[cs..lt].contains('<') {
                            push_node(n, (ts, cs), (cs, lt), &mut stack, &mut lines);
                        }
                    }
                } else {
                    last_open = None;
                }
            }
        }
        i = gt + 1;
    }
    (nodes, lines)
}
