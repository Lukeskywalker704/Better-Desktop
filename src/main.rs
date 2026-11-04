#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path as FsPath, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
    },
};
use winapi::{
    shared::minwindef::{BOOL, LPARAM, LRESULT, WPARAM},
    shared::windef::{HBITMAP, HDC, HGDIOBJ, HICON, HMONITOR, HWND, POINT, RECT, SIZE},
    um::{
        errhandlingapi::GetLastError,
        libloaderapi::GetModuleHandleW,
        synchapi::CreateMutexW,
        shellapi::{DragQueryFileW, Shell_NotifyIconW, HDROP, NOTIFYICONDATAW, NIM_ADD, NIM_DELETE},
        wingdi::{
            BLENDFUNCTION, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, GetTextExtentPoint32W,
            DeleteObject, SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, DIB_RGB_COLORS, SRCCOPY,
        },
        winuser::{
            AppendMenuW, CallWindowProcW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
            CloseClipboard, DestroyMenu, DrawTextW, EnumDisplayMonitors, FindWindowW, GetClassNameW,
            GetClipboardData, GetCursorPos, GetForegroundWindow, GetMonitorInfoW, keybd_event,
            MessageBoxW, OpenClipboard, RegisterHotKey,
            GetDC, GetSystemMetrics, GetWindowLongPtrW, LoadIconW, LoadImageW, MonitorFromPoint,
            MONITORINFO, PostMessageW,
            PrintWindow, RegisterClassW,
            ReleaseDC, ReleaseCapture, SetCapture, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, TrackPopupMenu,
            UpdateLayeredWindow, GWL_EXSTYLE, GWLP_WNDPROC, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON,
            LR_DEFAULTSIZE, LR_SHARED, MAKEINTRESOURCEW,
            SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
            CF_HDROP, HWND_BOTTOM, HWND_TOPMOST, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, VK_CONTROL,
            TPM_RETURNCMD, WM_CLOSE, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
            WS_EX_NOACTIVATE,
        },
    },
};
use winit::{
    event::{ElementState, Event, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::EventLoop,
    raw_window_handle::RawWindowHandle,
    window::{CursorIcon, Window, WindowBuilder},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Design width of a card, in 96-dpi pixels. Card interiors are laid out
/// proportionally to this, so a card keeps its look when zoomed or when the
/// display runs at a scaling factor.
const CARD_BASE_W: f32 = 380.0;
/// height / width kept when a card is zoomed with the wheel
const CARD_ASPECT: f32 = 1.45;

static UI_SCALE_MILLI: AtomicU32 = AtomicU32::new(1000);

/// Window-DPI / 96, i.e. how many physical pixels one logical pixel is.
fn ui_scale() -> f32 {
    UI_SCALE_MILLI.load(Ordering::Relaxed) as f32 / 1000.0
}


/// A plain message box, used by `--version` / `--help` and the tray menu.
fn info_box(text: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("better desktop").as_ptr(),
            0x0000_0040, // MB_ICONINFORMATION
        );
    }
}

fn config_dir() -> PathBuf {
    // %APPDATA%\BetterDesktop: keeps the card layout out of the build tree, so
    // `cargo clean` (or moving the exe) can never take the user's cards with it
    let dir = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("BetterDesktop");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// One-shot migration: the old build-tree config moves to %APPDATA%.
fn migrate_legacy_config() {
    let new = config_dir().join("config.json");
    if new.exists() { return; }
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".better-desktop")
        .join("config.json");
    if legacy.exists() {
        let _ = fs::copy(&legacy, &new);
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Card {
    x: f32, y: f32, width: f32, height: f32,
    path: String, color: [u8; 3],
    /// may the card reshape itself to the aspect of its own preview? Cleared
    /// once the user resizes it by hand.
    #[serde(default = "default_true")]
    auto_fit: bool,
    /// folded down to the caption strip; `height` keeps the expanded size
    #[serde(default)]
    collapsed: bool,
    /// DPI this card's pixel size was chosen for (0 = adopt on first layout)
    #[serde(default)]
    dpi: f32,
}

fn default_true() -> bool { true }

/// height of the caption strip a collapsed card folds down to
const COLLAPSED_H: f32 = 52.0;

impl Card {
    fn new(path: String, x: f32, y: f32) -> Self {
        let color = file_color(&path);
        let base = CARD_BASE_W * ui_scale();
        Self {
            x, y, width: base, height: base * CARD_ASPECT, path, color,
            auto_fit: true, collapsed: false, dpi: 0.0,
        }
    }
    /// Same card, sized for a specific monitor scale instead of the system one.
    fn with_scale(path: String, x: f32, y: f32, scale: f32) -> Self {
        let mut card = Self::new(path, x, y);
        card.width = CARD_BASE_W * scale;
        card.height = card.width * CARD_ASPECT;
        card
    }
    fn file_name(&self) -> String {
        FsPath::new(&self.path).file_name().unwrap_or_default().to_string_lossy().to_string()
    }
    fn file_ext(&self) -> String {
        FsPath::new(&self.path).extension().unwrap_or_default().to_string_lossy().to_uppercase()
    }
    /// interior scale, derived from the width
    fn scale(&self) -> f32 {
        (self.width / CARD_BASE_W).clamp(0.45, 2.4)
    }
    /// height the card actually occupies right now (a folded card is just a strip)
    fn box_h(&self) -> f32 {
        if self.collapsed { COLLAPSED_H * self.scale() } else { self.height }
    }
    /// width a folded card shrinks to: badge + caption + chevron + padding
    fn label_width(&self) -> f32 {
        let s = self.scale();
        let pad = CARD_PAD * s;
        let size = (14.0 * s).round() as i32;
        let text = measure_text(&self.file_name(), size, 600);
        let w = pad + 30.0 * s + 10.0 * s + text + 10.0 * s + 30.0 * s + pad;
        w.clamp(150.0 * s, self.width)
    }
    /// width the card actually occupies right now
    fn box_w(&self) -> f32 {
        if self.collapsed { self.label_width() } else { self.width }
    }
    /// x the card is actually drawn at: a folded pill hugs the column's right
    /// edge instead of floating at the card's left edge
    fn visual_x(&self) -> f32 {
        if self.collapsed { self.x + self.width - self.box_w() } else { self.x }
    }
}

/// Reshape a card for a landscape preview (wide photo, slide, landscape page):
/// the paper is fitted to the image aspect and the card shrinks to match, so a
/// 16:9 shot no longer sits in a postcard-shaped card with empty tint.
/// Returns true when the height actually changed.
fn fit_card_to_preview(card: &mut Card, thumb: Option<&Thumbnail>) -> bool {
    let Some(t) = thumb else { return false };
    if t.width == 0 || t.height == 0 { return false; }
    let aspect = t.width as f32 / t.height as f32;
    if aspect < 1.15 { return false; } // portrait or square: keep the default
    let s = card.width / CARD_BASE_W;
    let page_w = card.width - 30.0 * s;
    let page_h = (page_w / aspect).max(70.0 * s);
    let height = (46.0 + 15.0) * s + page_h;
    if (card.height - height).abs() > 2.0 {
        card.height = height;
        true
    } else {
        false
    }
}

/// Per-type palette: a saturated accent for the icon badge, plus the letter
/// drawn inside it.
struct TypeStyle {
    accent: (u8, u8, u8),
    glyph: &'static str,
}

fn type_style(ext: &str) -> TypeStyle {
    match ext {
        "PDF" => TypeStyle { accent: (216, 63, 58), glyph: "P" },
        "DOCX" | "DOC" => TypeStyle { accent: (36, 98, 234), glyph: "W" },
        "XLSX" | "XLS" => TypeStyle { accent: (21, 156, 98), glyph: "X" },
        "PPTX" | "PPT" => TypeStyle { accent: (230, 94, 35), glyph: "P" },
        "TXT" => TypeStyle { accent: (108, 121, 140), glyph: "T" },
        "MD" => TypeStyle { accent: (92, 107, 130), glyph: "M" },
        "PNG" | "JPG" | "JPEG" | "GIF" | "BMP" | "WEBP" => {
            TypeStyle { accent: (110, 95, 222), glyph: "I" }
        }
        _ => TypeStyle { accent: (148, 160, 176), glyph: "F" },
    }
}

fn file_color(path: &str) -> [u8; 3] {
    let ext = FsPath::new(path).extension().unwrap_or_default().to_string_lossy().to_uppercase();
    let (r, g, b) = type_style(&ext).accent;
    [r, g, b]
}

#[derive(Serialize, Deserialize)]
struct AppConfig {
    cards: Vec<Card>,
    /// pin the overlay above every other window instead of keeping it on the
    /// desktop layer (default: visible, so cards are never hidden behind apps)
    #[serde(default = "default_true")]
    topmost: bool,
    /// the cards currently form a scrollable right-hand column (set by tidy when
    /// the stack does not fit; any manual drag ends it)
    #[serde(default)]
    rail: bool,
}

impl AppConfig {
    fn path() -> PathBuf { config_dir().join("config.json") }
    fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), json);
        }
    }
    fn load() -> Self {
        fs::read_to_string(Self::path()).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(Self::default)
    }
    fn default() -> Self {
        Self {
            topmost: true,
            rail: false,
            cards: vec![
                Card::new("C:/example/report.pdf".into(), 80.0, 60.0),
                Card::new("C:/example/notes.docx".into(), 330.0, 100.0),
                Card::new("C:/example/data.xlsx".into(), 580.0, 60.0),
                Card::new("C:/example/slides.pptx".into(), 180.0, 380.0),
                Card::new("C:/example/readme.md".into(), 430.0, 380.0),
            ],
        }
    }
}

// Pixels are stored PREMULTIPLIED BGRA (required by UpdateLayeredWindow;
// alpha=0 regions are automatically click-through and skip DWM work).
struct Framebuffer { width: u32, height: u32, pixels: Vec<u8> }

impl Framebuffer {
    fn new(w: u32, h: u32) -> Self {
        Self { width: w, height: h, pixels: vec![0; (w * h * 4) as usize] }
    }
    fn clear(&mut self) { self.pixels.fill(0); }
    /// Blend a straight-alpha color onto the premultiplied buffer.
    fn set_pixel(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
        if x >= 0 && x < self.width as i32 && y >= 0 && y < self.height as i32 {
            let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
            let sa = a as f32 / 255.0;
            let inv = 1.0 - sa;
            self.pixels[idx]     = (b as f32 * sa + self.pixels[idx]     as f32 * inv) as u8;
            self.pixels[idx + 1] = (g as f32 * sa + self.pixels[idx + 1] as f32 * inv) as u8;
            self.pixels[idx + 2] = (r as f32 * sa + self.pixels[idx + 2] as f32 * inv) as u8;
            self.pixels[idx + 3] = (a as f32 + self.pixels[idx + 3] as f32 * inv) as u8;
        }
    }
    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: u8, g: u8, b: u8, a: u8) {
        for dy in 0..h { for dx in 0..w { self.set_pixel(x + dx, y + dy, r, g, b, a); } }
    }
    /// Anti-aliased filled rounded rectangle (signed-distance coverage).
    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rad: f32, c: (u8, u8, u8), a: u8) {
        if w <= 0.0 || h <= 0.0 || a == 0 { return; }
        let rad = rad.max(0.0).min(w * 0.5).min(h * 0.5);
        let hw = w * 0.5;
        let hh = h * 0.5;
        let cx = x + hw;
        let cy = y + hh;
        let x0 = (x.floor() - 1.0) as i32;
        let y0 = (y.floor() - 1.0) as i32;
        let x1 = ((x + w).ceil() + 1.0) as i32;
        let y1 = ((y + h).ceil() + 1.0) as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let qx = ((px as f32 + 0.5) - cx).abs() - (hw - rad);
                let qy = ((py as f32 + 0.5) - cy).abs() - (hh - rad);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                    + qx.max(qy).min(0.0) - rad;
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let aa = (a as f32 * cov + 0.5) as u8;
                    if aa > 0 { self.set_pixel(px, py, c.0, c.1, c.2, aa); }
                }
            }
        }
    }
    /// Anti-aliased rounded-rect hairline centred on the rect edge.
    fn round_rect_stroke(&mut self, x: f32, y: f32, w: f32, h: f32, rad: f32, tw: f32, c: (u8, u8, u8), a: u8) {
        if w <= 0.0 || h <= 0.0 || a == 0 { return; }
        let rad = rad.max(0.0).min(w * 0.5).min(h * 0.5);
        let hw = w * 0.5;
        let hh = h * 0.5;
        let cx = x + hw;
        let cy = y + hh;
        let x0 = (x.floor() - 2.0) as i32;
        let y0 = (y.floor() - 2.0) as i32;
        let x1 = ((x + w).ceil() + 2.0) as i32;
        let y1 = ((y + h).ceil() + 2.0) as i32;
        let half = tw * 0.5;
        for py in y0..y1 {
            for px in x0..x1 {
                let qx = ((px as f32 + 0.5) - cx).abs() - (hw - rad);
                let qy = ((py as f32 + 0.5) - cy).abs() - (hh - rad);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                    + qx.max(qy).min(0.0) - rad;
                let cov = (half + 0.5 - d.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let aa = (a as f32 * cov + 0.5) as u8;
                    if aa > 0 { self.set_pixel(px, py, c.0, c.1, c.2, aa); }
                }
            }
        }
    }
    /// Anti-aliased filled annulus (the settings gear's body).
    fn ring(&mut self, cx: f32, cy: f32, r_out: f32, r_in: f32, c: (u8, u8, u8), a: u8) {
        if a == 0 || r_out <= r_in { return; }
        let x0 = (cx - r_out - 1.0).floor() as i32;
        let y0 = (cy - r_out - 1.0).floor() as i32;
        let x1 = (cx + r_out + 1.0).ceil() as i32;
        let y1 = (cy + r_out + 1.0).ceil() as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let d = (((px as f32 + 0.5) - cx).powi(2) + ((py as f32 + 0.5) - cy).powi(2)).sqrt();
                let cov = ((r_out - d).min(d - r_in) + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let aa = (a as f32 * cov + 0.5) as u8;
                    if aa > 0 { self.set_pixel(px, py, c.0, c.1, c.2, aa); }
                }
            }
        }
    }
    /// Fill a horizontal run with an opaque colour (straight copy, no blending).
    fn span_opaque(&mut self, x0: i32, x1: i32, y: i32, c: (u8, u8, u8)) {
        if y < 0 || y >= self.height as i32 { return; }
        let xs = x0.max(0) as usize;
        let xe = x1.min(self.width as i32).max(0) as usize;
        if xe <= xs { return; }
        let row = y as usize * self.width as usize * 4;
        for p in self.pixels[row + xs * 4..row + xe * 4].chunks_mut(4) {
            p[0] = c.2;
            p[1] = c.1;
            p[2] = c.0;
            p[3] = 255;
        }
    }
    /// A sheet whose outer `band` pixels are translucent, so the desktop shows
    /// through its edge the way light passes through the rim of a glass slab.
    /// The opaque face is filled span by span; only the rim costs per-pixel
    /// work, which keeps a resize drag responsive.
    fn glass_paper(&mut self, x: f32, y: f32, w: f32, h: f32, rad: f32, band: f32, anim: f32) {
        if w <= 0.0 || h <= 0.0 || anim <= 0.0 { return; }
        let rad = rad.max(0.0).min(w * 0.5).min(h * 0.5);
        let band = band.clamp(1.0, w * 0.25).clamp(1.0, h * 0.25);
        let (ix, iy) = (x + band, y + band);
        let (iw, ih) = (w - band * 2.0, h - band * 2.0);
        let irad = (rad - band).max(0.0);

        // 1. opaque face: one span per row, no per-pixel distance work
        let face_y0 = iy.ceil() as i32;
        let face_y1 = (iy + ih).floor() as i32;
        for py in face_y0..face_y1 {
            if let Some((sx, ex)) = rounded_span(ix, iy, iw, ih, irad, py) {
                self.span_opaque(sx.ceil() as i32, ex.floor() as i32, py, (255, 255, 255));
            }
        }

        // 2. glass rim: only the band, never the whole sheet
        let hw = w * 0.5;
        let hh = h * 0.5;
        let cx = x + hw;
        let cy = y + hh;
        let y0 = (y.floor() - 1.0) as i32;
        let y1 = ((y + h).ceil() + 1.0) as i32;
        for py in y0..y1 {
            let Some((ox0, ox1)) = rounded_span(x, y, w, h, rad, py) else { continue };
            // the interior of this row was already painted by the face pass
            let inner = rounded_span(ix, iy, iw, ih, irad, py);
            let (mut sx, mut ex) = (ox0.floor() as i32 - 1, ox1.ceil() as i32 + 1);
            if let Some((nx0, nx1)) = inner {
                // two stripes: left rim and right rim
                for px in sx..(nx0.floor() as i32).max(sx) {
                    self.paint_rim(px, py, cx, cy, hw, hh, rad, band, anim);
                }
                for px in (nx1.ceil() as i32).min(ex)..ex {
                    self.paint_rim(px, py, cx, cy, hw, hh, rad, band, anim);
                }
                continue;
            }
            // a row inside the top/bottom band: the whole span is rim
            if sx < 0 { sx = 0; }
            if ex > self.width as i32 { ex = self.width as i32; }
            for px in sx..ex {
                self.paint_rim(px, py, cx, cy, hw, hh, rad, band, anim);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_rim(&mut self, px: i32, py: i32, cx: f32, cy: f32, hw: f32, hh: f32, rad: f32, band: f32, anim: f32) {
        let qx = ((px as f32 + 0.5) - cx).abs() - (hw - rad);
        let qy = ((py as f32 + 0.5) - cy).abs() - (hh - rad);
        let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
            + qx.max(qy).min(0.0) - rad;
        let depth = -d;
        if depth <= 0.0 || depth >= band { return; }
        let cov = (0.5 + depth).clamp(0.0, 1.0);
        let t = (depth / band).min(1.0);
        let mut a = 42.0 + 213.0 * t;
        if depth < 1.1 { a = a.max(205.0); }
        a *= cov;
        let a = (a * anim).clamp(0.0, 255.0) as u8;
        if a > 0 { self.set_pixel(px, py, 255, 255, 255, a); }
    }
}

/// Horizontal extent of a rounded rect at pixel row `py`, in float pixels.
fn rounded_span(x: f32, y: f32, w: f32, h: f32, rad: f32, py: i32) -> Option<(f32, f32)> {
    if w <= 0.0 || h <= 0.0 { return None; }
    let cy = py as f32 + 0.5;
    if cy < y || cy > y + h { return None; }
    let rad = rad.max(0.0).min(w * 0.5).min(h * 0.5);
    let dy = if cy < y + rad {
        (y + rad) - cy
    } else if cy > y + h - rad {
        cy - (y + h - rad)
    } else {
        0.0
    };
    let inset = if dy > 0.0 {
        rad - (rad * rad - dy * dy).max(0.0).sqrt()
    } else {
        0.0
    };
    Some((x + inset, x + w - inset))
}

// ── Frosted backdrop (what the glass refracts) ──────────────────────────────

/// The desktop behind the overlay, grabbed once and heavily blurred. Cards
/// sample it instead of the raw screen, which is what makes them read as
/// frosted glass rather than a tinted rectangle.
struct Backdrop { w: u32, h: u32, px: Vec<u8> } // straight BGRA, 1/4 scale

static BACKDROP: Mutex<Option<Backdrop>> = Mutex::new(None);

impl Backdrop {
    #[inline]
    fn at(&self, x: i32, y: i32) -> (u8, u8, u8) {
        let sx = (x / 4).clamp(0, self.w as i32 - 1) as usize;
        let sy = (y / 4).clamp(0, self.h as i32 - 1) as usize;
        let i = (sy * self.w as usize + sx) * 4;
        (self.px[i + 2], self.px[i + 1], self.px[i])
    }
}

/// Snapshot the desktop and blur it down to a quarter resolution. Prefers the
/// wallpaper itself (rendered by the Program Manager, so no app windows leak
/// in); falls back to the composited screen, where BitBlt without CAPTUREBLT
/// skips layered windows — that is, skips this overlay.
fn capture_backdrop(vx: i32, vy: i32, vw: i32, vh: i32) -> Option<Backdrop> {
    if vw <= 0 || vh <= 0 { return None; }
    if let Some(bd) = wallpaper_backdrop(vw as u32, vh as u32) { return Some(bd); }
    if let Some(bd) = capture_wallpaper(vw, vh) { return Some(bd); }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() { return None; }
        let hdc = CreateCompatibleDC(screen);
        if hdc.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let (dib, bits) = create_dib(hdc, vw as u32, vh as u32);
        if dib.is_null() || bits.is_null() {
            DeleteDC(hdc);
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        SelectObject(hdc, dib as *mut _);
        let copied = BitBlt(hdc, 0, 0, vw, vh, screen, vx, vy, SRCCOPY) != 0;
        let out = if copied {
            let src = std::slice::from_raw_parts(bits, (vw as usize) * (vh as usize) * 4);
            downsample_and_blur(src, vw as usize, vh as usize)
        } else {
            None
        };
        SelectObject(hdc, std::ptr::null_mut());
        DeleteObject(dib as *mut _);
        DeleteDC(hdc);
        ReleaseDC(std::ptr::null_mut(), screen);
        out
    }
}

/// Decode the desktop wallpaper itself and scale it to fill the virtual screen.
/// This is the glass's ideal light source: stable, colourful, and independent of
/// whichever window happens to be open behind the cards.
fn wallpaper_backdrop(vw: u32, vh: u32) -> Option<Backdrop> {
    use winapi::um::winuser::{SystemParametersInfoW, SPI_GETDESKWALLPAPER};
    let mut buf = [0u16; 1024];
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            buf.as_mut_ptr() as *mut _,
            0,
        )
    };
    if ok == 0 { return None; }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if len == 0 { return None; }
    let path = String::from_utf16_lossy(&buf[..len]);
    let bytes = fs::read(&path).ok()?;

    let (iw, ih, rgb) = decode_image(&bytes, &path)?;
    if iw == 0 || ih == 0 { return None; }
    // aspect-fill, like the default Windows wallpaper mode
    let (dw, dh) = ((vw / 4).max(1) as usize, (vh / 4).max(1) as usize);
    let scale = (dw as f32 / iw as f32).max(dh as f32 / ih as f32);
    let off_x = (iw as f32 - dw as f32 / scale) * 0.5;
    let off_y = (ih as f32 - dh as f32 / scale) * 0.5;
    let mut px = vec![0u8; dw * dh * 4];
    for y in 0..dh {
        let sy = ((off_y + y as f32 / scale) as usize).min(ih - 1);
        for x in 0..dw {
            let sx = ((off_x + x as f32 / scale) as usize).min(iw - 1);
            let s = (sy * iw + sx) * 3;
            let d = (y * dw + x) * 4;
            px[d] = rgb[s + 2];
            px[d + 1] = rgb[s + 1];
            px[d + 2] = rgb[s];
            px[d + 3] = 255;
        }
    }
    box_blur(&mut px, dw, dh, 2);
    box_blur(&mut px, dw, dh, 2);
    grade_backdrop(&mut px);
    Some(Backdrop { w: dw as u32, h: dh as u32, px })
}

