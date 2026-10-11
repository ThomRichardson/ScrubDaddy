//! Picks the right scrubber for a file and saves `<name>-scrubbed.<ext>`, either into `out_dir`
//! (the window uses a private temp folder until the user drags the result somewhere) or next
//! to the original (command line).

use crate::names::{Hit, Line};
use crate::{docx, pdf};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

pub type Progress<'a> = &'a (dyn Fn(f32, &str) + Sync);

pub struct Outcome {
    pub output: PathBuf,
    pub names: usize,
    pub people: usize,
    pub case_numbers: usize,
}

pub fn supported(path: &Path) -> bool {
    matches!(ext(path).as_str(), "pdf" | "docx")
}

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn run(input: &Path, out_dir: Option<&Path>, progress: Progress) -> Result<Outcome> {
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // The file name is scanned too: it often contains the person's name.
    let name_line = Line::new(&stem.replace(['_', '-', '.'], " "), true, false);
    let ext = ext(input);
    let (bytes, det, name_hits) = match ext.as_str() {
        "pdf" => pdf::scrub(input, name_line, progress)?,
        "docx" => docx::scrub(input, name_line, progress)?,
        "doc" => return Err(anyhow!("Old .doc files aren't supported. Open it in Word and Save As .docx first.")),
        _ => return Err(anyhow!("Drop a PDF or Word (.docx) file.")),
    };
    let out_name = format!("{}-scrubbed.{ext}", scrub_stem(&stem, &name_hits));
    let output = save(input, out_dir, &out_name, &bytes)?;
    progress(1.0, "Done");
    Ok(Outcome { output, names: det.names, people: det.people, case_numbers: det.case_numbers })
}

/// Replace names/case numbers found in the file name with "REDACTED".
fn scrub_stem(stem: &str, hits: &[Hit]) -> String {
    let chars: Vec<char> = stem.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut sorted: Vec<&Hit> = hits.iter().collect();
    sorted.sort_by_key(|h| h.start);
    for h in sorted {
        if h.start < i {
            continue;
        }
        out.extend(&chars[i..h.start]);
        if !out.ends_with("REDACTED") && !out.trim_end().ends_with("REDACTED") {
            out.push_str("REDACTED");
        } else {
            // Collapse "REDACTED REDACTED" into one.
            let t = out.trim_end().len();
            out.truncate(t);
        }
        i = h.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    let out = out.trim().to_string();
    if out.is_empty() { "document".into() } else { out }
}

fn save(input: &Path, out_dir: Option<&Path>, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let dirs = match out_dir {
        Some(d) => vec![d.to_path_buf()],
        None => vec![
            input.parent().map(Path::to_path_buf).unwrap_or_default(),
            std::env::temp_dir().join("ScrubDaddy"),
        ],
    };
    let mut last_err = None;
    for dir in dirs {
        let _ = std::fs::create_dir_all(&dir);
        let path = unique(&dir.join(name));
        let tmp = path.with_extension("partial");
        match std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)) {
            Ok(()) => return Ok(path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                last_err = Some(e);
            }
        }
    }
    Err(anyhow!("Could not save the scrubbed file: {}", last_err.map(|e| e.to_string()).unwrap_or_default()))
}

fn unique(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().unwrap_or_default().to_string_lossy().into_owned();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}
