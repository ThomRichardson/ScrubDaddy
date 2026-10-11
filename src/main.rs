#![cfg_attr(not(test), windows_subsystem = "windows")]

mod dict;
mod docx;
mod gui;
mod imaging;
mod names;
mod ocr;
mod pdf;
mod pdfwrite;
mod pipeline;

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--cli") {
        std::process::exit(cli(&args[1..]));
    }
    // Files dropped onto the exe icon (or "Open with") start right away.
    gui::run(args.first().map(PathBuf::from));
}

/// `scrubdaddy.exe --cli file1.pdf [file2.docx ...]` scrubs without the window.
fn cli(files: &[String]) -> i32 {
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let mut code = 0;
    for f in files {
        let path = PathBuf::from(f);
        let t = std::time::Instant::now();
        let report = |p: f32, msg: &str| eprint!("\r{:>3}% {msg:<40}", (p * 100.0) as u32);
        match pipeline::run(&path, None, &report) {
            Ok(o) => eprintln!(
                "\r{} -> {}\n     {} name(s) of {} people, {} case number(s) scrubbed in {:.1}s",
                path.display(),
                o.output.display(),
                o.names,
                o.people,
                o.case_numbers,
                t.elapsed().as_secs_f32()
            ),
            Err(e) => {
                eprintln!("\r{}: {e:#}", path.display());
                code = 1;
            }
        }
    }
    code
}