/// Decode a wallpaper file into (width, height, RGB8).
fn decode_image(bytes: &[u8], path: &str) -> Option<(usize, usize, Vec<u8>)> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
        let mut buf = vec![0u8; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let (w, h) = (info.width as usize, info.height as usize);
        let ch = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            _ => return None,
        };
        let mut rgb = Vec::with_capacity(w * h * 3);
        for i in 0..w * h {
            rgb.push(buf[i * ch]);
            rgb.push(buf[i * ch + 1]);
            rgb.push(buf[i * ch + 2]);
        }
        return Some((w, h, rgb));
    }
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let data = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => data,
        jpeg_decoder::PixelFormat::L8 => {
            let mut v = Vec::with_capacity(w * h * 3);
            for p in data { v.push(p); v.push(p); v.push(p); }
            v
        }
        _ => return None,
    };
    Some((w, h, rgb))
}

/// Ask the desktop window to render itself: wallpaper without the clutter of
/// whatever windows happen to be open.
fn capture_wallpaper(vw: i32, vh: i32) -> Option<Backdrop> {    unsafe {
        let class = wide("Progman");
        let progman = FindWindowW(class.as_ptr(), std::ptr::null());
        if progman.is_null() { return None; }
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() { return None; }
        let hdc = CreateCompatibleDC(screen);
        if hdc.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let (dib, bits) = create_dib(hdc, vw as u32, vh as u32);
        if dib.is_null() || bits.is_null() {
            DeleteDC(hdc);
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        SelectObject(hdc, dib as *mut _);
        // PW_RENDERFULLCONTENT keeps DWM-composed content (the wallpaper) in
        let ok = PrintWindow(progman, hdc, 2) != 0;
        let mut out = None;
        if ok {
            let src = std::slice::from_raw_parts(bits, (vw as usize) * (vh as usize) * 4);
            // a flat answer means the wallpaper lives on another window: reject
            let (mut lo, mut hi) = (255u8, 0u8);
            let mut i = 0usize;
            while i < src.len() {
                let l = src[i];
                lo = lo.min(l);
                hi = hi.max(l);
                i += 4099;
            }
            if hi.saturating_sub(lo) > 6 {
                out = downsample_and_blur(src, vw as usize, vh as usize);
            }
        }
        SelectObject(hdc, std::ptr::null_mut());
        DeleteObject(dib as *mut _);
        DeleteDC(hdc);
        ReleaseDC(std::ptr::null_mut(), screen);
        out
    }
}

/// Colour-grade the blurred backdrop once, at build time: a little more
/// saturation and a scatter lift toward white, which is how thick glass carries
/// colour and light. Doing it here keeps the per-frame blit a plain sample.
fn grade_backdrop(px: &mut [u8]) {
    let mut i = 0;
    while i + 3 < px.len() {
        let (b, g, r) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
        let luma = (r * 77 + g * 150 + b * 29) >> 8;
        let grade = |c: i32| -> u8 {
            let s = (luma + (c - luma) * 3 / 2).clamp(0, 255);
            ((s * 205) / 255 + 42).clamp(0, 255) as u8
        };
        px[i] = grade(b);
        px[i + 1] = grade(g);
        px[i + 2] = grade(r);
        i += 4;
    }
}

fn downsample_and_blur(src: &[u8], w: usize, h: usize) -> Option<Backdrop> {
    if w < 8 || h < 8 { return None; }
    let (dw, dh) = ((w / 4).max(1), (h / 4).max(1));
    let mut px = vec![0u8; dw * dh * 4];
    for y in 0..dh {
        for x in 0..dw {
            let (mut b, mut g, mut r, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..4 {
                let sy = (y * 4 + dy).min(h - 1);
                for dx in 0..4 {
                    let sx = (x * 4 + dx).min(w - 1);
                    let i = (sy * w + sx) * 4;
                    b += src[i] as u32;
                    g += src[i + 1] as u32;
                    r += src[i + 2] as u32;
                    a += src[i + 3] as u32;
                }
            }
            let o = (y * dw + x) * 4;
            px[o] = (b / 16) as u8;
            px[o + 1] = (g / 16) as u8;
            px[o + 2] = (r / 16) as u8;
            px[o + 3] = (a / 16) as u8;
        }
    }
    // two box passes ≈ a soft gaussian; at 1/4 scale this is a wide blur on screen
    box_blur(&mut px, dw, dh, 2);
    box_blur(&mut px, dw, dh, 2);
    grade_backdrop(&mut px);
    Some(Backdrop { w: dw as u32, h: dh as u32, px })
}

/// In-place separable box blur over BGRA bytes (prefix sums, clamped edges).
fn box_blur(px: &mut [u8], w: usize, h: usize, radius: usize) {
    if radius == 0 || w == 0 || h == 0 { return; }
    let mut tmp = px.to_vec();
    let mut pref = vec![0u32; w.max(h) + 1];
    for y in 0..h {
        let row = y * w * 4;
        for c in 0..4 {
            for x in 0..w {
                pref[x + 1] = pref[x] + px[row + x * 4 + c] as u32;
            }
            for x in 0..w {
                let l = x.saturating_sub(radius);
                let r = (x + radius).min(w - 1);
                let n = (r - l + 1) as u32;
                tmp[row + x * 4 + c] = ((pref[r + 1] - pref[l]) / n) as u8;
            }
        }
    }
    for x in 0..w {
        for c in 0..4 {
            for y in 0..h {
                pref[y + 1] = pref[y] + tmp[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                let l = y.saturating_sub(radius);
                let r = (y + radius).min(h - 1);
                let n = (r - l + 1) as u32;
                px[(y * w + x) * 4 + c] = ((pref[r + 1] - pref[l]) / n) as u8;
            }
        }
    }
}

// ── Shell thumbnails ─────────────────────────────────────────────────────────

struct Thumbnail { width: u32, height: u32, data: Vec<u8> } // BGRA

impl Thumbnail {
    /// Share of sampled pixels that carry visible ink. A blank sheet answers
    /// ~0, a page of text or a photo answers well above the reject threshold.
    fn ink_ratio(&self) -> f32 {
        let step = 3u32;
        let (mut ink, mut total) = (0u32, 0u32);
        let mut y = 0;
        while y < self.height {
            let mut x = 0;
            while x < self.width {
                let i = ((y * self.width + x) * 4) as usize;
                let lum = (self.data[i] as u32 + self.data[i + 1] as u32 + self.data[i + 2] as u32) / 3;
                if self.data[i + 3] > 24 && lum < 232 { ink += 1; }
                total += 1;
                x += step;
            }
            y += step;
        }
        if total == 0 { 0.0 } else { ink as f32 / total as f32 }
    }
}

/// Get a real document thumbnail via the Windows Shell thumbnail factory.
/// Works for anything Explorer can preview (PDF, Office, images, videos...).
fn shell_thumbnail(path: &str, target: u32) -> Option<Thumbnail> {
    unsafe {
        use windows::Win32::UI::Shell::{
            IShellItemImageFactory, IShellItem, SHCreateItemFromParsingName, SIIGBF_RESIZETOFIT,
            SIIGBF_THUMBNAILONLY,
        };
        use windows::core::{Interface, PCWSTR};

        // The shell parser rejects forward slashes in a drive path.
        let native = path.replace('/', "\\");
        let path_w: Vec<u16> = native.encode_utf16().chain(std::iter::once(0)).collect();
        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(path_w.as_ptr()), None).ok()?;
        let factory: IShellItemImageFactory = item.cast().ok()?;
        // THUMBNAILONLY: a file without a preview handler must fail here rather
        // than hand back a giant app icon to stretch across the page.
        let hb = factory
            .GetImage(
                windows::Win32::Foundation::SIZE { cx: target as i32, cy: target as i32 },
                SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY,
            )
            .ok()?;

        let thumb = hbitmap_to_bgra(hb);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(
            windows::Win32::Graphics::Gdi::HGDIOBJ(hb.0),
        );
        let thumb = thumb?;
        // Some providers answer with a blank sheet; that reads as an empty card,
        // so fall through to our drawn placeholder instead.
        if thumb.ink_ratio() < 0.004 { return None; }
        Some(thumb)
    }
}

/// Rasterise page 1 of a PDF through the Windows PDF engine: most machines ship
/// no shell thumbnail provider for PDFs, but they do have Windows.Data.Pdf.
fn pdf_first_page(path: &str, target_w: u32) -> Option<Thumbnail> {
    use windows::core::HSTRING;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};

    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).ok()?.get().ok()?;
        let doc = PdfDocument::LoadFromFileAsync(&file).ok()?.get().ok()?;
        if doc.PageCount().ok()? == 0 { return None; }
        let page = doc.GetPage(0).ok()?;
        let stream = InMemoryRandomAccessStream::new().ok()?;
        let options = PdfPageRenderOptions::new().ok()?;
        options.SetDestinationWidth(target_w).ok()?;
        page.RenderWithOptionsToStreamAsync(&stream, &options).ok()?.get().ok()?;
        let size = stream.Size().ok()? as usize;
        if size == 0 { return None; }
        let input = stream.GetInputStreamAt(0).ok()?;
        let reader = DataReader::CreateDataReader(&input).ok()?;
        reader.LoadAsync(size as u32).ok()?.get().ok()?;
        let mut png = vec![0u8; size];
        reader.ReadBytes(&mut png).ok()?;
        png_to_thumbnail(&png)
    }
}

/// Decode the PNG the PDF engine produced into our BGRA thumbnail form.
fn png_to_thumbnail(data: &[u8]) -> Option<Thumbnail> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 { return None; }
    let channels = match info.color_type {
        png::ColorType::Rgba => 4usize,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        _ => return None,
    };
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for i in 0..(w as usize * h as usize) {
        let s = i * channels;
        if s + channels > buf.len() { break; }
        let (r, g, b, a) = match channels {
            4 => (buf[s], buf[s + 1], buf[s + 2], buf[s + 3]),
            3 => (buf[s], buf[s + 1], buf[s + 2], 255),
            2 => (buf[s], buf[s], buf[s], buf[s + 1]),
            _ => (buf[s], buf[s], buf[s], 255),
        };
        out[i * 4] = b;
        out[i * 4 + 1] = g;
        out[i * 4 + 2] = r;
        out[i * 4 + 3] = a;
    }
    Some(Thumbnail { width: w, height: h, data: out })
}

/// Preview for one file: the shell's thumbnail when it has one, otherwise our
/// own renderer for the formats we can handle.
fn file_preview(path: &str, target: u32) -> Option<Thumbnail> {
    if let Some(t) = shell_thumbnail(path, target) {
        return Some(t);
    }
    let ext = FsPath::new(path).extension().unwrap_or_default().to_string_lossy().to_uppercase();
    if ext == "PDF" {
        // only pay for a page render when there is no text layer to show
        if pdf_text(path).is_empty() {
            return pdf_first_page(path, target);
        }
    }
    if ext == "PPTX" || ext == "PPTM" || ext == "POTX" {
        // no thumbnail handler installed (no Office): use the deck's own first
        // slide picture instead of a text-only page
        return pptx_slide_image(path);
    }
    None
}

/// Convert a 32bpp HBITMAP into a top-down BGRA pixel buffer.
unsafe fn hbitmap_to_bgra(hb: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Thumbnail> {
    use windows::Win32::Graphics::Gdi::{BITMAP, GetObjectW, HGDIOBJ};

    let mut bm = BITMAP::default();
    if GetObjectW(
        HGDIOBJ(hb.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    ) == 0
    {
        return None;
    }
    let w = bm.bmWidth.max(0) as u32;
    let h = bm.bmHeight.max(0) as u32;
    if w == 0 || h == 0 {
        return None;
    }

    let hdc_winapi = GetDC(std::ptr::null_mut());
    let hdc = windows::Win32::Graphics::Gdi::HDC(hdc_winapi as *mut _);
    let mut bmi = windows::Win32::Graphics::Gdi::BITMAPINFO {
        bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
            biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default(); 1],
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let rows = windows::Win32::Graphics::Gdi::GetDIBits(
        hdc,
        hb,
        0,
        h,
        Some(buf.as_mut_ptr() as *mut _),        &mut bmi,
        windows::Win32::Graphics::Gdi::DIB_RGB_COLORS,
    );
    ReleaseDC(std::ptr::null_mut(), hdc_winapi);
    if rows == 0 {
        return None;
    }

    // Some providers return an all-zero alpha channel; treat as opaque
    let mut max_a: u8 = 0;
    for i in (3..buf.len()).step_by(4) {
        if buf[i] > max_a { max_a = buf[i]; }
    }
    if max_a < 16 {
        for i in (3..buf.len()).step_by(4) { buf[i] = 255; }
    }

    Some(Thumbnail { width: w, height: h, data: buf })
}

fn generate_thumbnails(cards: &[Card], thumbs: &mut HashMap<String, Thumbnail>) {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }
    for card in cards {
        if !thumbs.contains_key(&card.path) {
            if let Some(t) = file_preview(&card.path, THUMB_PX) {
                thumbs.insert(card.path.clone(), t);
            }
        }
    }
}

/// Fit-width draw of a thumbnail into the preview area (document pages fill
/// the card width); vertically centered when shorter, top-clipped when taller.
fn draw_thumbnail(fb: &mut Framebuffer, thumb: &Thumbnail, px: i32, py: i32, pw: i32, ph: i32) {
    if thumb.width == 0 || thumb.height == 0 || pw <= 1 || ph <= 1 {
        return;
    }
    let scale = pw as f32 / thumb.width as f32;
    let dh = (thumb.height as f32 * scale).round().max(1.0) as i32;
    let dy = if dh <= ph { py + (ph - dh) / 2 } else { py };
    let clip_h = dh.min(ph);
    let dx = px;

    let src_w = thumb.width;
    let src_h = thumb.height;
    let data = &thumb.data;

    for yy in 0..clip_h {
        for xx in 0..pw {
            let fx = (xx as f32 + 0.5) / scale - 0.5;
            let fy = (yy as f32 + 0.5) / scale - 0.5;
            let x0 = (fx.floor().max(0.0) as u32).min(src_w - 1);
            let y0 = (fy.floor().max(0.0) as u32).min(src_h - 1);
            let x1 = (x0 + 1).min(src_w - 1);
            let y1 = (y0 + 1).min(src_h - 1);
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);
            let ty = (fy - y0 as f32).clamp(0.0, 1.0);

            let i00 = ((y0 * src_w + x0) * 4) as usize;
            let i10 = ((y0 * src_w + x1) * 4) as usize;
            let i01 = ((y1 * src_w + x0) * 4) as usize;
            let i11 = ((y1 * src_w + x1) * 4) as usize;

            fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }
            let r = lerp(
                lerp(data[i00 + 2] as f32, data[i10 + 2] as f32, tx),
                lerp(data[i01 + 2] as f32, data[i11 + 2] as f32, tx),
                ty,
            );
            let g = lerp(
                lerp(data[i00 + 1] as f32, data[i10 + 1] as f32, tx),
                lerp(data[i01 + 1] as f32, data[i11 + 1] as f32, tx),
                ty,
            );
            let b = lerp(
                lerp(data[i00] as f32, data[i10] as f32, tx),
                lerp(data[i01] as f32, data[i11] as f32, tx),
                ty,
            );
            let a = lerp(
                lerp(data[i00 + 3] as f32, data[i10 + 3] as f32, tx),
                lerp(data[i01 + 3] as f32, data[i11 + 3] as f32, tx),
                ty,
            );
            let (r, g, b) = if a <= 1.0 { (0.0, 0.0, 0.0) } else { (r, g, b) };
            fb.set_pixel(dx + xx, dy + yy, r as u8, g as u8, b as u8, a as u8);
        }
    }
}

// ── Document content extraction ─────────────────────────────────────────────
//
// Cards should show what is actually inside a file, not a grey stand-in. For
// text-bearing formats we read the content ourselves (OOXML is a zip of XML)
// and lay it out at a size that stays readable; PDFs are rasterised by the
// Windows PDF engine into a real page image.

/// One piece of a document, in reading order.
#[derive(Clone, Debug)]
enum Block {
    Title(String),
    Heading(String),
    Para(String),
    Bullet(String),
    Table(Vec<Vec<String>>),
}

