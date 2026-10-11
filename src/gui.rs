//! The little drop window: drop a file on the folder, watch the sponge scrub, then drag the
//! finished folder out to wherever the scrubbed file should go. Until it is dragged out, the
//! scrubbed file only exists in a private temp folder that is deleted when the app closes.

use crate::imaging::{self, Image};
use crate::pipeline::{self, Outcome};
use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::core::{implement, w, Ref, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::DragDetect;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_APP_PROGRESS: u32 = WM_APP + 1;
const WM_APP_DONE: u32 = WM_APP + 2;
const TIMER_ANIM: usize = 1;

// Logical (96 DPI) layout.
const CLIENT_W: i32 = 300;
const CLIENT_H: i32 = 378;
const ZONE: (i32, i32, i32, i32) = (14, 14, 286, 254);
const FOLDER: i32 = 168;

const BG: u32 = rgb(255, 250, 235);
const ZONE_BG: u32 = rgb(255, 255, 255);
const ZONE_HOVER: u32 = rgb(255, 243, 200);
const BORDER: u32 = rgb(236, 214, 150);
const ACCENT: u32 = rgb(255, 184, 0);
const TEXT: u32 = rgb(62, 39, 35);
const MUTED: u32 = rgb(141, 110, 99);
const TRACK: u32 = rgb(241, 227, 184);
const ERROR: u32 = rgb(198, 40, 40);
const GOOD: u32 = rgb(34, 140, 70);

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

static FOLDER_PNG: &[u8] = include_bytes!("../assets/folder.png");
static FOLDER_DONE_PNG: &[u8] = include_bytes!("../assets/folder_done.png");
static SPONGE_PNG: &[u8] = include_bytes!("../assets/sponge.png");

enum State {
    Idle(Option<String>),
    Working { progress: f32, msg: String },
    /// Scrubbed files waiting in the temp folder `dir` to be dragged out together.
    Done { paths: Vec<PathBuf>, dir: PathBuf, summary: String, saved: bool },
    Error(String),
}

struct Bmp {
    hbm: HBITMAP,
    w: i32,
    h: i32,
}

struct Res {
    folder: Bmp,
    done: Bmp,
    sponge: Bmp,
    big: HFONT,
    small: HFONT,
    label: HFONT,
}

struct App {
    hwnd: HWND,
    jobs: u32,
    dpi: u32,
    state: State,
    hover: bool,
    tick: u32,
    res: Option<Res>,
    sources: Sources,
}

struct Sources {
    folder: Image,
    done: Image,
    sponge: Image,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn s(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

/// Private scratch space for this run of the app.
fn session_dir() -> PathBuf {
    std::env::temp_dir().join("ScrubDaddy").join(format!("session-{}", std::process::id()))
}

/// Remove scratch folders left behind by earlier runs that did not exit cleanly.
fn clean_stale_sessions() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir().join("ScrubDaddy")) else { return };
    for e in rd.flatten() {
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if e.file_name().to_string_lossy().starts_with("session-") && age.is_some_and(|d| d.as_secs() > 12 * 3600) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `WM_COPYDATA` tag for "scrub this file", sent by a second launch to the running window.
const COPYDATA_OPEN: usize = 0x5344_4F50;

/// Keep to one window. If Scrub Daddy is already running (or still starting, e.g. while an
/// antivirus scans a new exe), hand it the file, bring it forward and return true. A copy that
/// is hung or never shows a window is ignored, so a fresh one can start instead.
fn forward_to_running(initial: &[PathBuf]) -> bool {
    unsafe {
        let created = CreateMutexW(None, false, w!("Local\\ScrubDaddy.SingleInstance"));
        if created.is_err() || GetLastError() != ERROR_ALREADY_EXISTS {
            // We are the first copy; the mutex handle lives as long as the process.
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let hwnd = loop {
            match FindWindowW(w!("ScrubDaddyWindow"), None) {
                Ok(h) if !h.is_invalid() => break h,
                _ if Instant::now() > deadline => return false,
                _ => std::thread::sleep(Duration::from_millis(250)),
            }
        };
        // NUL-separated paths, double-NUL terminated.
        let mut payload: Vec<u16> = Vec::new();
        for p in initial {
            payload.extend(p.as_os_str().encode_wide());
            payload.push(0);
        }
        payload.push(0);
        let cds = COPYDATASTRUCT { dwData: COPYDATA_OPEN, cbData: (payload.len() * 2) as u32, lpData: payload.as_mut_ptr() as _ };
        let mut result = 0usize;
        let sent = SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            5000,
            Some(&mut result),
        );
        if sent.0 == 0 {
            return false;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
        true
    }
}

pub fn run(initial: Vec<PathBuf>) {
    if forward_to_running(&initial) {
        return;
    }
    clean_stale_sessions();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = OleInitialize(None);
        crate::dict::preload();

        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("ScrubDaddyWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hIcon: LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_APPWINDOW,
            class,
            w!("Scrub Daddy"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CLIENT_W,
            CLIENT_H,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let sources = Sources {
            folder: imaging::decode_png(FOLDER_PNG).expect("folder art"),
            done: imaging::decode_png(FOLDER_DONE_PNG).expect("done art"),
            sponge: imaging::decode_png(SPONGE_PNG).expect("sponge art"),
        };
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                jobs: 0,
                dpi,
                state: State::Idle(None),
                hover: false,
                tick: 0,
                res: None,
                sources,
            })
        });
        with_app(|a| a.rebuild());
        resize_for_dpi(hwnd, dpi, None);
        let target: IDropTarget = DropTarget { hwnd }.into();
        let _ = RegisterDragDrop(hwnd, &target);
        let _ = ShowWindow(hwnd, SW_SHOW);

        if !initial.is_empty() {
            start(hwnd, initial);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
        OleUninitialize();
        let _ = std::fs::remove_dir_all(session_dir());
    }
}

unsafe fn resize_for_dpi(hwnd: HWND, dpi: u32, suggested: Option<&RECT>) {
    let mut r = RECT { left: 0, top: 0, right: s(CLIENT_W, dpi), bottom: s(CLIENT_H, dpi) };
    let _ = AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, false, WS_EX_APPWINDOW, dpi);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    match suggested {
        Some(sr) => {
            let _ = SetWindowPos(hwnd, None, sr.left, sr.top, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        None => {
            let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOZORDER | SWP_NOMOVE | SWP_NOACTIVATE);
        }
    }
}

impl App {
    fn rebuild(&mut self) {
        if let Some(r) = self.res.take() {
            unsafe {
                for b in [r.folder.hbm, r.done.hbm, r.sponge.hbm] {
                    let _ = DeleteObject(b.into());
                }
                for f in [r.big, r.small, r.label] {
                    let _ = DeleteObject(f.into());
                }
            }
        }
        let d = self.dpi;
        let fs = s(FOLDER, d);
        self.res = Some(Res {
            folder: make_bitmap(&self.sources.folder, fs, fs),
            done: make_bitmap(&self.sources.done, fs, fs),
            sponge: make_bitmap(&self.sources.sponge, s(64, d), s(64, d)),
            big: font(s(17, d), 600),
            small: font(s(13, d), 400),
            label: font(s(12, d), 600),
        });
    }

    fn zone(&self) -> RECT {
        let d = self.dpi;
        RECT { left: s(ZONE.0, d), top: s(ZONE.1, d), right: s(ZONE.2, d), bottom: s(ZONE.3, d) }
    }
}

fn font(px: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

/// Scale an RGBA image (area-averaged) into a premultiplied 32-bit DIB for AlphaBlend.
fn make_bitmap(img: &Image, w: i32, h: i32) -> Bmp {
    let (sw, sh) = (img.w as f32, img.h as f32);
    let (fx, fy) = (sw / w as f32, sh / h as f32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let y0 = y as f32 * fy;
        let y1 = (y0 + fy).min(sh);
        for x in 0..w {
            let x0 = x as f32 * fx;
            let x1 = (x0 + fx).min(sw);
            let mut acc = [0f32; 4];
            let mut wsum = 0f32;
            let mut yy = y0.floor() as usize;
            while (yy as f32) < y1 {
                let wy = ((yy + 1) as f32).min(y1) - (yy as f32).max(y0);
                let mut xx = x0.floor() as usize;
                while (xx as f32) < x1 {
                    let wx = ((xx + 1) as f32).min(x1) - (xx as f32).max(x0);
                    let wgt = wx * wy;
                    let p = &img.px[(yy * img.w + xx) * 4..][..4];
                    let a = p[3] as f32 / 255.0;
                    acc[0] += p[2] as f32 * a * wgt;
                    acc[1] += p[1] as f32 * a * wgt;
                    acc[2] += p[0] as f32 * a * wgt;
                    acc[3] += p[3] as f32 * wgt;
                    wsum += wgt;
                    xx += 1;
                }
                yy += 1;
            }
            let o = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                px[o + c] = (acc[c] / wsum.max(1e-6)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbm = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        }
        Bmp { hbm, w, h }
    }
}

unsafe fn blit(hdc: HDC, b: &Bmp, x: i32, y: i32) {
    let mem = CreateCompatibleDC(Some(hdc));
    let old = SelectObject(mem, b.hbm.into());
    let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    let _ = AlphaBlend(hdc, x, y, b.w, b.h, mem, 0, 0, b.w, b.h, bf);
    SelectObject(mem, old);
    let _ = DeleteDC(mem);
}

unsafe fn fill_round(hdc: HDC, r: RECT, radius: i32, fill: u32, border: Option<(u32, i32)>) {
    let brush = CreateSolidBrush(COLORREF(fill));
    let pen = match border {
        Some((c, w)) => CreatePen(PS_SOLID, w, COLORREF(c)),
        None => CreatePen(PS_NULL, 0, COLORREF(0)),
    };
    let ob = SelectObject(hdc, brush.into());
    let op = SelectObject(hdc, pen.into());
    let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(pen.into());
}

unsafe fn text(hdc: HDC, s: &str, mut r: RECT, f: HFONT, color: u32, flags: DRAW_TEXT_FORMAT) {
    let of = SelectObject(hdc, f.into());
    SetTextColor(hdc, COLORREF(color));
    let mut w: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(hdc, &mut w, &mut r, flags | DT_NOPREFIX);
    SelectObject(hdc, of);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
    let old = SelectObject(mem, bmp.into());
    with_app(|a| draw(a, mem, rc));
    let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn draw(a: &App, hdc: HDC, rc: RECT) {
    let Some(res) = &a.res else { return };
    let d = a.dpi;
    let bg = CreateSolidBrush(COLORREF(BG));
    FillRect(hdc, &rc, bg);
    let _ = DeleteObject(bg.into());
    SetBkMode(hdc, TRANSPARENT);

    let zone = a.zone();
    let (zfill, zborder) = if a.hover { (ZONE_HOVER, (ACCENT, s(3, d))) } else { (ZONE_BG, (BORDER, s(2, d))) };
    fill_round(hdc, zone, s(22, d), zfill, Some(zborder));

    let fs = s(FOLDER, d);
    let fx = (zone.left + zone.right - fs) / 2;
    let fy = zone.top + s(18, d);
    let done = matches!(a.state, State::Done { .. });
    // A gentle bob on the finished folder invites dragging it.
    let bob = if done { ((a.tick as f32 * 0.12).sin() * s(3, d) as f32) as i32 } else { 0 };
    blit(hdc, if done { &res.done } else { &res.folder }, fx, fy + bob);
    // How many files are inside, when there is more than one.
    if let State::Done { paths, .. } = &a.state {
        if paths.len() > 1 {
            let r = s(17, d);
            let (cx, cy) = (fx + s(34, d), fy + s(44, d) + bob);
            let pill = RECT { left: cx - r, top: cy - r, right: cx + r, bottom: cy + r };
            fill_round(hdc, pill, 2 * r, ACCENT, Some((rgb(255, 255, 255), s(3, d))));
            text(hdc, &paths.len().to_string(), pill, res.big, TEXT, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
        }
    }

    if let State::Working { .. } = a.state {
        // The sponge scrubs back and forth across the folder.
        let t = a.tick as f32 * 0.16;
        let sx = fx + fs / 2 - res.sponge.w / 2 + (t.sin() * s(46, d) as f32) as i32;
        let sy = fy + fs / 2 - res.sponge.h / 2 + s(14, d) + ((t * 2.0).cos() * s(8, d) as f32) as i32;
        blit(hdc, &res.sponge, sx, sy);
    }

    let caption = RECT { left: zone.left, top: zone.bottom - s(44, d), right: zone.right, bottom: zone.bottom - s(14, d) };
    let (title, sub, color) = match &a.state {
        State::Idle(note) => (
            if a.hover { "Let go to scrub it!" } else { "Drop PDF or Word files" }.to_string(),
            note.clone().unwrap_or_else(|| "Names and claim numbers get scrubbed".into()),
            TEXT,
        ),
        State::Working { msg, .. } => ("Scrubbing…".to_string(), msg.clone(), TEXT),
        State::Done { summary, saved, .. } => (
            if *saved { "Saved! Drag again or drop another" } else { "Squeaky clean! Drag me out" }.to_string(),
            summary.clone(),
            GOOD,
        ),
        State::Error(e) => ("Couldn't scrub that one".to_string(), e.clone(), ERROR),
    };
    text(hdc, &title, caption, res.big, color, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);

    // Progress bar.
    let bar = RECT { left: s(26, d), top: s(270, d), right: rc.right - s(26, d), bottom: s(282, d) };
    let progress = match &a.state {
        State::Working { progress, .. } => Some(*progress),
        State::Done { .. } => Some(1.0),
        _ => None,
    };
    if let Some(p) = progress {
        fill_round(hdc, bar, s(12, d), TRACK, None);
        let fillw = ((bar.right - bar.left) as f32 * p.clamp(0.0, 1.0)) as i32;
        if fillw > s(6, d) {
            let fr = RECT { right: bar.left + fillw, ..bar };
            fill_round(hdc, fr, s(12, d), if done { GOOD } else { ACCENT }, None);
        }
        let pct = RECT { left: bar.left, top: bar.bottom + s(2, d), right: bar.right, bottom: bar.bottom + s(18, d) };
        text(hdc, &format!("{}%", (p * 100.0).round() as u32), pct, res.label, MUTED, DT_RIGHT | DT_SINGLELINE | DT_TOP);
    }

    let subr = RECT { left: s(18, d), top: s(298, d), right: rc.right - s(18, d), bottom: rc.bottom - s(6, d) };
    text(hdc, &sub, subr, res.small, if matches!(a.state, State::Error(_)) { ERROR } else { MUTED }, DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS);
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Expand dropped folders into the PDFs/Word files inside them (skipping earlier scrubbed
/// output and Office lock files). Returns the files to scrub and how many were skipped.
fn expand(paths: Vec<PathBuf>) -> (Vec<PathBuf>, usize) {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>, skipped: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if depth < 4 {
                    walk(&p, depth + 1, out, skipped);
                }
            } else {
                let name = file_name(&p);
                let scrubbed = name.contains("-scrubbed");
                if pipeline::supported(&p) && !scrubbed && !name.starts_with("~$") {
                    out.push(p);
                } else if !scrubbed && !name.starts_with("~$") {
                    *skipped += 1;
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    for p in paths {
        if p.is_dir() {
            walk(&p, 0, &mut out, &mut skipped);
        } else if pipeline::supported(&p) {
            out.push(p);
        } else {
            skipped += 1;
        }
    }
    out.dedup();
    (out, skipped)
}

/// What the worker thread reports when a batch finishes.
struct Batch {
    results: Vec<(String, std::result::Result<Outcome, String>)>,
    skipped: usize,
    dir: PathBuf,
}

fn start(hwnd: HWND, dropped: Vec<PathBuf>) {
    let busy = with_app(|a| matches!(a.state, State::Working { .. })).unwrap_or(true);
    if busy || dropped.is_empty() {
        return;
    }
    let (files, skipped) = expand(dropped);
    if files.is_empty() {
        set_state(State::Error(if skipped == 1 {
            "That isn't a PDF or Word (.docx) file.".into()
        } else {
            "No PDF or Word (.docx) files in what was dropped.".into()
        }));
        return;
    }
    let name = if files.len() == 1 { file_name(&files[0]) } else { format!("{} files", files.len()) };
    // Results that were never dragged out are thrown away when new files are dropped.
    let job = with_app(|a| {
        a.jobs += 1;
        a.jobs
    })
    .unwrap_or(1);
    let _ = std::fs::remove_dir_all(session_dir());
    let out_dir = session_dir().join(job.to_string());
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        set_state(State::Error(format!("Couldn't create a temp folder: {e}")));
        return;
    }
    set_state(State::Working { progress: 0.0, msg: format!("Opening {name}…") });
    unsafe {
        let _ = SetTimer(Some(hwnd), TIMER_ANIM, 33, None);
    }
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let n = files.len();
        let mut results = Vec::with_capacity(n);
        for (i, path) in files.iter().enumerate() {
            // Overall progress across the batch, with the current file's step underneath.
            let report = move |p: f32, msg: &str| unsafe {
                let overall = (i as f32 + p) / n as f32;
                let text = if n > 1 { format!("File {} of {n}: {msg}", i + 1) } else { msg.to_string() };
                let b = Box::into_raw(Box::new(text));
                if PostMessageW(Some(HWND(h as *mut _)), WM_APP_PROGRESS, WPARAM((overall * 1000.0) as usize), LPARAM(b as isize)).is_err() {
                    drop(Box::from_raw(b));
                }
            };
            let result = std::panic::catch_unwind(|| pipeline::run(path, Some(&out_dir), &report))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Something went wrong while scrubbing.")))
                .map_err(|e| format!("{e:#}"));
            results.push((file_name(path), result));
        }
        let b = Box::new(Batch { results, skipped, dir: out_dir });
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_DONE, WPARAM(0), LPARAM(Box::into_raw(b) as isize));
        }
    });
}

fn set_state(st: State) {
    let hwnd = with_app(|a| {
        a.state = st;
        a.hwnd
    });
    if let Some(h) = hwnd {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false);
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn summary(ok: &[&Outcome], failed: &[(String, String)], skipped: usize) -> String {
    let names: usize = ok.iter().map(|o| o.names).sum();
    let cases: usize = ok.iter().map(|o| o.case_numbers).sum();
    let mut found = plural(names, "name", "names");
    if ok.len() == 1 {
        found += &format!(" ({})", plural(ok[0].people, "person", "people"));
    }
    if cases > 0 {
        found += &format!(", {cases} case #");
    }
    let mut lines = vec![if ok.len() == 1 {
        format!("Scrubbed {found} → {}", file_name(&ok[0].output))
    } else {
        format!("Scrubbed {}: {found}", plural(ok.len(), "file", "files"))
    }];
    match failed {
        [] => {}
        [(name, why)] => lines.push(format!("Couldn't scrub {name}: {why}")),
        many => lines.push(format!("Couldn't scrub {}: {}", plural(many.len(), "file", "files"), many.iter().map(|f| f.0.as_str()).collect::<Vec<_>>().join(", "))),
    }
    if skipped > 0 {
        lines.push(format!("Skipped {} (not PDF or Word)", plural(skipped, "file", "files")));
    }
    lines.push(if ok.len() == 1 {
        "Drag the folder to save it · double-click to preview".into()
    } else {
        "Drag out to save them all · double-click to view".into()
    });
    lines.join("\n")
}

/// One shell data object for all the files, so a single drag carries them together.
unsafe fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    let mut pidls: Vec<*const windows::Win32::UI::Shell::Common::ITEMIDLIST> = Vec::new();
    for p in paths {
        let mut pidl = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(p.as_os_str()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
            pidls.push(pidl);
        }
    }
    let data = if pidls.is_empty() {
        None
    } else {
        SHCreateShellItemArrayFromIDLists(&pidls)
            .and_then(|arr| arr.BindToHandler::<_, IDataObject>(None, &BHID_DataObject))
            .ok()
    };
    for p in pidls {
        ILFree(Some(p));
    }
    data
}

unsafe fn drag_out(hwnd: HWND, paths: &[PathBuf]) {
    let live: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
    let Some(data) = data_object(&live) else { return };
    let effect = SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_MOVE).unwrap_or(DROPEFFECT_NONE);
    if live.iter().all(|p| !p.exists()) {
        let msg = if live.len() == 1 { "Saved! Drop another file to scrub it.".to_string() } else { format!("Saved {} files! Drop more to scrub them.", live.len()) };
        set_state(State::Idle(Some(msg)));
    } else if effect != DROPEFFECT_NONE {
        with_app(|a| {
            if let State::Done { saved, .. } = &mut a.state {
                *saved = true;
            }
        });
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn in_zone(lparam: LPARAM) -> bool {
    let x = (lparam.0 & 0xffff) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
    with_app(|a| {
        let z = a.zone();
        x >= z.left && x < z.right && y >= z.top && y < z.bottom
    })
    .unwrap_or(false)
}

fn done_paths() -> Option<Vec<PathBuf>> {
    with_app(|a| match &a.state {
        State::Done { paths, .. } => Some(paths.clone()),
        _ => None,
    })
    .flatten()
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                let animate = with_app(|a| {
                    a.tick = a.tick.wrapping_add(1);
                    matches!(a.state, State::Working { .. } | State::Done { .. })
                })
                .unwrap_or(false);
                if animate {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                } else {
                    let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                }
                LRESULT(0)
            }
            WM_APP_PROGRESS => {
                let msg = *Box::from_raw(lparam.0 as *mut String);
                with_app(|a| {
                    if let State::Working { progress, msg: m } = &mut a.state {
                        *progress = (wparam.0 as f32 / 1000.0).max(*progress);
                        *m = msg;
                    }
                });
                LRESULT(0)
            }
            WM_APP_DONE => {
                let batch = *Box::from_raw(lparam.0 as *mut Batch);
                let ok: Vec<&Outcome> = batch.results.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
                let failed: Vec<(String, String)> =
                    batch.results.iter().filter_map(|(n, r)| r.as_ref().err().map(|e| (n.clone(), e.clone()))).collect();
                if ok.is_empty() {
                    let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                    let msg = match failed.as_slice() {
                        [(_, why)] => why.clone(),
                        many => format!("None of the {} files could be scrubbed. First error: {}", many.len(), many[0].1),
                    };
                    set_state(State::Error(msg));
                } else {
                    let summary = summary(&ok, &failed, batch.skipped);
                    let paths = ok.iter().map(|o| o.output.clone()).collect();
                    set_state(State::Done { paths, dir: batch.dir, summary, saved: false });
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(paths) = done_paths() {
                    if in_zone(lparam) {
                        let pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                        let mut spt = pt;
                        let _ = ClientToScreen(hwnd, &mut spt);
                        if DragDetect(hwnd, spt).as_bool() {
                            drag_out(hwnd, &paths);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                let target = with_app(|a| match &a.state {
                    // One file: preview it. Several: show them in Explorer.
                    State::Done { paths, dir, .. } => Some(if paths.len() == 1 { paths[0].clone() } else { dir.clone() }),
                    _ => None,
                })
                .flatten();
                if let Some(t) = target.filter(|_| in_zone(lparam)) {
                    let t = HSTRING::from(t.as_os_str());
                    ShellExecuteW(Some(hwnd), w!("open"), &t, None, None, SW_SHOWNORMAL);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let reset = with_app(|a| matches!(a.state, State::Done { .. } | State::Error(_))).unwrap_or(false);
                if reset {
                    set_state(State::Idle(None));
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if (lparam.0 & 0xffff) as u32 == HTCLIENT {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = ScreenToClient(hwnd, &mut pt);
                    let over = done_paths().is_some() && in_zone(LPARAM(((pt.y as isize) << 16) | (pt.x as isize & 0xffff)));
                    SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).ok());
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xffff) as u32;
                with_app(|a| {
                    a.dpi = dpi;
                    a.rebuild();
                });
                resize_for_dpi(hwnd, dpi, Some(&*(lparam.0 as *const RECT)));
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_COPYDATA => {
                // Another launch of the exe handed us a file (or just asked us to come forward).
                let cds = &*(lparam.0 as *const COPYDATASTRUCT);
                if cds.dwData != COPYDATA_OPEN {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                // NUL-separated paths.
                let wide = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
                let paths: Vec<PathBuf> =
                    wide.split(|&c| c == 0).filter(|s| !s.is_empty()).map(|s| PathBuf::from(OsString::from_wide(s))).collect();
                start(hwnd, paths);
                LRESULT(1)
            }
            WM_CLOSE => {
                let unsaved = with_app(|a| matches!(&a.state, State::Done { paths, saved: false, .. } if paths.iter().any(|p| p.exists())))
                    .unwrap_or(false);
                let busy = with_app(|a| matches!(a.state, State::Working { .. })).unwrap_or(false);
                let ask = if unsaved {
                    Some(w!("You haven't dragged the scrubbed files anywhere yet. Close and discard them?"))
                } else if busy {
                    Some(w!("Still scrubbing. Close and discard it?"))
                } else {
                    None
                };
                if let Some(q) = ask {
                    if MessageBoxW(Some(hwnd), q, w!("Scrub Daddy"), MB_YESNO | MB_ICONQUESTION) != IDYES {
                        return LRESULT(0);
                    }
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Files dropped onto the window.
#[implement(IDropTarget)]
struct DropTarget {
    hwnd: HWND,
}

fn dropped_files(data: &IDataObject) -> Vec<PathBuf> {
    unsafe {
        let fmt = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let Ok(mut medium) = data.GetData(&fmt) else { return Vec::new() };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        ReleaseStgMedium(&mut medium);
        out
    }
}

/// Dropped files, minus our own scrubbed output being dragged back over the window.
fn incoming(data: &IDataObject) -> Vec<PathBuf> {
    let own = done_paths().unwrap_or_default();
    dropped_files(data).into_iter().filter(|f| !own.contains(f)).collect()
}

fn acceptable(data: &IDataObject) -> bool {
    let working = with_app(|a| matches!(a.state, State::Working { .. })).unwrap_or(true);
    !working && !incoming(data).is_empty()
}

fn set_hover(hwnd: HWND, on: bool) {
    let changed = with_app(|a| std::mem::replace(&mut a.hover, on) != on).unwrap_or(false);
    if changed {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let ok = data.as_ref().is_some_and(acceptable);
        unsafe { *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        set_hover(self.hwnd, ok);
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let hover = with_app(|a| a.hover).unwrap_or(false);
        unsafe { *effect = if hover { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        set_hover(self.hwnd, false);
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        set_hover(self.hwnd, false);
        let files = data.as_ref().map(incoming).unwrap_or_default();
        unsafe { *effect = if files.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY } };
        start(self.hwnd, files);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drag-out data object must carry every scrubbed file as one CF_HDROP list.
    #[test]
    fn data_object_carries_all_files() {
        let dir = std::env::temp_dir().join(format!("scrubdaddy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<PathBuf> = (1..=3).map(|i| dir.join(format!("file{i}-scrubbed.pdf"))).collect();
        for p in &paths {
            std::fs::write(p, b"%PDF-1.7").unwrap();
        }
        unsafe {
            let _ = OleInitialize(None);
            let data = data_object(&paths).expect("data object");
            let mut got = dropped_files(&data);
            got.sort();
            assert_eq!(got, paths);
            OleUninitialize();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_expand_to_documents() {
        let dir = std::env::temp_dir().join(format!("scrubdaddy-expand-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for f in ["a.pdf", "b.docx", "c.txt", "a-scrubbed.pdf", "~$b.docx"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(sub.join("d.PDF"), b"x").unwrap();
        let (files, skipped) = expand(vec![dir.clone()]);
        let names: Vec<String> = files.iter().map(|p| file_name(p)).collect();
        assert_eq!(names, ["a.pdf", "b.docx", "d.PDF"]);
        assert_eq!(skipped, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
