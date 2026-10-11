//! The little drop window: drop a file on the folder, watch the sponge scrub, then drag the
//! finished folder out to wherever the scrubbed file should go.

use crate::imaging::{self, Image};
use crate::pipeline::{self, Outcome};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::core::{implement, w, Ref, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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
const CLIENT_H: i32 = 350;
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
    Done { path: PathBuf, summary: String },
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

pub fn run(initial: Option<PathBuf>) {
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

        if let Some(p) = initial {
            start(hwnd, p);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
        OleUninitialize();
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
            if a.hover { "Let go to scrub it!" } else { "Drop a PDF or Word file" }.to_string(),
            note.clone().unwrap_or_else(|| "Names and claim numbers get scrubbed".into()),
            TEXT,
        ),
        State::Working { msg, .. } => ("Scrubbing…".to_string(), msg.clone(), TEXT),
        State::Done { summary, .. } => ("Squeaky clean! Drag me out".to_string(), summary.clone(), GOOD),
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

fn start(hwnd: HWND, path: PathBuf) {
    let busy = with_app(|a| matches!(a.state, State::Working { .. })).unwrap_or(true);
    if busy {
        return;
    }
    if !pipeline::supported(&path) {
        set_state(State::Error(format!(
            "{} isn't a PDF or .docx file.",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        )));
        return;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    set_state(State::Working { progress: 0.0, msg: format!("Opening {name}…") });
    unsafe {
        let _ = SetTimer(Some(hwnd), TIMER_ANIM, 33, None);
    }
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let report = move |p: f32, msg: &str| unsafe {
            let b = Box::into_raw(Box::new(msg.to_string()));
            if PostMessageW(Some(HWND(h as *mut _)), WM_APP_PROGRESS, WPARAM((p * 1000.0) as usize), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
        };
        let hwnd = HWND(h as *mut _);
        let result = std::panic::catch_unwind(|| pipeline::run(&path, &report))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Something went wrong while scrubbing.")))
            .map_err(|e| format!("{e:#}"));
        let b: Box<std::result::Result<Outcome, String>> = Box::new(result);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_DONE, WPARAM(0), LPARAM(Box::into_raw(b) as isize));
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

fn summary(o: &Outcome) -> String {
    let file = o.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut parts = vec![format!("{} name{} ({} {})", o.names, if o.names == 1 { "" } else { "s" }, o.people, if o.people == 1 { "person" } else { "people" })];
    if o.case_numbers > 0 {
        parts.push(format!("{} case #", o.case_numbers));
    }
    format!("Scrubbed {} → {file}", parts.join(", "))
}

unsafe fn drag_out(hwnd: HWND, path: &Path) {
    let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path.as_os_str()), None) else { return };
    let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else { return };
    let _ = SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_MOVE);
    if !path.exists() {
        set_state(State::Idle(Some("Moved! Drop another file to scrub it.".into())));
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

fn done_path() -> Option<PathBuf> {
    with_app(|a| match &a.state {
        State::Done { path, .. } => Some(path.clone()),
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
                let res = *Box::from_raw(lparam.0 as *mut std::result::Result<Outcome, String>);
                match res {
                    Ok(o) => {
                        let summary = summary(&o);
                        set_state(State::Done { path: o.output, summary });
                    }
                    Err(e) => {
                        let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                        set_state(State::Error(e));
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(path) = done_path() {
                    if in_zone(lparam) {
                        let pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                        let mut spt = pt;
                        let _ = ClientToScreen(hwnd, &mut spt);
                        if DragDetect(hwnd, spt).as_bool() {
                            drag_out(hwnd, &path);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                if let Some(path) = done_path() {
                    if in_zone(lparam) {
                        // Show the file in Explorer.
                        let args = HSTRING::from(format!("/select,\"{}\"", path.display()));
                        ShellExecuteW(Some(hwnd), w!("open"), w!("explorer.exe"), &args, None, SW_SHOWNORMAL);
                    }
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
                    let over = done_path().is_some() && in_zone(LPARAM(((pt.y as isize) << 16) | (pt.x as isize & 0xffff)));
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

fn acceptable(data: &IDataObject) -> bool {
    let files = dropped_files(data);
    let own = done_path();
    let working = with_app(|a| matches!(a.state, State::Working { .. })).unwrap_or(true);
    !working && files.iter().any(|f| Some(f) != own.as_ref())
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
        let own = done_path();
        let file = data
            .as_ref()
            .map(dropped_files)
            .unwrap_or_default()
            .into_iter()
            .find(|f| Some(f) != own.as_ref());
        unsafe { *effect = if file.is_some() { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        if let Some(f) = file {
            start(self.hwnd, f);
        }
        Ok(())
    }
}