fn xml_unescape(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => {
                if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else if let Some(dec) = ent.strip_prefix('#') {
                    dec.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Concatenated text of every `<tag ...>…</tag>` in `xml`.
fn xml_tag_texts(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        // `<w:t>` / `<w:t xml:space=…>` but not `<w:tab/>`, `<w:tbl>`
        if !after.starts_with('>') && !after.starts_with(' ') { 
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else { break };
        out.push_str(&xml_unescape(&body[..end]));
        rest = &body[end + close.len()..];
    }
    out
}

/// Read one member of a zip container (docx/xlsx/pptx are zips).
fn zip_entry(path: &str, names: &[&str]) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
    for want in names {
        if let Ok(mut entry) = archive.by_name(want) {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

fn docx_blocks(xml: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for para in xml.split("</w:p>") {
        if !para.contains("<w:p") { continue; }
        let text = xml_tag_texts(para, "w:t");
        let text = text.trim().to_string();
        if text.is_empty() { continue; }
        let style = para.find("w:pStyle")
            .and_then(|i| para[i..].find("w:val=\"").map(|j| &para[i + j + 7..]))
            .and_then(|r| r.find('"').map(|k| &r[..k]))
            .unwrap_or("");
        let block = if style.starts_with("Title") {
            Block::Title(text)
        } else if style.starts_with("Heading") {
            Block::Heading(text)
        } else if style.contains("List") {
            Block::Bullet(text)
        } else if out.is_empty() {
            Block::Title(text)
        } else {
            Block::Para(text)
        };
        out.push(block);
        if out.len() >= 40 { break; }
    }
    out
}

/// Turn an encoded image (png/jpeg/bmp) into a BGRA thumbnail capped at `target`.
fn image_to_thumbnail(bytes: &[u8], hint: &str, target: u32) -> Option<Thumbnail> {
    let (iw, ih, rgb) = decode_image(bytes, hint)?;
    if iw == 0 || ih == 0 { return None; }
    let scale = (target as f32 / iw as f32).min(target as f32 / ih as f32).min(1.0);
    let tw = ((iw as f32 * scale).round() as usize).max(1);
    let th = ((ih as f32 * scale).round() as usize).max(1);
    let mut data = vec![0u8; tw * th * 4];
    for y in 0..th {
        let sy = (y * ih / th).min(ih - 1);
        for x in 0..tw {
            let sx = (x * iw / tw).min(iw - 1);
            let s = (sy * iw + sx) * 3;
            let d = (y * tw + x) * 4;
            data[d] = rgb[s + 2];
            data[d + 1] = rgb[s + 1];
            data[d + 2] = rgb[s];
            data[d + 3] = 255;
        }
    }
    Some(Thumbnail { width: tw as u32, height: th as u32, data })
}

fn attr_value(chunk: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let i = chunk.find(&needle)?;
    let after = &chunk[i + needle.len()..];
    let j = after.find('"')?;
    Some(after[..j].to_string())
}

/// Every `r:embed="rIdN"` in a slide, in document order.
fn slide_embeds(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("r:embed=\"") {
        let after = &rest[i + 9..];
        match after.find('"') {
            Some(j) => {
                out.push(after[..j].to_string());
                rest = &after[j..];
            }
            None => break,
        }
    }
    out
}

/// `Id -> Target` pairs from a `.rels` part.
fn rel_targets(xml: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for chunk in xml.split("<Relationship ").skip(1) {
        if let (Some(id), Some(target)) = (attr_value(chunk, "Id"), attr_value(chunk, "Target")) {
            out.push((id, target));
        }
    }
    out
}

/// Resolve a relationship target that is relative to `ppt/slides/`.
fn resolve_media(target: &str) -> Option<String> {
    let t = target.replace('\\', "/");
    let path = if let Some(rest) = t.strip_prefix("../") {
        format!("ppt/{rest}")
    } else if let Some(rest) = t.strip_prefix('/') {
        rest.to_string()
    } else {
        format!("ppt/slides/{t}")
    };
    if path.contains("..") { None } else { Some(path) }
}

/// The biggest picture on the first slide, decoded. A designed deck is mostly a
/// picture, so this is the closest thing to a real slide preview we can render.
fn pptx_slide_image(path: &str) -> Option<Thumbnail> {
    // the cover slide is often text-only: walk the first slides, then fall back
    // to the layout background the deck inherits
    for n in 1..=3 {
        if let Some(t) = pptx_picture_of(path, "slides", &format!("slide{n}")) { return Some(t); }
    }
    let rels_name = "ppt/slides/_rels/slide1.xml.rels".to_string();
    let rels = zip_entry(path, &[rels_name.as_str()])?;
    let rels = String::from_utf8_lossy(&rels).to_string();
    for (_, target) in rel_targets(&rels) {
        let Some(part) = resolve_media(&target) else { continue };
        let Some(stem) = part.strip_prefix("ppt/slideLayouts/") else { continue };
        let Some(stem) = stem.strip_suffix(".xml") else { continue };
        if let Some(t) = pptx_picture_of(path, "slideLayouts", stem) { return Some(t); }
    }
    None
}

/// Biggest picture referenced by one slide part (`kind` = slides / slideLayouts).
fn pptx_picture_of(path: &str, kind: &str, stem: &str) -> Option<Thumbnail> {
    let rels_name = format!("ppt/{kind}/_rels/{stem}.xml.rels");
    let part_name = format!("ppt/{kind}/{stem}.xml");
    let rels = zip_entry(path, &[rels_name.as_str()])?;
    let rels = String::from_utf8_lossy(&rels).to_string();
    let pairs = rel_targets(&rels);
    let body = zip_entry(path, &[part_name.as_str()])?;
    let body = String::from_utf8_lossy(&body).to_string();

    let mut best: Option<Thumbnail> = None;
    let mut best_area = 0usize;
    for id in slide_embeds(&body) {
        let Some((_, target)) = pairs.iter().find(|(rid, _)| rid == &id) else { continue };
        let Some(part) = resolve_media(target) else { continue };
        let Some(bytes) = zip_entry(path, &[part.as_str()]) else { continue };
        let Some(t) = image_to_thumbnail(&bytes, &part, 900) else { continue };
        // only a slide-shaped picture is worth showing: a square logo or a tiny
        // decoration would make a terrible card, and the text page reads better
        let aspect = t.width as f32 / t.height.max(1) as f32;
        if !(1.15..=2.6).contains(&aspect) || t.width < 400 {
            continue;
        }
        let area = (t.width * t.height) as usize;
        if area > best_area {
            best_area = area;
            best = Some(t);
        }
    }
    best
}


/// One paragraph of a slide, with the style of its first run.
struct SlideLine {
    text: String,
    size_pt: f32,
    bold: bool,
    color: (u8, u8, u8),
    align: u8, // 0 left, 1 centre, 2 right
    bullet: bool,
}

struct SlideShape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lines: Vec<SlideLine>,
    image: Option<Thumbnail>,
}

/// A first slide we can actually draw: EMU geometry, solid background, text
/// boxes and pictures.
struct Slide {
    w: f32,
    h: f32,
    bg: Option<(u8, u8, u8)>,
    shapes: Vec<SlideShape>,
}

fn emu_attr(chunk: &str, name: &str) -> f32 {
    attr_value(chunk, name)
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0)
}

fn hex_color(raw: &str) -> Option<(u8, u8, u8)> {
    if raw.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(raw, 16).ok()?;
    Some((((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8))
}

/// Pull `algn`, bullet flag and the first run's style out of a `<a:p>` chunk.
fn slide_line(para: &str) -> Option<SlideLine> {
    let text = xml_tag_texts(para, "a:t");
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let first_run = para.split("<a:r>").nth(1).unwrap_or(para);
    let size_pt = attr_value(first_run, "sz")
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v / 100.0)
        .unwrap_or(18.0);
    let bold = attr_value(first_run, "b").map(|v| v == "1").unwrap_or(false);
    let color = first_run
        .find("<a:srgbClr ")
        .and_then(|i| hex_color(&attr_value(&first_run[i..], "val").unwrap_or_default()))
        .unwrap_or((40, 40, 44));
    let align = match attr_value(para, "algn").unwrap_or_default().as_str() {
        "ctr" => 1,
        "r" => 2,
        _ => 0,
    };
    let bullet = para.contains("<a:buChar") || para.contains("<a:buAutoNum");
    Some(SlideLine { text, size_pt, bold, color, align, bullet })
}

/// Parse the first slide of a deck into something drawable.
fn slide_model(path: &str, n: u32) -> Option<Slide> {
    let size = zip_entry(path, &["ppt/presentation.xml"])?;
    let size = String::from_utf8_lossy(&size).to_string();
    let (sw, sh) = size
        .find("<p:sldSz ")
        .map(|i| {
            let chunk = &size[i..];
            (emu_attr(chunk, "cx"), emu_attr(chunk, "cy"))
        })
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
        .unwrap_or((12192000.0, 6858000.0));

    let rels_name = format!("ppt/slides/_rels/slide{n}.xml.rels");
    let part_name = format!("ppt/slides/slide{n}.xml");
    let rels = zip_entry(path, &[rels_name.as_str()])?;
    let rels = String::from_utf8_lossy(&rels).to_string();
    let pairs = rel_targets(&rels);
    let body = zip_entry(path, &[part_name.as_str()])?;
    let body = String::from_utf8_lossy(&body).to_string();

    let bg = body
        .find("<p:bg>")
        .and_then(|i| body[i..].find("<a:srgbClr "))
        .map(|i| i + body.find("<p:bg>").unwrap())
        .and_then(|i| hex_color(&attr_value(&body[i..], "val").unwrap_or_default()));

    let mut shapes = Vec::new();
    let mut rest = body.as_str();
    // shapes and pictures interleave, so always take whichever comes first
    while let Some(pos) = match (rest.find("<p:sp>"), rest.find("<p:pic>")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    } {
        let chunk_all = &rest[pos..];
        let is_pic = chunk_all.starts_with("<p:pic>");
        let close = if is_pic { "</p:pic>" } else { "</p:sp>" };
        let end = chunk_all.find(close).map(|e| e + close.len()).unwrap_or(chunk_all.len());
        let chunk = &chunk_all[..end.min(2400)];
        rest = &chunk_all[end.min(chunk_all.len())..];

        let xfrm = chunk.find("<a:xfrm").unwrap_or(0);
        let geom = &chunk[xfrm..];
        let x = emu_attr(geom, "x");
        let y = emu_attr(geom, "y");
        let w = emu_attr(geom, "cx");
        let h = emu_attr(geom, "cy");
        if w <= 0.0 || h <= 0.0 {
            continue;
        }

        let mut lines = Vec::new();
        for para in chunk.split("</a:p>") {
            if let Some(line) = slide_line(para) {
                lines.push(line);
            }
        }

        let mut image = None;
        if is_pic {
            for id in slide_embeds(chunk) {
                let Some((_, target)) = pairs.iter().find(|(rid, _)| rid == &id) else { continue };
                let Some(part) = resolve_media(target) else { continue };
                let Some(bytes) = zip_entry(path, &[part.as_str()]) else { continue };
                image = image_to_thumbnail(&bytes, &part, 700);
                if image.is_some() {
                    break;
                }
            }
        }
        if lines.is_empty() && image.is_none() {
            continue;
        }
        shapes.push(SlideShape { x, y, w, h, lines, image });
    }

    if shapes.is_empty() {
        return None;
    }
    if std::env::var("BD_SLIDE_DEBUG").is_ok() {
        eprintln!("slide {n}: {}x{} emu, bg={:?}, shapes={}", sw, sh, bg, shapes.len());
        for s in &shapes {
            eprintln!("  shape {:.0},{:.0} {:.0}x{:.0} lines={} image={}",
                      s.x, s.y, s.w, s.h, s.lines.len(), s.image.is_some());
        }
    }
    Some(Slide { w: sw, h: sh, bg, shapes })
}

/// Paint a parsed slide into the paper rect, scaled to fit.
fn draw_slide(fb: &mut Framebuffer, slide: &Slide, px: f32, py: f32, pw: f32, ph: f32, radius: f32) {
    // fit the slide into the paper, centred, and keep the paper's rounded corner
    let scale = (pw / slide.w).min(ph / slide.h);
    let body_w = slide.w * scale;
    let body_h = slide.h * scale;
    let sx0 = px + (pw - body_w) * 0.5;
    let sy0 = py + (ph - body_h) * 0.5;
    if let Some(bg) = slide.bg {
        fb.round_rect(sx0, sy0, body_w, body_h, radius.max(6.0), bg, 255);
    }
    for shape in &slide.shapes {
        let sx = (sx0 + shape.x * scale).max(sx0);
        let sy = (sy0 + shape.y * scale).max(sy0);
        let sw = (shape.w * scale).max(1.0).min(sx0 + body_w - sx);
        let sh = (shape.h * scale).max(1.0).min(sy0 + body_h - sy);
        if sw < 2.0 || sh < 2.0 {
            continue;
        }
        if let Some(img) = &shape.image {
            draw_thumbnail(fb, img, sx as i32, sy as i32, sw as i32, sh as i32);
        }
        if shape.lines.is_empty() {
            continue;
        }
        let mut y = sy;
        for line in &shape.lines {
            let size = (line.size_pt * 12700.0 * scale).max(6.0);
            if size < 7.0 || y > sy + sh + size * 2.0 {
                break;
            }
            let text = if line.bullet { format!("· {}", line.text) } else { line.text.clone() };
            for wrapped in wrap_lines(&text, size, sw * 0.98) {
                draw_text(
                    fb, &wrapped, sx as i32, y as i32, sw as i32,
                    size as i32, if line.bold { 700 } else { 400 }, line.color, line.align,
                );
                y += size * 1.32;
            }
        }
    }
}

fn pptx_blocks(path: &str) -> Vec<Block> {
    // the first slides carry the message; slide one supplies the caption
    let mut out = Vec::new();
    for n in 1..=3 {
        let name = format!("ppt/slides/slide{n}.xml");
        let Some(bytes) = zip_entry(path, &[name.as_str()]) else { continue };
        let xml = String::from_utf8_lossy(&bytes).to_string();
        for para in xml.split("</a:p>") {
            let text = xml_tag_texts(para, "a:t").trim().to_string();
            if text.is_empty() { continue; }
            if out.is_empty() {
                out.push(Block::Title(text));
            } else {
                out.push(Block::Bullet(text));
            }
            if out.len() >= 26 { return out; }
        }
    }
    out
}

/// Trim the float noise spreadsheets carry in their XML (8.699999999999999).
fn fmt_number(raw: &str) -> String {
    let raw = raw.trim();
    match raw.parse::<f64>() {
        Ok(v) if raw.chars().any(|c| c.is_ascii_digit()) => {
            if (v - v.round()).abs() < 1e-9 {
                format!("{}", v.round() as i64)
            } else {
                let s = format!("{v:.2}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        }
        _ => raw.to_string(),
    }
}

fn xlsx_blocks(path: &str) -> Option<Vec<Block>> {
    let shared: Vec<String> = zip_entry(path, &["xl/sharedStrings.xml"])
        .map(|b| {
            let s = String::from_utf8_lossy(&b).to_string();
            s.split("</si>").map(|si| xml_tag_texts(si, "t").trim().to_string()).collect()
        })
        .unwrap_or_default();
    let sheet = zip_entry(path, &["xl/worksheets/sheet1.xml"])?;
    let sheet = String::from_utf8_lossy(&sheet).to_string();
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in sheet.split("</row>") {
        if !row.contains("<c") { continue; }
        let mut cells = Vec::new();
        let mut rest = row;
        while let Some(mut i) = rest.find("<c") {
            // `<c ` is a cell; skip `<col …>` and friends
            if !(rest[i + 2..].starts_with('>') || rest[i + 2..].starts_with(' ')) {
                i += 2;
                rest = &rest[i..];
                continue;
            }
            let after = &rest[i..];
            let Some(gt) = after.find('>') else { break };
            let head = &after[..gt];
            let body = &after[gt + 1..];
            let Some(end) = body.find("</c>") else { break };
            let inner = &body[..end];
            let raw = xml_tag_texts(inner, "v");
            let value = if head.contains("t=\"s\"") {
                raw.trim().parse::<usize>().ok()
                    .and_then(|i| shared.get(i).cloned()).unwrap_or_default()
            } else if head.contains("inlineStr") {
                xml_tag_texts(inner, "t")
            } else if raw.trim().is_empty() {
                raw
            } else {
                fmt_number(&raw)
            };
            cells.push(value.trim().to_string());
            rest = &body[end..];
        }
        if cells.iter().any(|c| !c.is_empty()) {
            cells.truncate(6);
            rows.push(cells);
        }
        if rows.len() >= 14 { break; }
    }
    if rows.is_empty() { None } else { Some(vec![Block::Table(rows)]) }
}

const TEXT_EXTS: &[&str] = &[
    "TXT", "MD", "MARKDOWN", "JSON", "CSV", "LOG", "INI", "TOML", "YAML", "YML",
    "XML", "HTML", "HTM", "CSS", "JS", "TS", "RS", "PY", "SH", "BAT", "PS1",
];

/// Turn markdown-ish source into the shapes the page renderer understands.
fn md_blocks(lines: &[String]) -> Vec<Block> {
    let mut out = Vec::new();
    for line in lines {
        let l = line.trim();
        if l.is_empty() { continue; }
        if let Some(rest) = l.strip_prefix('#') {
            let title = rest.trim_start_matches('#').trim();
            if !title.is_empty() {
                out.push(if out.is_empty() {
                    Block::Title(title.to_string())
                } else {
                    Block::Heading(title.to_string())
                });
            }
        } else if let Some(rest) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
            out.push(Block::Bullet(rest.trim().to_string()));
        } else if let Some(rest) = l.strip_prefix("> ") {
            out.push(Block::Para(rest.trim().to_string()));
        } else {
            out.push(Block::Para(l.to_string()));
        }
        if out.len() >= 30 { break; }
    }
    out
}

/// Pull the text layer out of a PDF, so a text PDF reads like a document
/// instead of a postage-stamp page render. Returns "" when the text does not
/// look like language (CID/embedded fonts, scans), and the caller then shows
/// the rasterised page instead.
fn pdf_text(path: &str) -> String {
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) > 64 * 1024 * 1024 { return String::new(); }
    let Ok(bytes) = fs::read(path) else { return String::new() };
    let mut content: Vec<u8> = Vec::new();
    let mut rest: &[u8] = &bytes;
    while let Some(i) = find_bytes(rest, b"stream") {
        let mut p = i + 6;
        while p < rest.len() && (rest[p] == b'\r' || rest[p] == b'\n') { p += 1; }
        let Some(e) = find_bytes(&rest[p..], b"endstream") else { break };
        let raw = &rest[p..p + e];
        match inflate(raw) {
            Some(dec) => content.extend_from_slice(&dec),
            None => content.extend_from_slice(raw),
        }
        rest = &rest[p + e + 9..];
        if content.len() > 4 * 1024 * 1024 { break; }
    }
    if content.is_empty() { return String::new(); }

    let mut out = String::new();
    let mut i = 0;
    while i < content.len() {
        match content[i] {
            b'(' => {
                let mut j = i + 1;
                let mut buf: Vec<u8> = Vec::new();
                while j < content.len() && content[j] != b')' {
                    if content[j] == b'\\' && j + 1 < content.len() {
                        j += 1;
                        match content[j] {
                            b'n' | b'r' | b't' => buf.push(b' '),
                            b'0'..=b'7' => {
                                let mut v = 0u32;
                                let mut k = 0;
                                while k < 3 && j < content.len() && (b'0'..=b'7').contains(&content[j]) {
                                    v = v * 8 + (content[j] - b'0') as u32;
                                    j += 1;
                                    k += 1;
                                }
                                buf.push(v as u8);
                                continue;
                            }
                            c => buf.push(c),
                        }
                        j += 1;
                        continue;
                    }
                    buf.push(content[j]);
                    j += 1;
                }
                let s = String::from_utf8_lossy(&buf);
                if !s.is_empty() {
                    if !out.is_empty() && !out.ends_with(' ') { out.push(' '); }
                    out.push_str(&s);
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }

    // sanity: PDF text with CID fonts comes out as noise, so only keep runs of
    // characters that look like language
    let total = out.chars().count();
    if total < 40 { return String::new(); }
    let sane = out.chars().filter(|c| {
        c.is_ascii_graphic() || *c == ' ' || ('\u{4e00}'..='\u{9fff}').contains(c)
            || ('\u{3000}'..='\u{303f}').contains(c)
    }).count();
    if sane * 100 / total < 85 { return String::new(); }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() { return None; }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    if flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    out.clear();
    if flate2::read::DeflateDecoder::new(data).read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Some(out);
    }
    None
}

/// Turn a PDF text layer into blocks: first line as the title, the rest as one
/// re-flowed paragraph (our page renderer does its own wrapping).
fn pdf_blocks(path: &str) -> Vec<Block> {
    let text = pdf_text(path);
    if text.is_empty() { return Vec::new(); }
    // title: the first sentence, or the first clause if it runs long
    let mut cut = 0usize;
    for (i, c) in text.char_indices() {
        if i > 72 { break; }
        if matches!(c, '.' | '?' | '!' | ':' | '：' | '。') && i >= 6 {
            cut = i + c.len_utf8();
            break;
        }
        if i > 46 && c == ' ' {
            cut = i;
            break;
        }
    }
    if cut == 0 { cut = text.len().min(56); }
    if !text.is_char_boundary(cut) { cut = text.len().min(56); }
    let title = text[..cut].trim().to_string();
    let rest = text[cut..].trim().to_string();
    let mut blocks = vec![Block::Title(title)];
    if !rest.is_empty() {
        blocks.push(Block::Para(rest));
    }
    blocks
}

/// Extract the readable content of a file, or an empty list when we cannot.
fn extract_blocks(path: &str) -> Vec<Block> {
    let ext = FsPath::new(path).extension().unwrap_or_default().to_string_lossy().to_uppercase();
    match ext.as_str() {
        "DOCX" | "DOCM" | "DOTX" => zip_entry(path, &["word/document.xml"])
            .map(|b| docx_blocks(&String::from_utf8_lossy(&b)))
            .unwrap_or_default(),
        "PPTX" | "PPTM" | "POTX" => pptx_blocks(path),
        "XLSX" | "XLSM" | "XLTX" => xlsx_blocks(path).unwrap_or_default(),
        "PDF" => pdf_blocks(path),
        "MD" | "MARKDOWN" => text_head(path, 400)
            .map(|lines| md_blocks(&lines))
            .unwrap_or_default(),
        "CSV" => text_head(path, 13).map(|lines| {
            let rows: Vec<Vec<String>> = lines.iter()
                .map(|l| l.split(',').map(|c| c.trim().to_string()).take(6).collect())
                .collect();
            vec![Block::Table(rows)]
        }).unwrap_or_default(),
        _ if TEXT_EXTS.contains(&ext.as_str()) => text_head(path, 400)
            .map(|lines| vec![Block::Para(lines.join("\n"))])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Cached extraction: a card repaints on hover/zoom and the container is
/// re-read only when the file changes size or mtime.
fn doc_blocks(path: &str) -> Vec<Block> {
    /// (file size, modified tick, parsed blocks)
    type BlockCache = HashMap<String, (u64, u64, Vec<Block>)>;
    static CACHE: Mutex<Option<BlockCache>> = Mutex::new(None);
    let stamp = fs::metadata(path).ok()
        .map(|m| {
            let t = m.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0);
            (m.len(), t)
        })
        .unwrap_or((0, 0));
    let mut guard = CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((len, t, blocks)) = cache.get(path) {
        if (*len, *t) == stamp { return blocks.clone(); }
    }
    let blocks = extract_blocks(path);
    if cache.len() > 64 { cache.clear(); }
    cache.insert(path.to_string(), (stamp.0, stamp.1, blocks.clone()));
    blocks
}

// ── Text rendering (GDI DrawTextW, alpha bitmap cache) ──────────────────────

struct TextBitmap { w: u32, h: u32, alpha: Vec<u8> }

// text, max width, pixel size, GDI weight, align (0 = left, 1 = centre)
type TextKey = (String, i32, i32, i32, u8);

const UI_FONT: &str = "Microsoft YaHei UI";

static TEXT_CACHE: Mutex<Option<HashMap<TextKey, TextBitmap>>> = Mutex::new(None);


/// Real advance width of a caption, measured with the same GDI font the card
/// draws with (the per-character estimate is only used for wrapping).
fn measure_text(text: &str, size: i32, weight: i32) -> f32 {
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let hdc = CreateCompatibleDC(screen);
        ReleaseDC(std::ptr::null_mut(), screen);
        if hdc.is_null() { return 0.0; }
        let face = wide("Microsoft YaHei UI");
        let font = CreateFontW(-size, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let old = SelectObject(hdc, font as HGDIOBJ);
        let wide_text: Vec<u16> = text.encode_utf16().collect();
        let mut extent = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(hdc, wide_text.as_ptr(), wide_text.len() as i32, &mut extent);
        SelectObject(hdc, old);
        DeleteObject(font as HGDIOBJ);
        DeleteDC(hdc);
        extent.cx as f32
    }
}

fn draw_text(fb: &mut Framebuffer, text: &str, x: i32, y: i32, max_w: i32, size: i32, weight: i32, color: (u8, u8, u8), align: u8) {
    let key = (text.to_string(), max_w, size, weight, align);
    let mut guard = TEXT_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    let tb = cache.entry(key)
        .or_insert_with(|| render_text_to_bitmap(text, max_w, size, weight, align));
    for row in 0..tb.h as i32 {
        for col in 0..tb.w as i32 {
            let a = tb.alpha[(row as usize) * (tb.w as usize) + col as usize];
            if a > 0 {
                fb.set_pixel(x + col, y + row, color.0, color.1, color.2, a);
            }
        }
    }
}

fn render_text_to_bitmap(text: &str, max_w: i32, size: i32, weight: i32, align: u8) -> TextBitmap {
    let w = max_w.max(1);
    let h = size + 10;
    let mut tb = TextBitmap { w: w as u32, h: h as u32, alpha: vec![0; (w * h) as usize] };
    unsafe {
        let hdc_screen = GetDC(std::ptr::null_mut());
        let hdc = CreateCompatibleDC(hdc_screen);
        ReleaseDC(std::ptr::null_mut(), hdc_screen);
        if hdc.is_null() { return tb; }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: std::mem::zeroed(),
        };
        let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(hdc);
            return tb;
        }
        let old_bmp = SelectObject(hdc, dib as *mut _);
        let font = CreateFontW(
            -size, 0, 0, 0, weight, 0, 0, 0,
            1, 0, 0, 5, 0, wide(UI_FONT).as_ptr(),
        );
        let old_font = SelectObject(hdc, font as *mut _);
        SetBkMode(hdc, 1); // TRANSPARENT
        SetTextColor(hdc, 0x00FF_FFFF);
        let wtext = wide(text);
        let mut rect = RECT { left: 0, top: 0, right: w, bottom: h };
        let mut flags = 0x20 | 0x8 | 0x4000; // DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS
        if align == 1 { flags |= 0x1; } // DT_CENTER
        DrawTextW(hdc, wtext.as_ptr(), -1, &mut rect, flags);
        let px = bits as *const u8;
        let stride = (w as usize) * 4;
        for row in 0..h as usize {
            for col in 0..w as usize {
                let i = row * stride + col * 4;
                let b = *px.add(i);
                let g = *px.add(i + 1);
                let r = *px.add(i + 2);
                tb.alpha[row * w as usize + col] = r.max(g).max(b);
            }
        }
        SelectObject(hdc, old_bmp);
        SelectObject(hdc, old_font);
        DeleteObject(font as *mut _);
        DeleteObject(dib as *mut _);
        DeleteDC(hdc);
    }
    tb
}

#[derive(Clone, Copy)]
struct Fab { center: (f32, f32), hover: bool }

/// A card painted once into its own premultiplied buffer, so a frame is a
/// handful of blits instead of millions of per-pixel blends. Shadows, the page
/// and the text are all baked in; only the position is applied at blit time.
/// `body` is the card rect inside the padded bitmap (x, y, w, h): pixels there
/// are composited over the frosted backdrop, pixels outside over the screen.
struct CardImage { w: i32, h: i32, px: Vec<u8>, body: (i32, i32, i32, i32) }

type CardCache = HashMap<String, CardImage>;

fn render_cards(fb: &mut Framebuffer, cache: &mut CardCache, cards: &[Card], hover_idx: Option<usize>, lifted_idx: Option<usize>, thumbs: &HashMap<String, Thumbnail>, anims: &[(usize, std::time::Instant)], fab_add: Fab, fab_gear: Fab, drag_active: bool, guides: &[Guide], edge_hint: Option<(usize, u8)>) {
    fb.clear();
    if drag_active {
        // alpha>=1 makes the whole window hit-testable, turning the entire
        // desktop into a drop zone while a file drag is in progress
        fb.fill_rect(0, 0, fb.width as i32, fb.height as i32, 255, 255, 255, 7);
        draw_text(fb, "松开即可生成卡片（拖到任意卡片或 + 上）", (fb.width as i32 - 600) / 2, (fb.height as i32 - 110) / 2, 600, (26.0 * ui_scale()) as i32, 600, (58, 66, 88), 1);
    }
    let guard = BACKDROP.lock().unwrap();
    let backdrop = guard.as_ref();
    let now = std::time::Instant::now();
    for (i, card) in cards.iter().enumerate() {
        if lifted_idx == Some(i) { continue; }
        let anim = anim_progress(anims, i, now);
        draw_card(fb, cache, card, hover_idx == Some(i), false, anim, thumbs.get(&card.path), backdrop);
    }
    // lifted card renders last so it floats above the others
    if let Some(i) = lifted_idx {
        if let Some(card) = cards.get(i) {
            draw_card(fb, cache, card, hover_idx == Some(i), true, anim_progress(anims, i, now), thumbs.get(&card.path), backdrop);
        }
    }
    drop(guard);
    draw_guides(fb, guides);
    if let Some((i, e)) = edge_hint {
        if let Some(card) = cards.get(i) {
            draw_resize_hint(fb, card, e, ui_scale());
        }
    }
    draw_fab(fb, fab_add, 0);
    draw_fab(fb, fab_gear, 1);
}

/// Blend a premultiplied card bitmap onto the framebuffer at (x, y). Inside the
/// card body the glass is composited over the *blurred* backdrop; outside it
/// (shadow, light spill) it blends over the live screen as usual.
fn blit_card(fb: &mut Framebuffer, img: &CardImage, x: i32, y: i32, backdrop: Option<&Backdrop>) {
    let fw = fb.width as i32;
    let fh = fb.height as i32;
    let sx0 = (-x).max(0);
    let sx1 = img.w.min(fw - x);
    let sy0 = (-y).max(0);
    let sy1 = img.h.min(fh - y);
    if sx1 <= sx0 || sy1 <= sy0 { return; }
    let stride = img.w as usize * 4;
    let fstride = fw as usize * 4;
    for sy in sy0..sy1 {
        let sbase = sy as usize * stride + sx0 as usize * 4;
        let dbase = (y + sy) as usize * fstride + (x + sx0) as usize * 4;
        let src = &img.px[sbase..sbase + (sx1 - sx0) as usize * 4];
        let dst = &mut fb.pixels[dbase..dbase + (sx1 - sx0) as usize * 4];
        let in_body_y = sy >= img.body.1 && sy < img.body.1 + img.body.3;
        for (i, (s, d)) in src.chunks(4).zip(dst.chunks_mut(4)).enumerate() {
            let a = s[3];
            if a == 255 {
                d.copy_from_slice(s);
                continue;
            }
            if a == 0 { continue; }
            let inv = 255 - a as u32;
            let gx = i as i32 + sx0;
            let over_backdrop = backdrop.is_some() && in_body_y
                && gx >= img.body.0 && gx < img.body.0 + img.body.2;
            if let (true, Some(bd)) = (over_backdrop, backdrop) {
                // frosted: the glass sits on the blurred wallpaper, already
                // graded (saturated + scattered) when the backdrop was built
                let (br, bg, bb) = bd.at(x + gx, y + sy);
                d[0] = (s[0] as u32 + (bb as u32 * inv) / 255) as u8;
                d[1] = (s[1] as u32 + (bg as u32 * inv) / 255) as u8;
                d[2] = (s[2] as u32 + (br as u32 * inv) / 255) as u8;
                d[3] = 255;
            } else {
                d[0] = (s[0] as u32 + (d[0] as u32 * inv) / 255) as u8;
                d[1] = (s[1] as u32 + (d[1] as u32 * inv) / 255) as u8;
                d[2] = (s[2] as u32 + (d[2] as u32 * inv) / 255) as u8;
                d[3] = (a as u32 + (d[3] as u32 * inv) / 255) as u8;
            }
        }
    }
}

/// Short fingerprint of a thumbnail, so a refreshed preview invalidates the
/// cached card without hashing the whole bitmap.
fn thumb_sig(t: Option<&Thumbnail>) -> u64 {
    match t {
        None => 0,
        Some(t) => {
            let n = t.data.len();
            let mut h: u64 = (t.width as u64) << 32 | t.height as u64;
            for k in 0..8 {
                let i = (n / 8) * k + 3.min(n.saturating_sub(1));
                if i < n { h = h.wrapping_mul(1099511628211).wrapping_add(t.data[i] as u64); }
            }
            h
        }
    }
}

fn draw_card(fb: &mut Framebuffer, cache: &mut CardCache, card: &Card, hovered: bool, lifted: bool, anim: f32, thumb: Option<&Thumbnail>, backdrop: Option<&Backdrop>) {
    if anim <= 0.02 { return; }
    let w = card.box_w();
    let h = card.box_h();
    let s = (w / CARD_BASE_W).clamp(0.45, 2.4);
    let lift = if lifted { 8.0 * s } else { 0.0 };
    let slide = (1.0 - anim) * 18.0 * s;
    let dx = card.visual_x();
    let dy = card.y - lift - slide;

    if anim < 0.999 {
        // transient entrance animation: paint straight onto the frame
        paint_card(fb, card, dx, dy, hovered, lifted, anim, thumb);
        return;
    }

    let pad = (16.0 * s).ceil() as i32;
    let key = format!(
        "{}|{}x{}|{}{}{}|{}", card.path, w as i32, h as i32,
        hovered as u8, lifted as u8, card.collapsed as u8, thumb_sig(thumb)
    );
    if let Some(img) = cache.get(&key) {
        blit_card(fb, img, dx as i32 - pad, dy as i32 - pad, backdrop);
        return;
    }
    let bw = w as i32 + pad * 2;
    let bh = h as i32 + pad * 2;
    let mut scratch = Framebuffer::new(bw.max(1) as u32, bh.max(1) as u32);
    paint_card(&mut scratch, card, pad as f32, pad as f32, hovered, lifted, 1.0, thumb);
    let img = CardImage {
        w: bw, h: bh, px: std::mem::take(&mut scratch.pixels),
        body: (pad, pad, w as i32, h as i32),
    };
    blit_card(fb, &img, dx as i32 - pad, dy as i32 - pad, backdrop);
    if cache.len() > 48 { cache.clear(); }
    cache.insert(key, img);
}

/// Ease-out entrance progress for card `i` (1.0 = fully shown).
fn anim_progress(anims: &[(usize, std::time::Instant)], i: usize, now: std::time::Instant) -> f32 {
    match anims.iter().find(|(ai, _)| *ai == i) {
        Some((_, t)) => {
            let e = now.duration_since(*t).as_secs_f32() / 0.35;
            if e >= 1.0 { 1.0 } else { 1.0 - (1.0 - e).powi(3) }
        }
        None => 1.0,
    }
}

const FAB_R: f32 = 22.0;

fn on_fab(center: (f32, f32), mx: f32, my: f32) -> bool {
    let dx = mx - center.0;
    let dy = my - center.1;
    let r = FAB_R * ui_scale();
    dx * dx + dy * dy <= r * r
}

/// Round floating action puck: `glyph` 0 = plus, 1 = settings gear.
fn draw_fab(fb: &mut Framebuffer, fab: Fab, glyph: u8) {
    let (cx, cy) = fab.center;
    let k = ui_scale();
    let r = FAB_R * k;
    let d = r * 2.0;
    // soft shadow, tighter when hovered
    fb.round_rect(cx - r - 3.0 * k, cy - r + 2.0 * k, d + 6.0 * k, d + 6.0 * k, r + 3.0 * k, (46, 58, 82), 14);
    fb.round_rect(cx - r - k, cy - r + k, d + 2.0 * k, d + 2.0 * k, r + k, (46, 58, 82), if fab.hover { 26 } else { 20 });
    let body = if fab.hover { 255 } else { 243 };
    fb.round_rect(cx - r, cy - r, d, d, r, (255, 255, 255), body);
    fb.round_rect_stroke(cx - r + 0.5, cy - r + 0.5, d - 1.0, d - 1.0, r, 1.0, (28, 38, 58), 26);
    let ink = if fab.hover { (46, 52, 64) } else { (96, 103, 116) };
    if glyph == 0 {
        let (arm, thick) = (8.5 * k, 1.25 * k);
        fb.round_rect(cx - arm, cy - thick, arm * 2.0, thick * 2.0, thick, ink, 255);
        fb.round_rect(cx - thick, cy - arm, thick * 2.0, arm * 2.0, thick, ink, 255);
    } else {
        draw_gear(fb, cx, cy, 8.4 * k, ink);
    }
}

fn draw_gear(fb: &mut Framebuffer, cx: f32, cy: f32, r: f32, ink: (u8, u8, u8)) {
    use std::f32::consts::PI;
    let t = r * 0.25;
    for i in 0..8 {
        let a = i as f32 * PI / 4.0;
        let tx = cx + a.cos() * (r + r * 0.12);
        let ty = cy + a.sin() * (r + r * 0.12);
        fb.round_rect(tx - t, ty - t, t * 2.0, t * 2.0, t * 0.45, ink, 255);
    }
    fb.ring(cx, cy, r + r * 0.15, r * 0.40, ink, 255);
}

const CARD_PAD: f32 = 15.0;
/// shell thumbnails are fetched at this size (a card page is ~570px wide max)
const THUMB_PX: u32 = 640;

/// Paint one card — shadows, tinted body, page and text — with its top-left at
/// (x, y). This is the expensive path; draw_card caches its result.
fn paint_card(fb: &mut Framebuffer, card: &Card, x: f32, y: f32, _hovered: bool, _lifted: bool, anim: f32, thumb: Option<&Thumbnail>) {
    if anim <= 0.02 { return; }
    let style = type_style(&card.file_ext());
    let w = card.width;
    let h = card.box_h();
    // interior scale: the card is designed at CARD_BASE_W physical pixels, so
    // zooming keeps proportions and a 150 %+ display scales the whole card.
    let s = (w / CARD_BASE_W).clamp(0.45, 2.4);

    let pad = CARD_PAD * s;
    let fade = |v: f32| (v * anim).clamp(0.0, 255.0) as u8;
    let fs = |v: f32| ((v * s).round() as i32).max(7);

    // No card panel at all: only the paper and the caption above it. The old
    // glass body, its shadows and its specular are gone on purpose — anything
    // behind the paper read as a frame around it.

    // a folded card is just its caption strip on a soft pill
    if card.collapsed {
        let wv = card.box_w();
        fb.round_rect(x, y + 1.5 * s, wv, h - 1.5 * s, 14.0 * s, (40, 52, 74), fade(22.0));
        fb.round_rect(x, y, wv, h, 14.0 * s, (255, 255, 255), fade(78.0));
        fb.round_rect(x, y, wv, h * 0.5, 14.0 * s, (255, 255, 255), fade(30.0));
    }

    // ── header: type badge + filename, hugging the top of the card ──
    let by = y + 6.0 * s;
    let badge = 30.0 * s;
    fb.round_rect(x + pad, by + 1.5 * s, badge, badge, 9.0 * s, style.accent, fade(50.0));
    fb.round_rect(x + pad, by, badge, badge, 9.0 * s, style.accent, fade(255.0));
    draw_text(fb, style.glyph, (x + pad) as i32, (by + 3.0 * s) as i32, badge as i32, fs(15.0), 700, (255, 255, 255), 1);
    // the caption sits on whatever the desktop shows, so it carries its own
    // contrast: white letters over a soft dark shadow
    let name = card.file_name();
    let tx = (x + pad + badge + 10.0 * s) as i32;
    let ty = (by + 3.5 * s) as i32;
    let btn = 30.0 * s;
    let wv = card.box_w();
    let tw = (wv - pad * 2.0 - badge - 10.0 * s - btn - 8.0 * s) as i32;
    for (dx, dy) in [(0, 2), (1, 1), (2, 0)] {
        draw_text(fb, &name, tx + (dx as f32 * s) as i32, ty + (dy as f32 * s) as i32, tw, fs(14.0), 600, (16, 20, 28), 0);
    }
    draw_text(fb, &name, tx, ty, tw, fs(14.0), 600, (255, 255, 255), 0);

    // fold / unfold button, always drawn so the target is obvious
    if wv > 140.0 * s {
        let bx = x + wv - pad - btn;
        fb.round_rect(bx, by + 1.0 * s, btn, btn, btn * 0.5, (40, 52, 74), fade(24.0));
        fb.round_rect(bx, by, btn, btn, btn * 0.5, (255, 255, 255), fade(165.0));
        draw_chevron(
            fb, bx + btn * 0.5, by + btn * 0.5,
            btn * 0.26, btn * 0.15,
            if card.collapsed { -1.0 } else { 1.0 },
            (48, 54, 66), fade(235.0),
        );
    }

    // a folded card stops here: caption only, no sheet underneath
    if card.collapsed {
        return;
    }

    // ── page: the sheet starts right under the caption ──
    let px = x + 15.0 * s;
    let pw = w - 30.0 * s;
    let area_y = y + 46.0 * s;
    let area_h = h - 46.0 * s - 15.0 * s;
    // a landscape thumbnail (photo, slide) gets its own landscape sheet, hung
    // straight under the caption rather than floating in the middle
    let (ph, py) = match thumb {
        Some(t) if t.width > 0 && t.width as f32 > t.height as f32 * 1.08 => {
            let fitted = (pw * t.height as f32 / t.width as f32).min(area_h);
            (fitted, area_y)
        }
        _ => (area_h, area_y),
    };
    let band = 6.0 * s;
    if pw > 24.0 * s && ph > 24.0 * s {
        // paper: the one solid surface on the card — no frame, but its edge is
        // glass, so it picks up the desktop behind it
        fb.round_rect(px + 2.0 * s, py + 3.0 * s, pw - 4.0 * s, ph + s, 12.0 * s, (40, 52, 74), fade(26.0));
        fb.glass_paper(px, py, pw, ph, 12.0 * s, band, anim);
        let blocks = doc_blocks(&card.path);
        let ext = card.file_ext();
        let is_slide = matches!(ext.as_str(), "PPTX" | "PPTM" | "POTX" | "PPT");
        // a real slide (background + positioned text boxes + pictures) beats any
        // text approximation, so try that first
        let mut drew_slide = false;
        if is_slide {
            if let Some(model) = slide_model(&card.path, 1) {
                draw_slide(fb, &model, px + band, py + band, pw - 2.0 * band, ph - 2.0 * band, 10.0 * s);
                drew_slide = true;
            }
        }
        let slide = is_slide && !drew_slide;
        if drew_slide {
            // the slide already painted the paper
        } else if slide && thumb.is_some() {
            let t = thumb.unwrap();
            draw_thumbnail(fb, t, (px + band) as i32, (py + band) as i32,
                           (pw - 2.0 * band) as i32, (ph - 2.0 * band) as i32);
        } else if !blocks.is_empty() {
            draw_doc_page(fb, &blocks, &style, px, py, pw, ph, s);
        } else if let Some(t) = thumb {
            // keep the glass edge clear of the image: the print sits inside it
            draw_thumbnail(fb, t, (px + band) as i32, (py + band) as i32,
                           (pw - 2.0 * band) as i32, (ph - 2.0 * band) as i32);
        } else {
            draw_placeholder_page(fb, card, &style, px, py, pw, ph, anim, s);
        }
    }
}

/// Small chevron; `dir` +1 points down (fold), -1 points up (unfold).
fn draw_chevron(
    fb: &mut Framebuffer, cx: f32, cy: f32, half_w: f32, half_h: f32,
    dir: f32, ink: (u8, u8, u8), alpha: u8,
) {
    let n = ((half_w * 2.0) as i32).max(6);
    let thick = (half_w * 0.28).max(1.6);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let lx = cx - half_w + t * half_w;
        let ly = cy - dir * half_h + t * 2.0 * dir * half_h;
        fb.round_rect(lx - thick * 0.5, ly - thick * 0.5, thick, thick, thick * 0.5, ink, alpha);
        let rx = cx + t * half_w;
        let ry = cy + dir * half_h - t * 2.0 * dir * half_h;
        fb.round_rect(rx - thick * 0.5, ry - thick * 0.5, thick, thick, thick * 0.5, ink, alpha);
    }
}

/// Screen rect of a card's fold button (matches what `paint_card` draws).
fn fold_button_rect(card: &Card) -> (f32, f32, f32, f32) {
    let s = card.scale();
    let btn = 30.0 * s;
    (card.visual_x() + card.box_w() - CARD_PAD * s - btn, card.y + 6.0 * s, btn, btn)
}

/// Which card's fold button the cursor is on (with a little slop for fat fingers).
fn fold_hit(cards: &[Card], mx: f32, my: f32, k: f32) -> Option<usize> {
    for (i, c) in cards.iter().enumerate().rev() {
        let (x, y, w, h) = fold_button_rect(c);
        let pad = 5.0 * k;
        if mx >= x - pad && mx <= x + w + pad && my >= y - pad && my <= y + h + pad {
            return Some(i);
        }
    }
    None
}

/// Read the head of a plain-text file so its card can show real content.
fn text_head(path: &str, max_lines: usize) -> Option<Vec<String>> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() > 256 * 1024 || bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .collect();
    if lines.is_empty() { None } else { Some(lines) }
}

/// Approximate advance width of one character, in pixels. Good enough for
/// wrapping (the real glyphs come from GDI), and avoids a round trip per char.
fn char_advance(ch: char, size: f32) -> f32 {
    let em = if ch.is_ascii() {
        match ch {
            ' ' => 0.30,
            'i' | 'l' | 'j' | 't' | 'f' | 'r' | 'I' | '.' | ',' | '\'' | '!' | '|' | ':' | ';' => 0.33,
            'm' | 'w' | 'M' | 'W' => 0.88,
            '0'..='9' => 0.58,
            _ => 0.55,
        }
    } else {
        let c = ch as u32;
        let wide = (0x2E80..=0x9FFF).contains(&c)
            || (0x3000..=0x303F).contains(&c)
            || (0x3040..=0x30FF).contains(&c)
            || (0xAC00..=0xD7AF).contains(&c)
            || (0xF900..=0xFAFF).contains(&c)
            || (0xFF00..=0xFF60).contains(&c);
        if wide { 1.0 } else { 0.58 }
    };
    em * size
}

/// Greedy word wrap that also breaks CJK anywhere (those runs have no spaces).
fn wrap_lines(text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0.0f32;
    for ch in text.chars() {
        if ch == '\n' {
            if !cur.trim().is_empty() { lines.push(std::mem::take(&mut cur)); }
            cur.clear();
            cur_w = 0.0;
            continue;
        }
        let w = char_advance(ch, size);
        if cur_w + w > max_w && !cur.is_empty() {
            if let Some(i) = cur.rfind(' ') {
                let head: String = cur[..i].to_string();
                let tail: String = cur[i + 1..].to_string();
                if !head.trim().is_empty() { lines.push(head); }
                cur_w = tail.chars().map(|c| char_advance(c, size)).sum();
                cur = tail;
            } else {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0.0;
            }
        }
        cur.push(ch);
        cur_w += w;
    }
    if !cur.trim().is_empty() { lines.push(cur); }
    lines
}

/// Render extracted document content as a page you can actually read: real
/// text at a fixed reading size, wrapped to the paper, clipped at its edge.
fn draw_doc_page(fb: &mut Framebuffer, blocks: &[Block], style: &TypeStyle, px: f32, py: f32, pw: f32, ph: f32, s: f32) {
    let ix = px + 15.0 * s;
    let iw = pw - 30.0 * s;
    let bottom = py + ph - 10.0 * s;
    let mut y = py + 15.0 * s;
    let ink = (40, 45, 56);
    let body_ink = (62, 68, 80);

    for block in blocks {
        if y >= bottom { break; }
        match block {
            Block::Title(text) | Block::Heading(text) => {
                let title = matches!(block, Block::Title(_));
                let size = if title { 15.5 } else { 12.5 } * s;
                let weight = if title { 700 } else { 600 };
                let color = if title { ink } else { (52, 58, 70) };
                // bold glyphs run wider than the estimate: leave it headroom so
                // lines break instead of being clipped
                for line in wrap_lines(text, size, iw * 0.93) {
                    if y + size * 1.3 > bottom { return; }
                    draw_text(fb, &line, ix as i32, y as i32, iw as i32, size as i32, weight, color, 0);
                    y += size * 1.45;
                }
                y += size * 0.35;
            }
            Block::Para(text) | Block::Bullet(text) => {
                let size = 10.5 * s;
                let bullet = matches!(block, Block::Bullet(_));
                let tx = if bullet { ix + 11.0 * s } else { ix };
                let tw = if bullet { iw - 11.0 * s } else { iw };
                let mut first = true;
                for line in wrap_lines(text, size, tw * 0.96) {
                    if y + size * 1.35 > bottom { return; }
                    if bullet && first {
                        fb.round_rect(ix + 2.0 * s, y + size * 0.42, 4.0 * s, 4.0 * s, 2.0 * s, style.accent, 210);
                    }
                    draw_text(fb, &line, tx as i32, y as i32, tw as i32, size as i32, 400, body_ink, 0);
                    y += size * 1.5;
                    first = false;
                }
                y += size * 0.45;
            }
            Block::Table(rows) => {
                let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).clamp(1, 6);
                let cw = iw / cols as f32;
                let rh = 19.0 * s;
                for (ri, row) in rows.iter().enumerate() {
                    if y + rh > bottom { return; }
                    if ri == 0 {
                        fb.round_rect(ix, y, iw, rh, 3.0 * s, style.accent, 36);
                    }
                    for c in 0..cols {
                        if let Some(cell) = row.get(c) {
                            if cell.is_empty() { continue; }
                            draw_text(
                                fb, cell,
                                (ix + c as f32 * cw + 5.0 * s) as i32, (y + 4.0 * s) as i32,
                                (cw - 9.0 * s) as i32, (9.5 * s) as i32,
                                if ri == 0 { 600 } else { 400 }, body_ink, 0,
                            );
                        }
                    }
                    y += rh;
                    fb.round_rect(ix, y - 0.5, iw, 1.0, 0.5, (0, 0, 0), 22);
                }
            }
        }
    }
}

fn draw_placeholder_page(fb: &mut Framebuffer, card: &Card, style: &TypeStyle, px: f32, py: f32, pw: f32, ph: f32, anim: f32, s: f32) {
    let fade = |v: f32| (v * anim).clamp(0.0, 255.0) as u8;
    let fs = |v: f32| ((v * s).round() as i32).max(7);
    let ink = (44, 50, 62);
    let body_ink = (72, 79, 92);
    let bar = (0, 0, 0);
    let ix = px + 16.0 * s;
    let iw = pw - 32.0 * s;
    let stem = FsPath::new(&card.path).file_stem().unwrap_or_default().to_string_lossy().to_string();
    draw_text(fb, &stem, ix as i32, (py + 16.0 * s) as i32, iw as i32, fs(13.0), 700, ink, 0);
    fb.round_rect(ix, py + 42.0 * s, iw, 1.0, 0.5, bar, fade(36.0));

    let ext = card.file_ext();
    match ext.as_str() {
        "XLSX" | "XLS" => {
            let top = py + 54.0 * s;
            let step = 22.0 * s;
            let rows = (((ph - 78.0 * s) / step).floor()).max(2.0) as i32;
            let cols = 4;
            let cw = iw / cols as f32;
            fb.round_rect(ix, top, iw, 20.0 * s, 3.0 * s, style.accent, fade(42.0));
            for c in 1..cols {
                fb.round_rect(ix + c as f32 * cw, top, 1.0, rows as f32 * step, 0.5, bar, fade(22.0));
            }
            for r in 1..=rows {
                fb.round_rect(ix, top + r as f32 * step, iw, 1.0, 0.5, bar, fade(22.0));
            }
        }
        "PPTX" | "PPT" => {
            let sh = ((ph - 96.0 * s).max(40.0 * s)) * 0.62;
            let sw = iw * 0.86;
            let sx = ix + (iw - sw) * 0.5;
            let sy = py + 58.0 * s;
            fb.round_rect(sx, sy, sw, sh, 8.0 * s, style.accent, fade(14.0));
            fb.round_rect_stroke(sx, sy, sw, sh, 8.0 * s, 1.0, style.accent, fade(85.0));
            fb.round_rect(sx + 20.0 * s, sy + sh * 0.30, sw * 0.44, 7.0 * s, 3.5 * s, style.accent, fade(115.0));
            for i in 0..3 {
                let wf = 0.34 - i as f32 * 0.05;
                fb.round_rect(sx + 20.0 * s, sy + sh * 0.30 + (18.0 + i as f32 * 12.0) * s,
                              sw * wf, 5.0 * s, 2.5 * s, bar, fade(40.0));
            }
        }
        _ => {
            // Only genuine text formats get a text preview: a PDF or an Office
            // package would otherwise show its raw container bytes as "content".
            const TEXT_EXT: &[&str] = &[
                "TXT", "MD", "MARKDOWN", "JSON", "CSV", "LOG", "INI", "TOML", "YAML", "YML",
                "XML", "HTML", "HTM", "CSS", "JS", "TS", "RS", "PY", "SH", "BAT", "PS1",
            ];
            let line_h = 15.0 * s;
            let room = (((ph - 62.0 * s) / line_h).floor()).max(0.0) as usize;
            let head = if TEXT_EXT.contains(&ext.as_str()) {
                text_head(&card.path, room.min(20))
            } else {
                None
            };
            match head {
                Some(lines) if !lines.is_empty() => {
                    let mut ly = py + 54.0 * s;
                    for line in lines {
                        let clean = line.trim_start_matches('#').trim();
                        if clean.is_empty() { ly += line_h * 0.5; continue; }
                        draw_text(fb, clean, ix as i32, ly as i32, iw as i32, fs(9.0), 400, body_ink, 0);
                        ly += line_h;
                    }
                }
                _ => {
                    let mut ly = py + 56.0 * s;
                    let mut i = 0;
                    while ly + 8.0 * s < py + ph - 16.0 * s {
                        let jitter = ((i * 37) % 41) as f32 / 100.0;
                        let wf = (0.62 + jitter * 0.34).min(0.96);
                        fb.round_rect(ix, ly, iw * wf, 5.0 * s, 2.5 * s, bar, fade(24.0));
                        ly += 15.0 * s;
                        i += 1;
                        if i % 5 == 0 { ly += 10.0 * s; }
                    }
                }
            }
        }
    }
}

/// Raise a card to the top of the stack (paint order == vec order).
fn bring_to_front(cards: &mut Vec<Card>, idx: usize) -> usize {
    if idx + 1 >= cards.len() { return idx; }
    let card = cards.remove(idx);
    cards.push(card);
    cards.len() - 1
}

fn card_at(cards: &[Card], mx: f32, my: f32) -> Option<usize> {
    cards.iter().enumerate().rev().find(|(_, c)| {
        mx >= c.visual_x() && mx <= c.visual_x() + c.box_w() && my >= c.y && my <= c.y + c.box_h()
    }).map(|(i, _)| i)
}

/// Which resize handles the cursor is on. bit 0 = right, bit 1 = bottom,
/// bit 2 = left, bit 3 = top. Zero means the card body itself (drag).
/// The grab ring reaches a little *outside* the card, because that is where
/// people aim when they want to stretch something.
fn resize_edges(card: &Card, mx: f32, my: f32, k: f32) -> u8 {
    if card.collapsed { return 0; }
    let outer = 10.0 * k; // how far outside the edge still counts
    let inner = 16.0 * k; // how far inside the edge still counts
    let dl = mx - card.x;
    let dr = card.x + card.width - mx;
    let dt = my - card.y;
    let db = card.y + card.box_h() - my;
    if dl < -outer || dr < -outer || dt < -outer || db < -outer { return 0; }
    let mut e = 0;
    if dr <= inner && dr >= -outer {
        e |= 1;
    } else if dl <= inner && dl >= -outer {
        e |= 4;
    }
    if db <= inner && db >= -outer {
        e |= 2;
    } else if dt <= inner && dt >= -outer {
        e |= 8;
    }
    e
}

/// Expand dropped or selected paths: a folder contributes its first few files,
/// so dropping a folder does not produce a single useless card.
fn expand_paths(paths: &[String], per_dir: usize) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        let path = FsPath::new(p);
        if path.is_dir() {
            let mut taken = 0;
            if let Ok(rd) = fs::read_dir(path) {
                let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|f| f.is_file()).collect();
                entries.sort();
                for f in entries {
                    out.push(f.to_string_lossy().to_string());
                    taken += 1;
                    if taken >= per_dir { break; }
                }
            }
        } else if path.is_file() {
            out.push(p.clone());
        }
    }
    out
}

/// Files currently on the clipboard (CF_HDROP), if any.
fn clipboard_files() -> Vec<String> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return Vec::new(); }
        let mut out = Vec::new();
        let handle = GetClipboardData(CF_HDROP);
        if !handle.is_null() {
            let drop = handle as HDROP;
            let count = DragQueryFileW(drop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0) as usize;
                if len == 0 { continue; }
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, buf.as_mut_ptr(), (len + 1) as u32) as usize;
                if got > 0 { out.push(String::from_utf16_lossy(&buf[..got])); }
            }
        }
        CloseClipboard();
        out
    }
}

/// Ask the focused Explorer window to copy its selection (Ctrl+C), so the
/// hotkey can pick it up from the clipboard.
fn copy_explorer_selection() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() { return false; }
        let mut cls = [0u16; 128];
        let n = GetClassNameW(fg, cls.as_mut_ptr(), cls.len() as i32);
        if n <= 0 { return false; }
        let name = String::from_utf16_lossy(&cls[..n as usize]);
        if name != "CabinetWClass" && name != "ExploreWClass" { return false; }
        keybd_event(VK_CONTROL as u8, 0, 0, 0);
        keybd_event(b'C', 0, 0, 0);
        keybd_event(b'C', 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
        true
    }
}

/// Add a batch of files as cards: centred on `at` when given (a drop point),
/// otherwise near `fallback`.
fn add_paths(
    paths: Vec<String>, at: Option<(f32, f32)>,
    config: &mut AppConfig, thumbs: &mut HashMap<String, Thumbnail>,
    areas: &[WorkArea], anims: &mut Vec<(usize, std::time::Instant)>,
    fallback: (f32, f32),
) {
    let gap = 12.0 * ui_scale();
    for p in expand_paths(&paths, 8) {
        // the same file never gets a second card: surface the one already there
        let key = p.replace('/', "\\").to_lowercase();
        if let Some(existing) = config.cards.iter()
            .position(|c| c.path.replace('/', "\\").to_lowercase() == key)
        {
            let idx = bring_to_front(&mut config.cards, existing);
            config.cards[idx].collapsed = false;
            clamp_card(&mut config.cards[idx], areas);
            anims.push((idx, std::time::Instant::now()));
            continue;
        }
        let (want_x, want_y) = at.unwrap_or(fallback);
        let area = area_for(areas, want_x, want_y, 120.0, 120.0);
        let scale = area.map(|a| a.scale()).unwrap_or_else(ui_scale);
        let mut card = Card::with_scale(p.clone(), want_x - 150.0, want_y - 218.0, scale);
        card.dpi = area.map(|a| a.dpi).unwrap_or(96.0);
        let (px, py) = place_free(&config.cards, areas, card.x, card.y, card.box_w(), card.box_h(), gap);
        card.x = px;
        card.y = py;
        clamp_card(&mut card, areas);
        config.cards.push(card);
        if let Some(t) = file_preview(&p, THUMB_PX) {
            if let Some(card) = config.cards.last_mut() {
                fit_card_to_preview(card, Some(&t));
            }
            thumbs.insert(p, t);
        }
        anims.push((config.cards.len() - 1, std::time::Instant::now()));
    }
    if config.rail {
        pack_column(&mut config.cards);
    }
}

/// The topmost card whose grab ring contains the point (the point may sit just
/// outside that card).
fn resize_at(cards: &[Card], mx: f32, my: f32, k: f32) -> Option<(usize, u8)> {
    for (i, c) in cards.iter().enumerate().rev() {
        let e = resize_edges(c, mx, my, k);
        if e != 0 { return Some((i, e)); }
    }
    None
}

fn resize_cursor(e: u8) -> CursorIcon {
    let right = e & 1 != 0;
    let bottom = e & 2 != 0;
    let left = e & 4 != 0;
    let top = e & 8 != 0;
    match (right || left, top || bottom) {
        (true, true) => {
            if (left && top) || (right && bottom) { CursorIcon::NwseResize } else { CursorIcon::NeswResize }
        }
        (true, false) => CursorIcon::EwResize,
        (false, true) => CursorIcon::NsResize,
        _ => CursorIcon::Pointer,
    }
}

// ── Work areas, clamping and snapping ───────────────────────────────────────

/// One monitor's usable desktop (its work area minus the taskbar), already
/// inset by the screen margin so cards keep a little air from every edge.
#[derive(Clone, Copy)]
struct WorkArea {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// effective DPI of that monitor (96 = 100 %)
    dpi: f32,
    primary: bool,
}

impl WorkArea {
    fn scale(&self) -> f32 {
        (self.dpi / 96.0).clamp(0.5, 4.0)
    }
}

/// A line drawn while dragging to show what the card is lining up with.
struct Guide { vertical: bool, pos: f32, from: f32, to: f32 }

const WORK_MARGIN: f32 = 16.0;
/// pull towards a screen edge, × DPI scale
const SNAP_EDGE: f32 = 24.0;
/// pull towards another card's edge, × DPI scale
const SNAP_PEER: f32 = 8.0;

/// Effective DPI of the monitor under a point (falls back to the system DPI).
fn monitor_dpi(_left: i32, _top: i32, x: i32, y: i32) -> Option<u32> {
    unsafe {
        let pt = POINT { x, y };
        let hmon = MonitorFromPoint(pt, 2); // MONITOR_DEFAULTTONEAREST
        if hmon.is_null() {
            return None;
        }
        let mut dx = 0u32;
        let mut dy = 0u32;
        let ok = windows::Win32::UI::HiDpi::GetDpiForMonitor(
            windows::Win32::Graphics::Gdi::HMONITOR(hmon as *mut core::ffi::c_void),
            windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
            &mut dx,
            &mut dy,
        );
        if ok.is_ok() && dx > 0 {
            Some(dx)
        } else {
            None
        }
    }
}

fn work_areas(margin: f32) -> Vec<WorkArea> {
    unsafe extern "system" fn collect(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data as *mut Vec<WorkArea>);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut info) != 0 {
            let r = info.rcWork;
            list.push(WorkArea {
                x: r.left as f32,
                y: r.top as f32,
                w: (r.right - r.left) as f32,
                h: (r.bottom - r.top) as f32,
                dpi: 96.0,
                primary: info.dwFlags & 1 != 0, // MONITORINFOF_PRIMARY
            });
        }
        1
    }
    let mut raw: Vec<WorkArea> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut raw as *mut _ as LPARAM,
        );
    }
    // margin and card sizes follow each monitor's own scale, so a 100 % screen
    // next to a 4K 150 % one still looks consistent
    raw.into_iter()
        .map(|a| {
            let dpi = monitor_dpi(
                a.x as i32,
                a.y as i32,
                ((a.w * 0.5) as i32) + a.x as i32,
                ((a.h * 0.5) as i32) + a.y as i32,
            )
            .unwrap_or(96) as f32;
            let m = margin * (dpi / 96.0);
            WorkArea {
                x: a.x + m,
                y: a.y + m,
                w: (a.w - m * 2.0).max(120.0),
                h: (a.h - m * 2.0).max(120.0),
                dpi,
                primary: a.primary,
            }
        })
        .collect()
}

/// Work areas translated into window coordinates (the overlay starts at the
/// top-left of the virtual screen, cards are stored in that same space).
fn screen_areas(margin: f32, vx: i32, vy: i32) -> Vec<WorkArea> {
    let mut areas = work_areas(margin);
    for a in &mut areas {
        a.x -= vx as f32;
        a.y -= vy as f32;
    }
    areas
}

/// The monitor a card belongs to: most overlap wins, nearest centre otherwise.
fn area_for(areas: &[WorkArea], x: f32, y: f32, w: f32, h: f32) -> Option<WorkArea> {
    let mut best: Option<WorkArea> = None;
    let mut best_overlap = 0.0f32;
    for a in areas {
        let ox = (a.x + a.w).min(x + w) - x.max(a.x);
        let oy = (a.y + a.h).min(y + h) - y.max(a.y);
        let overlap = ox.max(0.0) * oy.max(0.0);
        if overlap > best_overlap {
            best_overlap = overlap;
            best = Some(*a);
        }
    }
    if best.is_some() { return best; }
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let mut best_d = f32::MAX;
    for a in areas {
        let dx = a.x + a.w * 0.5 - cx;
        let dy = a.y + a.h * 0.5 - cy;
        let d = dx * dx + dy * dy;
        if d < best_d {
            best_d = d;
            best = Some(*a);
        }
    }
    best
}

/// One-shot tidy: stack every card into a single column on the right edge,
/// keeping their top-to-bottom order. No docking state, no re-flow afterwards —
/// drag them apart again and nothing fights back.
fn tidy_column(cards: &mut [Card], areas: &[WorkArea]) -> bool {
    if cards.is_empty() { return false; }
    let u = ui_scale();
    let first = &cards[0];
    let Some(area) = area_for(areas, first.x, first.y, first.box_w(), first.box_h()) else { return false };

    // reading order: top to bottom, left to right as a tie break
    let mut order: Vec<usize> = (0..cards.len()).collect();
    order.sort_by(|&a, &b| {
        cards[a].y.total_cmp(&cards[b].y).then(cards[a].x.total_cmp(&cards[b].x))
    });

    // a shared width keeps the column neat; the median respects what is there
    let mut widths: Vec<f32> = cards.iter().map(|c| c.width).collect();
    widths.sort_by(f32::total_cmp);
    let median = widths[widths.len() / 2];
    let width = median.clamp(300.0 * u, (area.w * 0.42).min(900.0 * u));

    let margin = 24.0 * u;
    let available = (area.h - margin * 2.0).max(160.0 * u);
    let base: Vec<f32> = order.iter().map(|&i| cards[i].height.max(160.0 * u)).collect();
    let slots = base.len() as f32;
    let gap = 16.0 * u;
    let total = |k: f32, min_h: f32| -> f32 {
        base.iter().map(|h| (h * k).max(min_h)).sum::<f32>() + gap * (slots - 1.0)
    };
    if total(1.0, 0.0) <= available {
        // everything fits as it is
        place_column(cards, &order, &base, width, area, margin, gap);
        return false;
    }
    // squeeze down to a still-readable size before giving up
    let min_h = 220.0 * u;
    if total(1.0, min_h) > available {
        let (mut lo, mut hi) = (0.05f32, 1.0f32);
        for _ in 0..28 {
            let mid = (lo + hi) * 0.5;
            if total(mid, min_h) > available { hi = mid; } else { lo = mid; }
        }
        let heights: Vec<f32> = base.iter().map(|h| (h * lo).max(min_h)).collect();
        if total(lo, min_h) <= available + 1.0 {
            place_column(cards, &order, &heights, width, area, margin, gap);
            return false;
        }
    }
    // still too tall: keep natural heights and scroll instead of squashing
    let heights: Vec<f32> = base.iter().map(|h| h.min(available * 0.62)).collect();
    place_column(cards, &order, &heights, width, area, margin, gap);
    true
}

/// Gap used between the cards of a tidy column.
const COLUMN_GAP: f32 = 16.0;

/// Re-pack a tidy column: a folded card only takes its strip height, so the
/// cards below slide up and the pills sit tightly together. Re-running it with
/// everything unfolded puts every card back where it was, because the layout is
/// fully determined by the order, the heights and the gap.
fn pack_column(cards: &mut [Card]) -> bool {
    let Some((x0, w0)) = cards.first().map(|c| (c.x, c.width)) else { return false };
    let mut members: Vec<usize> = cards.iter().enumerate()
        .filter(|(_, c)| (c.x - x0).abs() < 1.5 && (c.width - w0).abs() < 1.5)
        .map(|(i, _)| i)
        .collect();
    if members.len() < 2 { return false; }
    members.sort_by(|&a, &b| cards[a].y.total_cmp(&cards[b].y));
    let top = cards[members[0]].y;
    let gap = COLUMN_GAP * ui_scale();
    let mut y = top;
    let mut changed = false;
    for i in members {
        let h = cards[i].box_h();
        if (cards[i].y - y).abs() > 0.5 { changed = true; }
        cards[i].y = y;
        y += h + gap;
    }
    changed
}

/// Vertical span of the rail: (top, bottom).
fn rail_span(cards: &[Card]) -> (f32, f32) {
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for c in cards {
        top = top.min(c.y);
        bottom = bottom.max(c.y + c.height);
    }
    (top, bottom)
}

/// Where the rail's work area is: from the first card that sits on screen.
fn rail_area(cards: &[Card], areas: &[WorkArea]) -> Option<WorkArea> {
    cards.first().and_then(|c| area_for(areas, c.x, c.y, c.box_w(), c.box_h()))
}

/// Scroll the rail by `dy` pixels (positive = further down).
fn scroll_rail(cards: &mut [Card], area: WorkArea, u: f32, dy: f32) -> bool {
    let (top, bottom) = rail_span(cards);
    let margin = 24.0 * u;
    let view = (area.h - margin * 2.0).max(1.0);
    let max_scroll = (bottom - top) - view;
    if max_scroll <= 1.0 { return false; }
    let current = (area.y + margin) - top;
    let next = (current + dy).clamp(0.0, max_scroll);
    let delta = next - current;
    if delta.abs() < 0.5 { return false; }
    for c in cards.iter_mut() {
        c.y -= delta;
    }
    true
}

/// How far the rail can scroll (0 when it fits).
fn rail_max_scroll(cards: &[Card], areas: &[WorkArea], u: f32) -> f32 {
    let Some(area) = rail_area(cards, areas) else { return 0.0 };
    let (top, bottom) = rail_span(cards);
    let view = (area.h - 48.0 * u).max(1.0);
    ((bottom - top) - view).max(0.0)
}

/// Jump the rail to an absolute scroll offset.
fn set_rail_scroll(cards: &mut [Card], area: WorkArea, u: f32, target: f32) -> bool {
    let (top, bottom) = rail_span(cards);
    let margin = 24.0 * u;
    let view = (area.h - margin * 2.0).max(1.0);
    let max_scroll = (bottom - top) - view;
    if max_scroll <= 1.0 { return false; }
    let current = (area.y + margin) - top;
    let next = target.clamp(0.0, max_scroll);
    let delta = next - current;
    if delta.abs() < 0.5 { return false; }
    for c in cards.iter_mut() {
        c.y -= delta;
    }
    true
}

/// Geometry of the rail scrollbar: (track x, track y, track h, thumb y, thumb h).
fn rail_thumb(cards: &[Card], areas: &[WorkArea], u: f32) -> Option<(f32, f32, f32, f32, f32)> {
    let area = rail_area(cards, areas)?;
    let (top, bottom) = rail_span(cards);
    let margin = 24.0 * u;
    let view = (area.h - margin * 2.0).max(1.0);
    let content = bottom - top;
    if content <= view + 1.0 { return None; }
    let track_y = area.y + margin;
    let track_h = view;
    let thumb_h = (track_h * view / content).clamp(48.0 * u, track_h);
    let max_scroll = content - view;
    let scroll = ((area.y + margin) - top).clamp(0.0, max_scroll);
    let thumb_y = track_y + (track_h - thumb_h) * (scroll / max_scroll);
    let track_x = area.x + area.w - 12.0 * u;
    Some((track_x, track_y, track_h, thumb_y, thumb_h))
}

/// Draw the rail's scrollbar (track + thumb), only while the rail overflows.
fn draw_rail(fb: &mut Framebuffer, cards: &[Card], areas: &[WorkArea], u: f32) {
    let Some((x, ty, th, thumb_y, thumb_h)) = rail_thumb(cards, areas, u) else { return };
    let w = 6.0 * u;
    fb.round_rect(x - 1.0 * u, ty, w + 2.0 * u, th, w * 0.5, (40, 52, 74), 26);
    fb.round_rect(x, ty, w, th, w * 0.5, (255, 255, 255), 70);
    fb.round_rect(x, thumb_y, w, thumb_h, w * 0.5, (255, 255, 255), 215);
}

/// Is the point on the rail scrollbar gutter?
fn on_rail_gutter(cards: &[Card], areas: &[WorkArea], mx: f32, my: f32, u: f32) -> bool {
    let Some(area) = rail_area(cards, areas) else { return false };
    let left = area.x + area.w - 30.0 * u;
    mx >= left && mx <= area.x + area.w && my >= area.y && my <= area.y + area.h
}

#[allow(clippy::too_many_arguments)]
fn place_column(
    cards: &mut [Card], order: &[usize], heights: &[f32], width: f32,
    area: WorkArea, margin: f32, gap: f32,
) {
    let x = area.x + area.w - margin - width;
    let mut y = area.y + margin;
    for (slot, &i) in order.iter().enumerate() {
        let h = heights[slot];
        let c = &mut cards[i];
        c.width = width;
        c.height = h;
        c.collapsed = false;
        c.auto_fit = false; // the column is an explicit layout: keep it
        c.x = x;
        c.y = y;
        y += h + gap;
    }
}

/// Ring-by-ring search for a spot near (x, y) that overlaps no existing card
/// and stays inside a work area. Falls back to the requested spot.
fn place_free(cards: &[Card], areas: &[WorkArea], x: f32, y: f32, w: f32, h: f32, gap: f32) -> (f32, f32) {
    let free = |cx: f32, cy: f32| -> bool {
        if let Some(a) = area_for(areas, cx, cy, w, h) {
            if cx < a.x || cy < a.y || cx + w > a.x + a.w || cy + h > a.y + a.h {
                return false;
            }
        }
        !cards.iter().any(|c| {
            cx < c.x + c.width + gap
                && cx + w + gap > c.x
                && cy < c.y + c.box_h() + gap
                && cy + h + gap > c.y
        })
    };
    if free(x, y) { return (x, y); }
    let step = 28.0;
    for ring in 1..40 {
        let r = ring as f32 * step;
        let samples = 8 * ring;
        for i in 0..samples {
            let t = i as f32 / samples as f32 * std::f32::consts::TAU;
            // wider than tall: cards are usually wider than their offset
            let cx = x + t.cos() * r * 1.5;
            let cy = y + t.sin() * r;
            if free(cx, cy) { return (cx, cy); }
        }
    }
    (x, y)
}

/// Keep a card fully inside one monitor's work area.
fn clamp_card(card: &mut Card, areas: &[WorkArea]) {
    let box_h = card.box_h();
    let Some(a) = area_for(areas, card.x, card.y, card.box_w(), box_h) else { return };
    if !card.collapsed {
        card.width = card.width.min(a.w);
    }
    if !card.collapsed {
        card.height = card.height.min(a.h);
    }
    let visible_h = card.box_h();
    // clamp the *visual* box, then map back to the stored x
    let shift = card.width - card.box_w();
    let vx = card.visual_x().clamp(a.x, a.x + a.w - card.box_w());
    card.x = vx + shift;
    card.y = card.y.clamp(a.y, a.y + a.h - visible_h);
}

/// Keep a card's apparent size the same when it lands on a monitor with a
/// different DPI: 150 % -> 100 % shrinks it by a third, and back again.
fn rescale_card_for_monitor(card: &mut Card, areas: &[WorkArea]) {
    let Some(area) = area_for(areas, card.x, card.y, card.box_w(), card.box_h()) else { return };
    let want = area.dpi;
    if card.dpi <= 0.0 {
        card.dpi = want; // first time we see this card: adopt, do not resize
        return;
    }
    let ratio = want / card.dpi;
    if (ratio - 1.0).abs() < 0.02 {
        card.dpi = want;
        return;
    }
    let before = (card.x, card.y, card.width, card.height);
    card.width = (card.width * ratio).clamp(200.0, 2600.0);
    card.height = (card.height * ratio).clamp(160.0, 2600.0);
    card.dpi = want;
    clamp_card(card, areas);
    if std::env::var("BD_DPI_DEBUG").is_ok() {
        eprintln!("rescale {:?} -> {:?} (ratio {ratio:.3}, area dpi {want})",
                  before, (card.x, card.y, card.width, card.height));
    }
}

/// Snap a dragged card: screen edges first (they define the desktop), then the
/// edges and centres of the other cards. Returns the adjusted position and the
/// guides that should be drawn for it.
fn snap_position(
    cards: &[Card], skip: usize, areas: &[WorkArea],
    x: f32, y: f32, w: f32, h: f32, k: f32,
) -> (f32, f32, Vec<Guide>) {
    let mut guides = Vec::new();
    let mut nx = x;
    let mut ny = y;
    let edge_t = SNAP_EDGE * k;
    let peer_t = SNAP_PEER * k;

    // ── screen edges ──
    let mut on_x = false;
    let mut on_y = false;
    if let Some(a) = area_for(areas, x, y, w, h) {
        for (target, line) in [(a.x, a.x), (a.x + a.w - w, a.x + a.w)] {
            if (target - x).abs() <= edge_t {
                nx = target;
                guides.push(Guide { vertical: true, pos: line, from: a.y - 8.0 * k, to: a.y + a.h + 8.0 * k });
                on_x = true;
                break;
            }
        }
        for (target, line) in [(a.y, a.y), (a.y + a.h - h, a.y + a.h)] {
            if (target - y).abs() <= edge_t {
                ny = target;
                guides.push(Guide { vertical: false, pos: line, from: a.x - 8.0 * k, to: a.x + a.w + 8.0 * k });
                on_y = true;
                break;
            }
        }
    }

    // ── other cards: align left/right/centre ──
    if !on_x {
        let mut best: Option<(f32, f32, f32)> = None; // (|delta|, target, line)
        for (i, c) in cards.iter().enumerate() {
            if i == skip { continue; }
            for (target, line) in [
                (c.x, c.x),
                (c.x + c.width - w, c.x + c.width),
                (c.x + (c.width - w) * 0.5, c.x + c.width * 0.5),
            ] {
                let d = (target - nx).abs();
                if d <= peer_t && best.map(|(bd, _, _)| d < bd).unwrap_or(true) {
                    best = Some((d, target, line));
                }
            }
        }
        if let Some((_, target, line)) = best {
            nx = target;
            let from = cards.iter().enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c.y)
                .fold(ny, f32::min) - 12.0 * k;
            let to = cards.iter().enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c.y + c.box_h())
                .fold(ny + h, f32::max) + 12.0 * k;
            guides.push(Guide { vertical: true, pos: line, from, to });
        }
    }
    if !on_y {
        let mut best: Option<(f32, f32, f32)> = None;
        for (i, c) in cards.iter().enumerate() {
            if i == skip { continue; }
            for (target, line) in [
                (c.y, c.y),
                (c.y + c.box_h() - h, c.y + c.box_h()),
                (c.y + (c.box_h() - h) * 0.5, c.y + c.box_h() * 0.5),
            ] {
                let d = (target - ny).abs();
                if d <= peer_t && best.map(|(bd, _, _)| d < bd).unwrap_or(true) {
                    best = Some((d, target, line));
                }
            }
        }
        if let Some((_, target, line)) = best {
            ny = target;
            let from = cards.iter().enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c.x)
                .fold(nx, f32::min) - 12.0 * k;
            let to = cards.iter().enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c.x + c.width)
                .fold(nx + w, f32::max) + 12.0 * k;
            guides.push(Guide { vertical: false, pos: line, from, to });
        }
    }
    (nx, ny, guides)
}

/// Snap one card edge to the work-area border, or to another card's border on
/// the same axis. `vertical` = x axis, `is_max` = the right / bottom edge.
fn snap_edge(
    cards: &[Card], skip: usize, area: Option<WorkArea>,
    edge: f32, vertical: bool, is_max: bool, k: f32, span: (f32, f32),
) -> (f32, Option<Guide>) {
    let edge_t = SNAP_EDGE * k;
    let peer_t = SNAP_PEER * k;
    if let Some(a) = area {
        let target = if vertical {
            if is_max { a.x + a.w } else { a.x }
        } else if is_max {
            a.y + a.h
        } else {
            a.y
        };
        if (target - edge).abs() <= edge_t {
            let g = if vertical {
                Guide { vertical: true, pos: target, from: a.y - 8.0 * k, to: a.y + a.h + 8.0 * k }
            } else {
                Guide { vertical: false, pos: target, from: a.x - 8.0 * k, to: a.x + a.w + 8.0 * k }
            };
            return (target, Some(g));
        }
    }
    let mut best: Option<(f32, f32)> = None;
    for (i, c) in cards.iter().enumerate() {
        if i == skip { continue; }
        let (lo, hi) = if vertical { (c.x, c.x + c.width) } else { (c.y, c.y + c.box_h()) };
        for target in [lo, hi] {
            let d = (target - edge).abs();
            if d <= peer_t && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, target));
            }
        }
    }
    match best {
        Some((_, target)) => {
            let g = Guide { vertical, pos: target, from: span.0 - 12.0 * k, to: span.1 + 12.0 * k };
            (target, Some(g))
        }
        None => (edge, None),
    }
}
fn draw_guides(fb: &mut Framebuffer, guides: &[Guide]) {
    for g in guides {
        if g.vertical {
            let h = (g.to - g.from).max(1.0);
            fb.fill_rect(g.pos as i32, g.from as i32, 1, h as i32, 86, 148, 255, 170);
        } else {
            let w = (g.to - g.from).max(1.0);
            fb.fill_rect(g.from as i32, g.pos as i32, w as i32, 1, 86, 148, 255, 170);
        }
    }
}

/// Hint that a card edge is grabbable: a soft light bar along that one edge.
fn draw_resize_hint(fb: &mut Framebuffer, card: &Card, edges: u8, k: f32) {
    let (x, y, w, h) = (card.x, card.y, card.width, card.box_h());
    let t = 2.5 * k;
    let inset = 8.0 * k;
    let white = (255, 255, 255);
    if edges & 1 != 0 { fb.round_rect(x + w - t, y + inset, t, h - inset * 2.0, t * 0.5, white, 150); }
    if edges & 4 != 0 { fb.round_rect(x, y + inset, t, h - inset * 2.0, t * 0.5, white, 150); }
    if edges & 2 != 0 { fb.round_rect(x + inset, y + h - t, w - inset * 2.0, t, t * 0.5, white, 150); }
    if edges & 8 != 0 { fb.round_rect(x + inset, y, w - inset * 2.0, t, t * 0.5, white, 150); }
}

fn get_hwnd(window: &Window) -> HWND {
    use winit::raw_window_handle::HasWindowHandle;
    match window.window_handle().ok().map(|h| h.as_raw()) {
        Some(RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    }
}

// Swallow WM_DPICHANGED so winit never resizes our fullscreen overlay
// (its DPI handler clamps the window to a single monitor's logical size).
static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
/// Set when the monitor layout changes, so the work areas get rebuilt.
static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);
/// Set by the global add-card hotkey (WM_HOTKEY).
static HOTKEY_ADD: AtomicBool = AtomicBool::new(false);
/// Fold request: 1 = fold everything, -1 = unfold everything, 2 = toggle.
static FOLD_REQUEST: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// Set by the tidy-into-a-column hotkey.
static HOTKEY_TIDY: AtomicBool = AtomicBool::new(false);

extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == 0x02E0 {
        return 0; // WM_DPICHANGED
    }
    if msg == 0x007E {
        // WM_DISPLAYCHANGE: monitors came or went
        DISPLAY_CHANGED.store(true, Ordering::Relaxed);
    }
    if msg == 0x0312 {
        // WM_HOTKEY: id 1 adds cards, id 2 folds/unfolds everything
        match wp {
            1 => HOTKEY_ADD.store(true, Ordering::Relaxed),
            2 => FOLD_REQUEST.store(2, Ordering::Relaxed),
            3 => HOTKEY_TIDY.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
    unsafe {
        CallWindowProcW(
            std::mem::transmute::<isize, winapi::um::winuser::WNDPROC>(
                ORIGINAL_WNDPROC.load(Ordering::Relaxed)),
            hwnd, msg, wp, lp,
        )
    }
}

fn install_dpi_guard(hwnd: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, main_wndproc as *const () as usize as isize);
        ORIGINAL_WNDPROC.store(old, Ordering::Relaxed);
    }
}

fn show_menu(window: &Window, x: i32, y: i32, items: &[&str]) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() { return None; }
        let mut id = 0u32;
        for item in items {
            if *item == "-" {
                AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
            } else {
                id += 1;
                AppendMenuW(menu, 0x00000000, id as usize, wide(item).as_ptr());
            }
        }
        let hwnd = get_hwnd(window);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | 0x0002, x, y, 0, hwnd, std::ptr::null());
        DestroyMenu(menu);
        if cmd == 0 { None } else { Some(cmd as u32) }
    }
}

/// Settings menu: needs a check mark on the z-order entry, so it is built here
/// rather than through the generic `show_menu`.
fn show_gear_menu(window: &Window, x: i32, y: i32, topmost: bool) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() { return None; }
        AppendMenuW(menu, 0, 1, wide("刷新全部缩略图").as_ptr());
        AppendMenuW(menu, 0, 2, wide("重新采样桌面背景").as_ptr());
        AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
        // MF_CHECKED = 0x8
        AppendMenuW(menu, if topmost { 0x00000008 } else { 0 }, 3, wide("置顶显示").as_ptr());
        AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
        AppendMenuW(menu, 0, 4, wide("全部折叠  Ctrl+Alt+H").as_ptr());
        AppendMenuW(menu, 0, 5, wide("全部展开  Ctrl+Alt+H").as_ptr());
        AppendMenuW(menu, 0, 6, wide("整理成右侧一列  Ctrl+Alt+T").as_ptr());
        AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
        AppendMenuW(menu, 0, 7, wide("打开配置文件夹").as_ptr());
        AppendMenuW(menu, 0, 8, wide("退出").as_ptr());
        let hwnd = get_hwnd(window);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | 0x0002, x, y, 0, hwnd, std::ptr::null());
        DestroyMenu(menu);
        if cmd == 0 { None } else { Some(cmd as u32) }
    }
}

fn open_file(path: &str) { let _ = open::that(path); }

// ── Tray icon ────────────────────────────────────────────────────────────────

const TRAY_CALLBACK: u32 = 0x8001; // WM_APP + 1
static REFRESH_REQUESTED: AtomicBool = AtomicBool::new(false);
static PENDING_ADDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

extern "system" fn tray_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        // WM_LBUTTONUP / WM_RBUTTONUP on the tray icon
        if msg == TRAY_CALLBACK && (lp as u32 == 0x0202 || lp as u32 == 0x0205) {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(hwnd);
            let menu = CreatePopupMenu();
            AppendMenuW(menu, 0x00000000, 1, wide("添加卡片...").as_ptr());
            AppendMenuW(menu, 0x00000000, 2, wide("刷新全部缩略图").as_ptr());
            AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
            AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
            AppendMenuW(menu, 0x00000000, 3, wide("全部折叠").as_ptr());
            AppendMenuW(menu, 0x00000000, 4, wide("全部展开").as_ptr());
            AppendMenuW(menu, 0x00000800, 0, std::ptr::null());
            AppendMenuW(menu, 0x00000000, 5, wide("关于 Better Desktop").as_ptr());
            AppendMenuW(menu, 0x00000000, 6, wide("退出 Better Desktop").as_ptr());
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | 0x0002, pt.x, pt.y, 0, hwnd, std::ptr::null());
            DestroyMenu(menu);
            match cmd {
                1 => *PENDING_ADDS.lock().unwrap() = vec![String::new()],
                2 => REFRESH_REQUESTED.store(true, Ordering::Relaxed),
                3 => FOLD_REQUEST.store(1, Ordering::Relaxed),
                4 => FOLD_REQUEST.store(-1, Ordering::Relaxed),
                5 => info_box(&format!(
                    "better desktop {}\n\nWindows 桌面卡片\nhttps://github.com/Lukeskywalker704/Better-Desktop",
                    env!("CARGO_PKG_VERSION")
                )),
                6 => { PostMessageW(MAIN_HWND.load(Ordering::Relaxed) as HWND, WM_CLOSE, 0, 0); }
                _ => {}
            }
            return 0;
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// Which modifier set actually got registered for each hotkey id, so the UI
/// can tell the user when their favourite combo was taken by another app.
static HOTKEY_LABELS: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());

fn hotkey_label(mods: u32, vk: u32) -> String {
    let mut s = String::new();
    if mods & 0x2 != 0 { s.push_str("Ctrl+"); }
    if mods & 0x1 != 0 { s.push_str("Alt+"); }
    if mods & 0x4 != 0 { s.push_str("Shift+"); }
    s.push((vk as u8) as char);
    s
}

/// Register `id` with the first combo that is still free.
fn register_hotkey(hwnd: HWND, id: i32, vk: u32, combos: &[u32]) -> Option<String> {
    for extra in combos {
        let mods = (MOD_CONTROL | MOD_ALT | (*extra as isize) | MOD_NOREPEAT) as u32;
        let ok = unsafe { RegisterHotKey(hwnd, id, mods, vk) };
        if ok != 0 {
            let label = hotkey_label(mods, vk);
            if let Ok(mut list) = HOTKEY_LABELS.lock() {
                list.push((id as u32, label.clone()));
            }
            return Some(label);
        }
    }
    None
}

/// One-off balloon on the tray icon (used when a hotkey cannot be registered).
fn tray_balloon(title: &str, text: &str) {
    unsafe {
        let hwnd = TRAY_HWND.load(Ordering::Relaxed) as HWND;
        if hwnd.is_null() {
            return;
        }
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = 0x10; // NIF_INFO
        nid.dwInfoFlags = 0x1; // NIIF_INFO
        let t = wide(title);
        let b = wide(text);
        let tn = t.len().min(nid.szInfoTitle.len() - 1);
        nid.szInfoTitle[..tn].copy_from_slice(&t[..tn]);
        let bn = b.len().min(nid.szInfo.len() - 1);
        nid.szInfo[..bn].copy_from_slice(&b[..bn]);
        Shell_NotifyIconW(0x0000_0001, &mut nid); // NIM_MODIFY
    }
}

fn setup_tray(main_hwnd: HWND) {
    unsafe {
        MAIN_HWND.store(main_hwnd as isize, Ordering::Relaxed);
        let class_name = wide("BetterDesktopTrayWnd");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(tray_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(std::ptr::null()),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&wc);
        let msg_hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("BDTray").as_ptr(),
            0, 0, 0, 0, 0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null_mut(),
        );
        if msg_hwnd.is_null() { return; }
        TRAY_HWND.store(msg_hwnd as isize, Ordering::Relaxed);
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = msg_hwnd;
        nid.uID = 1;
        nid.uFlags = 0x7; // NIF_MESSAGE | NIF_ICON | NIF_TIP
        nid.uCallbackMessage = TRAY_CALLBACK;
        // use the icon embedded in the exe (resource id 1) instead of the
        // generic application icon
        let module = GetModuleHandleW(std::ptr::null());
        let icon = LoadImageW(
            module,
            MAKEINTRESOURCEW(1),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        );
        nid.hIcon = if icon.is_null() {
            LoadIconW(std::ptr::null_mut(), IDI_APPLICATION as _)
        } else {
            icon as HICON
        };
        let tip = wide("Better Desktop");
        let n = tip.len().min(128);
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        Shell_NotifyIconW(NIM_ADD, &mut nid);
    }
}

fn remove_tray() {
    unsafe {
        let hwnd = TRAY_HWND.load(Ordering::Relaxed) as HWND;
        if hwnd.is_null() { return; }
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &mut nid);
    }
}

fn pick_files_dialog() -> Vec<String> {
    unsafe {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{
            FileOpenDialog, FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, IFileOpenDialog,
            SIGDN_FILESYSPATH,
        };
        use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
        use windows::core::w;

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let _ = dialog.SetOptions(FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM);
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("常用文档"),
                pszSpec: w!("*.pdf;*.docx;*.doc;*.xlsx;*.xls;*.pptx;*.ppt;*.txt;*.md;*.png;*.jpg;*.jpeg"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("所有文件"),
                pszSpec: w!("*.*"),
            },
        ];
        let _ = dialog.SetFileTypes(&filters);
        if dialog.Show(windows::Win32::Foundation::HWND::default()).is_err() {
            return Vec::new(); // user cancelled
        }
        let results = match dialog.GetResults() {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let count = results.GetCount().unwrap_or(0);
        let mut paths = Vec::new();
        for i in 0..count {
            if let Ok(item) = results.GetItemAt(i) {
                if let Ok(pw) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                    if let Ok(s) = pw.to_string() {
                        paths.push(s);
                    }
                    CoTaskMemFree(Some(pw.as_ptr() as *const _));
                }
            }
        }
        paths
    }
}

// ── Layered-window presenter ─────────────────────────────────────────────────

/// Submits the premultiplied framebuffer via UpdateLayeredWindow; per-pixel
/// alpha makes fully transparent regions click-through for free.
struct Presenter {
    hdc: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    w: u32,
    h: u32,
}

impl Presenter {
    fn new(w: u32, h: u32) -> Self {
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let hdc = CreateCompatibleDC(screen);
            ReleaseDC(std::ptr::null_mut(), screen);
            let (dib, bits) = create_dib(hdc, w, h);
            SelectObject(hdc, dib as *mut _);
            Self { hdc, dib, bits, w, h }
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        unsafe {
            if self.w == w && self.h == h { return; }
            let (dib, bits) = create_dib(self.hdc, w, h);
            SelectObject(self.hdc, dib as *mut _);
            DeleteObject(self.dib as *mut _);
            self.dib = dib;
            self.bits = bits;
            self.w = w;
            self.h = h;
        }
    }

    fn present(&mut self, hwnd: HWND, pos: (i32, i32), fb: &Framebuffer) {
        if fb.width == 0 || fb.height == 0 { return; }
        if std::env::var("BD_DUMP").is_ok() {
            dump_bmp(config_dir().join("frame.bmp"), fb);
        }
        self.resize(fb.width, fb.height);
        unsafe {
            std::ptr::copy_nonoverlapping(fb.pixels.as_ptr(), self.bits, fb.pixels.len());
            let mut dst = POINT { x: pos.0, y: pos.1 };
            let mut src = POINT { x: 0, y: 0 };
            let mut size = SIZE { cx: fb.width as i32, cy: fb.height as i32 };
            let mut blend = BLENDFUNCTION {
                BlendOp: 1, // AC_SRC_OVER
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1, // AC_SRC_ALPHA
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                std::ptr::null_mut(),
                &mut dst,
                &mut size,
                self.hdc,
                &mut src,
                0,
                &mut blend,
                2, // ULW_ALPHA
            );
        }
    }
}

unsafe fn create_dib(hdc: HDC, w: u32, h: u32) -> (HBITMAP, *mut u8) {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: std::mem::zeroed(),
    };
    let mut bits: *mut winapi::ctypes::c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    (dib, bits as *mut u8)
}

fn install_layered_style(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        // TOOLWINDOW keeps it out of the taskbar and out of Alt+Tab
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            ex | ((WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize),
        );
    }
}

/// Pin the overlay above every window, or drop it back into the normal band so
/// ordinary windows cover it (desktop-card behaviour).
fn apply_zorder(hwnd: HWND, topmost: bool) {
    unsafe {
        let after = if topmost { HWND_TOPMOST } else { HWND_BOTTOM };
        SetWindowPos(hwnd, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

// Debug helper: write the framebuffer to a viewable BMP.
fn dump_bmp(path: PathBuf, fb: &Framebuffer) {
    use std::io::Write;
    let data_size = fb.width * fb.height * 4;
    let ih = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: fb.width as i32,
        biHeight: -(fb.height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    if let Ok(mut f) = fs::File::create(path) {
        let _ = f.write_all(&0x4D42u16.to_le_bytes());
        let _ = f.write_all(&(14u32 + 40 + data_size).to_le_bytes());
        let _ = f.write_all(&0u16.to_le_bytes());
        let _ = f.write_all(&0u16.to_le_bytes());
        let _ = f.write_all(&54u32.to_le_bytes());
        let _ = f.write_all(unsafe {
            std::slice::from_raw_parts(std::ptr::from_ref(&ih) as *const u8, 40)
        });
        let _ = f.write_all(&fb.pixels);
    }
}

fn main() {
    // one overlay per desktop: two instances would fight over the config file
    if std::env::var("BD_PROBE").is_err() && std::env::var("BD_BENCH").is_err() && std::env::var("BD_SNAP").is_err()
        && std::env::var("BD_BACKDROP").is_err() && std::env::var("BD_DPI").is_err()
    {
        unsafe {
            let name = wide("BetterDesktop.SingleInstance.v1");
            let handle = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
            if !handle.is_null() && GetLastError() == 183 {
                // ERROR_ALREADY_EXISTS: another overlay already owns the desktop
                return;
            }
        }
    }

    // Become per-monitor-DPI-aware BEFORE any coordinate query. winit also
    // sets this, but only at EventLoop::new() — too late for GetSystemMetrics,
    // which would then return virtualized (single-monitor) values.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    // Physical-pixel UI scale for this display; cards and controls follow it so
    // the layout keeps the same apparent size at 125 % / 150 % / 200 %.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }.max(96);
    UI_SCALE_MILLI.store(((dpi as f32 / 96.0) * 1000.0) as u32, Ordering::Relaxed);

    migrate_legacy_config();
    // version / help: a released exe is a GUI program, so answer in a dialog
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V" || a == "-v") {
        info_box(&format!(
            "better desktop {}\n\nhttps://github.com/Lukeskywalker704/Better-Desktop",
            env!("CARGO_PKG_VERSION")
        ));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        info_box("better desktop — Windows 桌面卡片\n\n\
单击纸面：打开文件\n\
拖动卡片 / 页眉：移动（Alt 关闭吸附）\n\
拖四边或四角：独立调整宽高\n\
滚轮：缩放；右侧滑动槽内滚轮：滚动整列\n\
双击页眉 / 右上箭头：折叠 ↔ 展开\n\
右键卡片：打开 / 折叠 / 刷新 / 移除\n\
拖文件到卡片或 + 上：生成卡片\n\n\
Ctrl+Alt+D  添加卡片（剪贴板 / 资源管理器选中）\n\
Ctrl+Alt+H  全部折叠 ↔ 全部展开\n\
Ctrl+Alt+T  整理成右侧一列\n\n\
配置：%APPDATA%\\BetterDesktop");
        return;
    }

    let mut config = AppConfig::load();

    // virtual screen = union of all monitors, in physical pixels
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    };

    // keep every card inside some monitor's work area (a monitor may be gone
    // since the last run, and cards must never hide under the taskbar)
    let mut areas = screen_areas(WORK_MARGIN * ui_scale(), vx, vy);
    if std::env::var("BD_DPI").is_ok() {
        for (i, a) in areas.iter().enumerate() {
            eprintln!(
                "monitor {i}: work area {}x{} at ({}, {})  dpi={}  scale={:.2}  primary={}",
                a.w as i32, a.h as i32, a.x as i32, a.y as i32, a.dpi as i32, a.scale(), a.primary
            );
        }
        eprintln!("system dpi scale: {:.2}", ui_scale());
        std::process::exit(0);
    }
    for c in &mut config.cards {
        rescale_card_for_monitor(c, &areas);
    }
    if config.rail {
        // a column always re-packs on load, so a layout saved while folded can
        // never come back with the cards overlapping
        if pack_column(&mut config.cards) {
            config.save();
        }
    } else {
        for c in &mut config.cards {
            clamp_card(c, &areas);
        }
    }

    let event_loop = EventLoop::new().unwrap();
    let window = WindowBuilder::new()
        .with_title("Better Desktop")
        .with_position(winit::dpi::PhysicalPosition::new(vx, vy))
        .with_inner_size(winit::dpi::PhysicalSize::new(vw as u32, vh as u32))
        .with_decorations(false)
        .with_transparent(true)
        .build(&event_loop)
        .expect("Failed to create window");

    // swallow WM_DPICHANGED before any move/resize so winit can't clamp us
    install_dpi_guard(get_hwnd(&window));
    // layered window: per-pixel alpha => transparent regions are click-through
    install_layered_style(get_hwnd(&window));
    apply_zorder(get_hwnd(&window), config.topmost);
    // Ctrl+Alt+D: Explorer selection / clipboard files -> cards
    // Ctrl+Alt+H: fold everything / unfold everything
    setup_tray(get_hwnd(&window));
    let hwnd = get_hwnd(&window);
    let add = register_hotkey(hwnd, 1, 0x44, &[0]);
    let fold = register_hotkey(hwnd, 2, 0x48, &[0, 0x4]); // Ctrl+Alt+H, then +Shift
    let tidy = register_hotkey(hwnd, 3, 0x54, &[0]);
    if fold.is_none() {
        tray_balloon(
            "better desktop",
            "Ctrl+Alt+H 被别的程序占用了，全部折叠请用托盘或齿轮菜单",
        );
    } else if fold.as_deref() != Some("Ctrl+Alt+H") {
        let label = fold.clone().unwrap_or_default();
        tray_balloon("better desktop", &format!("全部折叠改用了 {label}（Ctrl+Alt+H 被占用）"));
    }
    let _ = (add, tidy);

    unsafe {
        SetWindowPos(get_hwnd(&window), std::ptr::null_mut(), vx, vy, vw, vh,
            SWP_NOZORDER | SWP_NOACTIVATE);
    }

    let (w, h) = (vw as u32, vh as u32);
    let mut fb = Framebuffer::new(w, h);
    let mut presenter = Presenter::new(w, h);
    // snapshot the desktop once for the glass to refract (our layered window is
    // excluded from the grab, so this is what sits behind the overlay)
    *BACKDROP.lock().unwrap() = capture_backdrop(vx, vy, vw, vh);

    // scroll probe: BD_SCROLL=<px> moves the rail before the frame is dumped
    if let Ok(v) = std::env::var("BD_SCROLL") {
        if let (Ok(px), Some(area)) = (v.parse::<f32>(), rail_area(&config.cards, &areas)) {
            scroll_rail(&mut config.cards, area, ui_scale(), px);
        }
    }

    // snap probe: BD_SNAP="x,y,w,h" prints the snapped position for card 0
    if let Ok(spec) = std::env::var("BD_SNAP") {
        let v: Vec<f32> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if v.len() >= 4 {
            let k = ui_scale();
            let probe_areas = work_areas(WORK_MARGIN * k);
            println!("areas: {:?}", probe_areas.iter().map(|a| (a.x, a.y, a.w, a.h)).collect::<Vec<_>>());
            let (nx, ny, g) = snap_position(&config.cards, 0, &probe_areas, v[0], v[1], v[2], v[3], k);
            println!("snap ({}, {}) -> ({}, {}) guides={}", v[0], v[1], nx, ny, g.len());
            for gg in &g {
                println!("  {} pos={} from={} to={}", if gg.vertical { "V" } else { "H" }, gg.pos, gg.from, gg.to);
            }
        }
        std::process::exit(0);
    }

    // backdrop probe: BD_BACKDROP=1 reports which source the glass sampled
    if std::env::var("BD_BACKDROP").is_ok() {
        match capture_backdrop(vx, vy, vw, vh) {
            Some(bd) => {
                let n = (bd.w * bd.h) as usize;
                let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
                for i in 0..n {
                    b += bd.px[i * 4] as u64;
                    g += bd.px[i * 4 + 1] as u64;
                    r += bd.px[i * 4 + 2] as u64;
                }
                println!(
                    "backdrop {}x{} mean rgb=({},{},{})",
                    bd.w, bd.h, r / n as u64, g / n as u64, b / n as u64
                );
            }
            None => println!("backdrop: none"),
        }
        std::process::exit(0);
    }

    // content probe: BD_PROBE=<path> prints what the preview would show, then exits
    if let Ok(path) = std::env::var("BD_PROBE") {
        for one in path.split(';').filter(|p| !p.is_empty()) {
            let t0 = std::time::Instant::now();
            let blocks = extract_blocks(one);
            println!("== {one} ({} blocks, {:?})", blocks.len(), t0.elapsed());
            for block in blocks.iter().take(40) {
                match block {
                    Block::Table(rows) => println!("TABLE {} rows: {:?}", rows.len(), rows.first()),
                    other => println!("{other:?}"),
                }
            }
        }
        std::process::exit(0);
    }

    // perf probe: BD_BENCH=<frames> renders that many frames headlessly and exits
    if let Ok(n) = std::env::var("BD_BENCH") {
        let n: u32 = n.parse().unwrap_or(20).max(1);
        let mut bench_thumbs: HashMap<String, Thumbnail> = HashMap::new();
        generate_thumbnails(&config.cards, &mut bench_thumbs);
        let add_fab = Fab { center: (w as f32 * 0.5 - 32.0, h as f32 - 118.0), hover: false };
        let gear_fab = Fab { center: (w as f32 * 0.5 + 32.0, h as f32 - 118.0), hover: false };
        let mut bench_cache: CardCache = CardCache::new();
        let t_cold = std::time::Instant::now();
        render_cards(&mut fb, &mut bench_cache, &config.cards, None, None, &bench_thumbs, &[], add_fab, gear_fab, false, &[], None);
        let cold = t_cold.elapsed().as_secs_f64() * 1000.0;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            render_cards(&mut fb, &mut bench_cache, &config.cards, None, None, &bench_thumbs, &[], add_fab, gear_fab, false, &[], None);
        }
        eprintln!(
            "bench: {:.2} ms/frame, cold first frame {:.1} ms, {} cards",
            t0.elapsed().as_secs_f64() * 1000.0 / n as f64,
            cold,
            config.cards.len()
        );
        std::process::exit(0);
    }
    let mut press_idx: Option<usize> = None;
    let mut press_pos = (0.0f32, 0.0f32);
    let mut is_dragging = false;
    let mut drag_off = (0.0f32, 0.0f32);
    // dragging an edge: (card, edges), plus where the drag started from
    let mut resizing: Option<(usize, u8)> = None;
    // press position, then the card box at the time the handle was grabbed
    let mut resize_from = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    // card + edges under the cursor when it is on a grab ring
    let mut edge_hint: Option<(usize, u8)> = None;
    // double-click on a caption folds the card; clicking a folded one unfolds it
    let mut last_header_click: Option<(usize, std::time::Instant)> = None;
    let mut suppress_open = std::time::Instant::now();
    // dragging the rail's scrollbar thumb: grab offset inside the thumb
    let mut rail_drag: Option<f32> = None;
    // hold Alt to place a card by hand without any snapping
    let mut alt_down = false;
    let mut hover_idx: Option<usize> = None;
    let mut needs_save = false;
    let mut cursor_pos = (0.0f32, 0.0f32);
    let mut thumbs: HashMap<String, Thumbnail> = HashMap::new();
    // cards born recently, animating in (index, birth instant)
    let mut anims: Vec<(usize, std::time::Instant)> = Vec::new();
    let k = ui_scale();
    // the two pucks sit at the bottom of the *primary* monitor: centring them on
    // the virtual screen would put them on the seam between two displays
    let primary = areas.iter().find(|a| a.primary).copied().or_else(|| areas.first().copied());
    let (fab_x, fab_bottom) = primary
        .map(|a| ((a.x + a.w * 0.5) - vx as f32, (a.y + a.h) - vy as f32))
        .unwrap_or((vw as f32 * 0.5, vh as f32));
    let fab_y = fab_bottom - 96.0 * k;
    let add_btn_center = (fab_x - 32.0 * k, fab_y);
    let gear_btn_center = (fab_x + 32.0 * k, fab_y);
    let mut add_btn_hover = false;
    let mut gear_btn_hover = false;
    let mut press_on_add = false;
    let mut press_on_gear = false;
    // a file drag hovered our content: whole desktop becomes a drop zone
    let mut drag_active = false;
    let mut card_cache: CardCache = CardCache::new();
    // alignment guides for the card being dragged / resized
    let mut guides: Vec<Guide> = Vec::new();

    event_loop.run(move |event, elwt| {
        match event {
            Event::NewEvents(StartCause::Init) => {
                generate_thumbnails(&config.cards, &mut thumbs);
                // landscape previews get a landscape card, unless the user has
                // already shaped the card by hand
                for card in &mut config.cards {
                    if !card.auto_fit || !doc_blocks(&card.path).is_empty() { continue; }
                    if fit_card_to_preview(card, thumbs.get(&card.path)) {
                        needs_save = true;
                    }
                }
                window.request_redraw();
            }
            Event::WindowEvent { event: we, .. } => match we {
                WindowEvent::RedrawRequested => {
                    let lifted = if is_dragging { press_idx } else { None };
                    render_cards(
                        &mut fb, &mut card_cache, &config.cards, hover_idx, lifted, &thumbs, &anims,
                        Fab { center: add_btn_center, hover: add_btn_hover },
                        Fab { center: gear_btn_center, hover: gear_btn_hover },
                        drag_active, &guides, edge_hint,
                    );
                    if config.rail {
                        draw_rail(&mut fb, &config.cards, &areas, ui_scale());
                    }
                    presenter.present(get_hwnd(&window), (vx, vy), &fb);
                }
                WindowEvent::ModifiersChanged(mods) => {
                    alt_down = mods.state().alt_key();
                }
                WindowEvent::HoveredFile(_) => {
                    if !drag_active {
                        drag_active = true;
                        window.request_redraw();
                    }
                }
                WindowEvent::HoveredFileCancelled => {
                    if drag_active {
                        drag_active = false;
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    let (mx, my) = cursor_pos;
                    if on_fab(add_btn_center, mx, my) {
                        press_on_add = true;
                    } else if on_fab(gear_btn_center, mx, my) {
                        press_on_gear = true;
                    } else if config.rail && rail_thumb(&config.cards, &areas, ui_scale())
                        .map(|(x, _ty, _th, thumb_y, thumb_h)| {
                            mx >= x - 6.0 * ui_scale() && mx <= x + 11.0 * ui_scale()
                                && my >= thumb_y && my <= thumb_y + thumb_h
                        }).unwrap_or(false)
                    {
                        let (.., thumb_y, _) = rail_thumb(&config.cards, &areas, ui_scale()).unwrap();
                        rail_drag = Some(my - thumb_y);
                        unsafe { SetCapture(get_hwnd(&window)); }
                    } else if let Some(idx) = fold_hit(&config.cards, mx, my, ui_scale()) {
                        // the chevron is the obvious target: one click folds or unfolds
                        let idx = bring_to_front(&mut config.cards, idx);
                        config.cards[idx].collapsed = !config.cards[idx].collapsed;
                        if !config.cards[idx].collapsed {
                            clamp_card(&mut config.cards[idx], &areas);
                        }
                        if config.rail { pack_column(&mut config.cards); }
                        needs_save = true;
                        window.request_redraw();
                    } else if let Some((idx, edges)) = resize_at(&config.cards, mx, my, ui_scale()) {
                        // grab ring wins over the body: the point may sit just
                        // outside the card, which is where the edge looks like it is
                        let idx = bring_to_front(&mut config.cards, idx);
                        resizing = Some((idx, edges));
                        resize_from = (mx, my, config.cards[idx].x, config.cards[idx].y,
                                       config.cards[idx].width, config.cards[idx].height);
                    } else if let Some(idx) = card_at(&config.cards, mx, my) {
                        // a double click on the caption folds the card
                        let idx = bring_to_front(&mut config.cards, idx);
                        let c = &config.cards[idx];
                        let header_bottom = c.y + COLLAPSED_H * c.scale() + 2.0 * c.scale();
                        let in_header = my <= header_bottom && !c.collapsed;
                        let now = std::time::Instant::now();
                        let double = matches!(
                            last_header_click,
                            Some((i, t)) if i == idx
                                && now.duration_since(t) < std::time::Duration::from_millis(420)
                        );
                        if in_header && double {
                            config.cards[idx].collapsed = true;
                            // fold in place: the strip keeps the card's top edge
                            last_header_click = None;
                            if config.rail { pack_column(&mut config.cards); }
                            needs_save = true;
                            window.request_redraw();
                        } else {
                            last_header_click = if in_header { Some((idx, now)) } else { None };
                            press_idx = Some(idx);
                            press_pos = (mx, my);
                            is_dragging = false;
                        }
                    }
                    // capture the mouse: a layered window only gets move events
                    // while the cursor is over an opaque pixel, so without this a
                    // drag dies the moment the pointer strays off the card
                    if resizing.is_some() || press_idx.is_some() || press_on_add || press_on_gear {
                        unsafe { SetCapture(get_hwnd(&window)); }
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                    rail_drag = None;
                    unsafe { ReleaseCapture(); }
                    if let Some((idx, _)) = resizing.take() {
                        // settle the committed box: snap it inside the work area
                        // and to whole pixels, then drop the guides
                        let mut card = config.cards[idx].clone();
                        clamp_card(&mut card, &areas);
                        card.x = card.x.round();
                        card.y = card.y.round();
                        card.width = card.width.round().max(200.0 * ui_scale());
                        card.height = card.height.round().max(160.0 * ui_scale());
                        // resizing by hand pins this monitor's scale for the card
                        if let Some(a) = area_for(&areas, card.x, card.y, card.width, card.height) {
                            card.dpi = a.dpi;
                        }
                        config.cards[idx] = card;
                        guides.clear();
                        config.rail = false;
                        needs_save = true;
                        window.request_redraw();
                    } else if press_on_add {
                        press_on_add = false;
                        *PENDING_ADDS.lock().unwrap() = vec![String::new()];
                    } else if press_on_gear {
                        press_on_gear = false;
                        let mut pt = POINT { x: 0, y: 0 };
                        unsafe { GetCursorPos(&mut pt); }
                        if let Some(cmd) = show_gear_menu(&window, pt.x - 110, pt.y - 130, config.topmost) {
                            match cmd {
                                1 => REFRESH_REQUESTED.store(true, Ordering::Relaxed),
                                2 => {
                                    *BACKDROP.lock().unwrap() = capture_backdrop(vx, vy, vw, vh);
                                    card_cache.clear();
                                    window.request_redraw();
                                }
                                3 => {
                                    // pin above every window, or sit back in the
                                    // desktop band under normal windows
                                    config.topmost = !config.topmost;
                                    apply_zorder(get_hwnd(&window), config.topmost);
                                    needs_save = true;
                                    window.request_redraw();
                                }
                                4 => {
                                    for c in &mut config.cards { c.collapsed = true; }
                                    if config.rail { pack_column(&mut config.cards); }
                                    needs_save = true;
                                    window.request_redraw();
                                }
                                5 => {
                                    for c in &mut config.cards {
                                        c.collapsed = false;
                                        clamp_card(c, &areas);
                                    }
                                    if config.rail { pack_column(&mut config.cards); }
                                    needs_save = true;
                                    window.request_redraw();
                                }
                                6 => {
                                    config.rail = tidy_column(&mut config.cards, &areas);
                                    guides.clear();
                                    needs_save = true;
                                    window.request_redraw();
                                }
                                7 => { let _ = open::that(config_dir()); }
                                8 => unsafe { PostMessageW(get_hwnd(&window), WM_CLOSE, 0, 0); },
                                _ => {}
                            }
                        }
                    } else if let Some(idx) = press_idx.take() {
                        if is_dragging {
                            // commit: snap inside the work area, quantise to whole
                            // pixels so repeated drags cannot drift
                            let mut card = config.cards[idx].clone();
                            clamp_card(&mut card, &areas);
                            rescale_card_for_monitor(&mut card, &areas);
                            card.x = card.x.round();
                            card.y = card.y.round();
                            config.cards[idx] = card;
                            guides.clear();
                            config.rail = false;
                            needs_save = true;
                            // the desktop behind may have changed while dragging;
                            // re-sample so the glass does not show a stale scene
                            *BACKDROP.lock().unwrap() = capture_backdrop(vx, vy, vw, vh);
                            card_cache.clear();
                        } else if config.cards[idx].collapsed {
                            // a click on a folded strip unfolds it
                            config.cards[idx].collapsed = false;
                            clamp_card(&mut config.cards[idx], &areas);
                            if config.rail { pack_column(&mut config.cards); }
                            last_header_click = None;
                            suppress_open = std::time::Instant::now() + std::time::Duration::from_millis(300);
                            needs_save = true;
                            window.request_redraw();
                        } else if std::time::Instant::now() >= suppress_open {
                            // only the sheet opens the file; the caption is a handle
                            let c = &config.cards[idx];
                            let header_bottom = c.y + COLLAPSED_H * c.scale();
                            if cursor_pos.1 > header_bottom {
                                open_file(&c.path);
                            }
                        }
                        is_dragging = false;
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                    let (mx, my) = cursor_pos;
                    if let Some(idx) = card_at(&config.cards, mx, my) {
                        let abs = window.outer_position().unwrap_or_default();
                        let sx = abs.x + mx as i32; let sy = abs.y + my as i32;
                        let fold_label = if config.cards[idx].collapsed { "展开卡片" } else { "折叠成标题条" };
                        let items: [&str; 5] = ["打开", fold_label, "刷新缩略图", "-", "移除卡片"];
                        if let Some(cmd) = show_menu(&window, sx, sy, &items) {
                            match cmd {
                                1 => open_file(&config.cards[idx].path),
                                2 => {
                                    config.cards[idx].collapsed = !config.cards[idx].collapsed;
                                    if !config.cards[idx].collapsed {
                                        clamp_card(&mut config.cards[idx], &areas);
                                    }
                                    if config.rail { pack_column(&mut config.cards); }
                                    needs_save = true;
                                }
                                3 => {
                                    let p = config.cards[idx].path.clone();
                                    thumbs.remove(&p);
                                    if let Some(t) = file_preview(&p, THUMB_PX) {
                                        let reshape = config.cards[idx].auto_fit
                                            && doc_blocks(&p).is_empty()
                                            && fit_card_to_preview(&mut config.cards[idx], Some(&t));
                                        thumbs.insert(p, t);
                                        if reshape { needs_save = true; }
                                    }
                                }
                                4 => { config.cards.remove(idx); needs_save = true; }
                                _ => {}
                            }
                            window.request_redraw();
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_pos = (position.x as f32, position.y as f32);
                    let (mx, my) = cursor_pos;
                    // resizing an edge: width and height move independently and
                    // the preview re-lays out from the new box
                    if let Some((idx, edges)) = resizing {
                        let k = ui_scale();
                        let (mx0, my0, x0, y0, w0, h0) = resize_from;
                        let dx = mx - mx0;
                        let dy = my - my0;
                        let (min_w, max_w) = (200.0 * k, 1600.0 * k);
                        let (min_h, max_h) = (160.0 * k, 2000.0 * k);
                        let mut x = x0;
                        let mut y = y0;
                        let mut w = w0;
                        let mut h = h0;
                        if edges & 1 != 0 { w = (w0 + dx).clamp(min_w, max_w); }
                        if edges & 4 != 0 {
                            let nw = (w0 - dx).clamp(min_w, max_w);
                            x = x0 + (w0 - nw);
                            w = nw;
                        }
                        if edges & 2 != 0 { h = (h0 + dy).clamp(min_h, max_h); }
                        if edges & 8 != 0 {
                            let nh = (h0 - dy).clamp(min_h, max_h);
                            y = y0 + (h0 - nh);
                            h = nh;
                        }
                        guides.clear();
                        if !alt_down {
                            let area = area_for(&areas, x, y, w, h);
                            if edges & 1 != 0 {
                                let (ne, g) = snap_edge(&config.cards, idx, area, x + w, true, true, k, (y, y + h));
                                if let Some(g) = g { guides.push(g); }
                                w = (ne - x).max(min_w);
                            }
                            if edges & 4 != 0 {
                                let (ne, g) = snap_edge(&config.cards, idx, area, x, true, false, k, (y, y + h));
                                if let Some(g) = g { guides.push(g); }
                                let right = x + w;
                                w = (right - ne).max(min_w);
                                x = right - w;
                            }
                            if edges & 2 != 0 {
                                let (ne, g) = snap_edge(&config.cards, idx, area, y + h, false, true, k, (x, x + w));
                                if let Some(g) = g { guides.push(g); }
                                h = (ne - y).max(min_h);
                            }
                            if edges & 8 != 0 {
                                let (ne, g) = snap_edge(&config.cards, idx, area, y, false, false, k, (x, x + w));
                                if let Some(g) = g { guides.push(g); }
                                let bottom = y + h;
                                h = (bottom - ne).max(min_h);
                                y = bottom - h;
                            }
                        }
                        let c = &mut config.cards[idx];
                        c.x = x;
                        c.y = y;
                        c.width = w;
                        c.height = h;
                        c.auto_fit = false;
                        clamp_card(c, &areas);
                        window.request_redraw();
                        return;
                    }
                    if let Some(idx) = press_idx {
                        if !is_dragging {
                            let ddx = mx - press_pos.0;
                            let ddy = my - press_pos.1;
                            if ddx * ddx + ddy * ddy > 25.0 {
                                is_dragging = true;
                                // grab from where the card is *drawn*: a folded
                                // pill is right-aligned, so its x sits far left
                                // of what the pointer sees
                                let c = &config.cards[idx];
                                drag_off = (mx - c.visual_x(), my - c.y);
                            }
                        }
                        if is_dragging {
                            let w = config.cards[idx].width;
                            let h = config.cards[idx].box_h();
                            let shift = w - config.cards[idx].box_w();
                            let want_x = mx - drag_off.0 - shift;
                            let want_y = my - drag_off.1;
                            let (nx, ny, g) = if alt_down {
                                (want_x, want_y, Vec::new())
                            } else {
                                snap_position(&config.cards, idx, &areas, want_x, want_y, w, h, ui_scale())
                            };
                            config.cards[idx].x = nx;
                            config.cards[idx].y = ny;
                            guides = g;
                            window.request_redraw();
                        }
                    }
                    if let Some(grab) = rail_drag {
                        if let (Some((_x, ty, th, _ty, thumb_h)), Some(area)) =
                            (rail_thumb(&config.cards, &areas, ui_scale()), rail_area(&config.cards, &areas))
                        {
                            let max_scroll = rail_max_scroll(&config.cards, &areas, ui_scale());
                            if th > thumb_h + 1.0 {
                                let target = ((my - grab - ty) / (th - thumb_h)) * max_scroll;
                                if set_rail_scroll(&mut config.cards, area, ui_scale(), target) {
                                    needs_save = true;
                                    window.request_redraw();
                                }
                            }
                        }
                        return;
                    }
                    let new_hover = card_at(&config.cards, mx, my);
                    let hint = resize_at(&config.cards, mx, my, ui_scale());
                    let add_hover = on_fab(add_btn_center, mx, my);
                    let gear_hover = on_fab(gear_btn_center, mx, my);
                    if new_hover != hover_idx || add_hover != add_btn_hover
                        || gear_hover != gear_btn_hover || hint != edge_hint
                    {
                        hover_idx = new_hover;
                        add_btn_hover = add_hover;
                        gear_btn_hover = gear_hover;
                        edge_hint = hint;
                        window.set_cursor_icon(if let Some((_, e)) = hint {
                            resize_cursor(e)
                        } else if hover_idx.is_some() || add_btn_hover || gear_btn_hover {
                            CursorIcon::Pointer
                        } else {
                            CursorIcon::Default
                        });
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if is_dragging { return; }
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 120.0,
                    };
                    if lines.abs() < 0.01 { return; }
                    let (mx, my) = cursor_pos;
                    // the rail's gutter scrolls the column; elsewhere the wheel zooms
                    if config.rail && on_rail_gutter(&config.cards, &areas, mx, my, ui_scale()) {
                        if let Some(area) = rail_area(&config.cards, &areas) {
                            if scroll_rail(&mut config.cards, area, ui_scale(), -lines * 120.0 * ui_scale()) {
                                needs_save = true;
                                window.request_redraw();
                            }
                        }
                        return;
                    }
                    if let Some(idx) = card_at(&config.cards, mx, my) {
                        if config.cards[idx].collapsed { return; }
                        let factor = (1.0 + lines * 0.1).clamp(0.5, 2.0);
                        let c = &mut config.cards[idx];
                        let old_w = c.width;
                        let new_w = (c.width * factor).clamp(220.0 * ui_scale(), 1200.0 * ui_scale());
                        if (new_w - old_w).abs() > 0.5 {
                            // zoom anchored at the cursor so the point under it stays fixed,
                            // keeping whatever aspect this card currently has (a landscape
                            // photo card must not snap back to portrait)
                            let aspect = if c.width > 1.0 {
                                (c.height / c.width).clamp(0.4, 2.6)
                            } else {
                                CARD_ASPECT
                            };
                            let ratio = new_w / old_w;
                            c.x = mx - (mx - c.x) * ratio;
                            c.y = my - (my - c.y) * ratio;
                            c.width = new_w;
                            c.height = new_w * aspect;
                            c.auto_fit = false; // the user owns the shape now
                            needs_save = true;
                            window.request_redraw();
                        }
                    }
                }
                WindowEvent::DroppedFile(path) => {
                    // card appears where the file was released
                    let (mx, my) = unsafe {
                        let mut pt = POINT { x: 0, y: 0 };
                        GetCursorPos(&mut pt);
                        (pt.x - vx, pt.y - vy)
                    };
                    let (fxc, fyc) = (mx as f32, my as f32);
                    let p = path.to_string_lossy().to_string();
                    // a drop on the + puck goes to free space instead of piling
                    // cards on top of the puck
                    let near_fab = (fxc - add_btn_center.0).abs() < 70.0 * ui_scale()
                        && (fyc - add_btn_center.1).abs() < 70.0 * ui_scale();
                    let at = if near_fab { None } else { Some((fxc, fyc)) };
                    add_paths(vec![p], at, &mut config, &mut thumbs, &areas, &mut anims,
                              (vw as f32 * 0.5, vh as f32 * 0.4));
                    needs_save = true;
                    drag_active = false;
                    window.request_redraw();
                }
                WindowEvent::Resized(ns) => {
                    let nw = ns.width.max(1);
                    let nh = ns.height.max(1);
                    fb = Framebuffer::new(nw, nh);
                    if nw != vw as u32 || nh != vh as u32 {
                        unsafe {
                            SetWindowPos(get_hwnd(&window), std::ptr::null_mut(),
                                vx, vy, vw, vh,
                                SWP_NOZORDER | SWP_NOACTIVATE);
                        }
                    }
                    window.request_redraw();
                }
                WindowEvent::CloseRequested => {
                    if needs_save { config.save(); }
                    remove_tray();
                    elwt.exit();
                }
                _ => {}
            },
            Event::AboutToWait => {
                if DISPLAY_CHANGED.swap(false, Ordering::Relaxed) {
                    // monitors came or went: rebuild the work areas and bring
                    // every card back inside one of them
                    areas = screen_areas(WORK_MARGIN * ui_scale(), vx, vy);
                    for c in &mut config.cards {
                        clamp_card(c, &areas);
                    }
                    needs_save = true;
                    window.request_redraw();
                }
                let fold_req = FOLD_REQUEST.swap(0, Ordering::Relaxed);
                if fold_req != 0 {
                    // Ctrl+Alt+H toggles; the tray menu asks for one direction
                    let collapse = if fold_req == 2 {
                        config.cards.iter().any(|c| !c.collapsed)
                    } else {
                        fold_req > 0
                    };
                    for c in &mut config.cards {
                        c.collapsed = collapse;
                        if !collapse {
                            clamp_card(c, &areas);
                        }
                    }
                    if config.rail { pack_column(&mut config.cards); }
                    needs_save = true;
                    window.request_redraw();
                }
                if HOTKEY_TIDY.swap(false, Ordering::Relaxed) {
                    // Ctrl+Alt+T: one-shot tidy into a right-hand column
                    config.rail = tidy_column(&mut config.cards, &areas);
                    guides.clear();
                    needs_save = true;
                    window.request_redraw();
                }
                if HOTKEY_ADD.swap(false, Ordering::Relaxed) {
                    // Ctrl+Alt+D: Explorer selection (via a copied selection) or
                    // whatever file list is already on the clipboard
                    let mut files = clipboard_files();
                    if files.is_empty() && copy_explorer_selection() {
                        std::thread::sleep(std::time::Duration::from_millis(140));
                        files = clipboard_files();
                    }
                    if files.is_empty() {
                        // nothing to copy: fall back to the file picker
                        *PENDING_ADDS.lock().unwrap() = vec![String::new()];
                    } else {
                        add_paths(files, None, &mut config, &mut thumbs, &areas, &mut anims,
                                  (vw as f32 * 0.5, vh as f32 * 0.45));
                        needs_save = true;
                        window.request_redraw();
                    }
                }
                if REFRESH_REQUESTED.swap(false, Ordering::Relaxed) {                    thumbs.clear();
                    generate_thumbnails(&config.cards, &mut thumbs);
                    window.request_redraw();
                }
                let pending = std::mem::take(&mut *PENDING_ADDS.lock().unwrap());
                if !pending.is_empty() {
                    let paths = if pending.len() == 1 && pending[0].is_empty() {
                        pick_files_dialog()
                    } else {
                        pending.into_iter().filter(|p| !p.is_empty()).collect()
                    };
                    if !paths.is_empty() {
                        add_paths(paths, None, &mut config, &mut thumbs, &areas, &mut anims,
                                  (vw as f32 * 0.5, vh as f32 * 0.45));
                        needs_save = true;
                    }
                }
                // entrance-animation ticks
                anims.retain(|(_, t)| t.elapsed().as_secs_f32() < 0.35);
                if !anims.is_empty() {
                    window.request_redraw();
                    elwt.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                        std::time::Instant::now() + std::time::Duration::from_millis(16)));
                }
                if needs_save {
                    config.save();
                    needs_save = false;
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }).unwrap();
}
