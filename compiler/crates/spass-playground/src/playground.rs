// Implements the Rust playground runtime: spass2d, spass3d, and native spio.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Win32 bindings (raw-dylib: rustc generates the import libraries itself,
// so this works on the gnullvm toolchain even without libgdi32.a).
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub type HWND = *mut c_void;
    pub type HINSTANCE = *mut c_void;
    pub type HDC = *mut c_void;
    pub type HMODULE = *mut c_void;
    pub type WPARAM = usize;
    pub type LPARAM = isize;
    pub type LRESULT = isize;
    pub type UINT = u32;
    pub type WORD = u16;
    pub type DWORD = u32;
    pub type BOOL = i32;

    #[repr(C)]
    pub struct WndClassExW {
        pub cb_size: UINT,
        pub style: UINT,
        pub lpfn_wnd_proc: extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT,
        pub cb_cls_extra: i32,
        pub cb_wnd_extra: i32,
        pub h_instance: HINSTANCE,
        pub h_icon: *mut c_void,
        pub h_cursor: *mut c_void,
        pub hbr_background: *mut c_void,
        pub lpsz_menu_name: *const u16,
        pub lpsz_class_name: *const u16,
        pub h_icon_sm: *mut c_void,
    }

    #[repr(C)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    pub struct Msg {
        pub hwnd: HWND,
        pub message: UINT,
        pub w_param: WPARAM,
        pub l_param: LPARAM,
        pub time: DWORD,
        pub pt: Point,
    }

    #[repr(C)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub bi_size: DWORD,
        pub bi_width: i32,
        pub bi_height: i32,
        pub bi_planes: WORD,
        pub bi_bit_count: WORD,
        pub bi_compression: DWORD,
        pub bi_size_image: DWORD,
        pub bi_x_pels_per_meter: i32,
        pub bi_y_pels_per_meter: i32,
        pub bi_clr_used: DWORD,
        pub bi_clr_important: DWORD,
    }

    #[repr(C)]
    pub struct RgbQuad {
        pub blue: u8,
        pub green: u8,
        pub red: u8,
        pub reserved: u8,
    }

    #[repr(C)]
    pub struct BitmapInfo {
        pub header: BitmapInfoHeader,
        pub colors: [RgbQuad; 1],
    }

    #[link(name = "user32", kind = "raw-dylib")]
    extern "system" {
        pub fn RegisterClassExW(c: *const WndClassExW) -> WORD;
        pub fn CreateWindowExW(
            ex_style: DWORD,
            class_name: *const u16,
            window_name: *const u16,
            style: DWORD,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: HWND,
            menu: *mut c_void,
            instance: HINSTANCE,
            param: *mut c_void,
        ) -> HWND;
        pub fn DefWindowProcW(h: HWND, m: UINT, w: WPARAM, l: LPARAM) -> LRESULT;
        pub fn PeekMessageW(m: *mut Msg, h: HWND, min: UINT, max: UINT, remove: UINT) -> BOOL;
        pub fn TranslateMessage(m: *const Msg) -> BOOL;
        pub fn DispatchMessageW(m: *const Msg) -> LRESULT;
        pub fn ShowWindow(h: HWND, cmd: i32) -> BOOL;
        pub fn UpdateWindow(h: HWND) -> BOOL;
        pub fn GetClientRect(h: HWND, rect: *mut Rect) -> BOOL;
        pub fn GetDC(h: HWND) -> HDC;
        pub fn ReleaseDC(h: HWND, dc: HDC) -> i32;
        pub fn DestroyWindow(h: HWND) -> BOOL;
        pub fn PostQuitMessage(code: i32);
        pub fn GetAsyncKeyState(vkey: i32) -> i16;
        pub fn GetCursorPos(pt: *mut Point) -> BOOL;
        pub fn ScreenToClient(h: HWND, pt: *mut Point) -> BOOL;
    }

    #[link(name = "gdi32", kind = "raw-dylib")]
    extern "system" {
        pub fn StretchDIBits(
            dc: HDC,
            xd: i32,
            yd: i32,
            wd: i32,
            hd: i32,
            xs: i32,
            ys: i32,
            ws: i32,
            hs: i32,
            bits: *const c_void,
            info: *const BitmapInfo,
            usage: UINT,
            rop: DWORD,
        ) -> i32;
    }

    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        pub fn GetModuleHandleW(name: *const u16) -> HMODULE;
    }

    pub const WS_OVERLAPPEDWINDOW: DWORD = 0x00CF_0000;
    pub const WS_VISIBLE: DWORD = 0x1000_0000;
    pub const PM_REMOVE: UINT = 0x0001;
    pub const WM_DESTROY: UINT = 0x0002;
    pub const SRCCOPY: DWORD = 0x00CC_0020;
    pub const BI_RGB: DWORD = 0;
    pub const DIB_RGB_COLORS: UINT = 0;
}

// ---------------------------------------------------------------------------
// Global playground state: one window, two layers.
// ---------------------------------------------------------------------------

/// The frame holds two layers.
///
/// `px` is the base layer: the background and the 3D scene. `ov` is
/// the 2D overlay, with an alpha per pixel. `out` is the composited
/// result that is handed to the window and to the PPM dump.
///
/// The layers are independent, so the draw order between `s2d:` and
/// `s3d:` no longer decides what ends up on top: the 2D overlay is
/// always composited over the 3D scene, and two 2D primitives still
/// cover each other in the order they were drawn.
struct Frame {
    w: i32,
    h: i32,
    /// The base layer: background plus 3D.
    px: Vec<u32>,
    /// The 2D overlay. Each entry is `0xAARRGGBB`; the alpha is 0 for
    /// "nothing drawn here".
    ov: Vec<u32>,
    /// The composited frame, rebuilt by `sp_composite` before every
    /// present.
    out: Vec<u32>,
    depth: Vec<f32>,
    depth_on: bool,
    t0: Instant,
    last: Instant,
    // Mouse state, refreshed by `sp_open` each frame.
    mx: f64,
    my: f64,
    mdx: f64,
    mdy: f64,
    mdown: bool,
    mprev: bool,
    wheel: f64,
    // Active slider (identified by its track position).
    slider: Option<(i32, i32)>,
    // Camera: eye, target, vertical FOV in degrees.
    cam: [f64; 7],
    light: [f64; 3],
    /// The model transform: local coordinates to world coordinates.
    model: M4,
    /// The transforms saved by `s3d:push`, restored by `s3d:pop`.
    stack: Vec<M4>,
}

static SP_FRAME: Mutex<Option<Frame>> = Mutex::new(None);
static SP_OPEN: AtomicBool = AtomicBool::new(false);
static SP_HWND: AtomicIsize = AtomicIsize::new(0);
static SP_FRAMES: AtomicU64 = AtomicU64::new(0);

fn sp_lock() -> std::sync::MutexGuard<'static, Option<Frame>> {
    SP_FRAME.lock().unwrap_or_else(|e| e.into_inner())
}

fn sp_frame_limit() -> u64 {
    std::env::var("SPASS_PLAYGROUND_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(windows) { 0 } else { 300 })
}

fn sp_pack(r: f64, g: f64, b: f64) -> u32 {
    let c = |v: f64| ((v.max(0.0).min(1.0)) * 255.0 + 0.5) as u32;
    0xFF00_0000 | (c(b) << 16) | (c(g) << 8) | c(r)
}

/// Pack a colour for the 2D overlay.
///
/// The alpha is the high byte: 0 means "nothing drawn here", 255 is
/// opaque. A translucent pixel lets the base layer show through when
/// the two layers are composited.
fn sp_pack_a(r: f64, g: f64, b: f64, a: f64) -> u32 {
    let c = |v: f64| ((v.max(0.0).min(1.0)) * 255.0 + 0.5) as u32;
    let ca = ((a.max(0.0).min(1.0)) * 255.0 + 0.5) as u32;
    (ca << 24) | (c(b) << 16) | (c(g) << 8) | c(r)
}

/// Composite the 2D overlay over the base layer into `out`.
///
/// The overlay is blended with the alpha it carries, so a translucent
/// panel over a 3D scene shows the scene through it. Both layers stay
/// untouched, so the next frame starts from the same base again.
fn sp_composite(g: &mut Frame) {
    for i in 0..g.px.len() {
        let ov = g.ov[i];
        let a = (ov >> 24) & 0xFF;
        if a == 0 {
            g.out[i] = g.px[i];
        } else if a == 255 {
            g.out[i] = 0xFF00_0000 | (ov & 0x00FF_FFFF);
        } else {
            let base = g.px[i];
            let br = base & 0xFF;
            let bg = (base >> 8) & 0xFF;
            let bb = (base >> 16) & 0xFF;
            let sr = ov & 0xFF;
            let sg = (ov >> 8) & 0xFF;
            let sb = (ov >> 16) & 0xFF;
            let inv = 255 - a;
            let r = (sr * a + br * inv) / 255;
            let gg = (sg * a + bg * inv) / 255;
            let b = (sb * a + bb * inv) / 255;
            g.out[i] = 0xFF00_0000 | (b << 16) | (gg << 8) | r;
        }
    }
}

fn sp_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
extern "system" fn sp_wnd_proc(
    h: win::HWND,
    m: win::UINT,
    _w: win::WPARAM,
    _l: win::LPARAM,
) -> win::LRESULT {
    if m == win::WM_DESTROY {
        SP_OPEN.store(false, Ordering::SeqCst);
        unsafe { win::PostQuitMessage(0) };
        return 0;
    }
    unsafe { win::DefWindowProcW(h, m, _w, _l) }
}

fn sp_pump() {
    #[cfg(windows)]
    unsafe {
        let mut msg = std::mem::MaybeUninit::<win::Msg>::uninit();
        while win::PeekMessageW(msg.as_mut_ptr(), std::ptr::null_mut(), 0, 0, win::PM_REMOVE) == 1
        {
            let m = msg.assume_init_ref();
            win::TranslateMessage(m);
            win::DispatchMessageW(m);
        }
    }
}

fn sp_time_dt() -> (f64, f64) {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let now = Instant::now();
            let dt = now.duration_since(f.last).as_secs_f64();
            (now.duration_since(f.t0).as_secs_f64(), dt)
        }
        None => (0.0, 0.0),
    }
}

fn sp_keycode(key: &str) -> i32 {
    let lower = key.trim().to_ascii_lowercase();
    match lower.as_str() {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "shift" => 0x10,
        "ctrl" | "control" => 0x11,
        "alt" => 0x12,
        "tab" => 0x09,
        "backspace" => 0x08,
        _ => lower.chars().next().map(|c| c as i32).unwrap_or(0),
    }
}

/// Create the playground window (idempotent). `s2d:window` and
/// `s3d:window` both route here -s2d and s3d share one surface.
fn sp_window(w: f64, h: f64, title: &str) -> bool {
    let wi = if w >= 32.0 { w as i32 } else { 32 };
    let hi = if h >= 32.0 { h as i32 } else { 32 };
    let mut g = sp_lock();
    if g.is_some() {
        return true;
    }
    let now = Instant::now();
        *g = Some(Frame {
        w: wi,
        h: hi,
        px: vec![0u32; (wi * hi) as usize],
        ov: vec![0u32; (wi * hi) as usize],
        out: vec![0u32; (wi * hi) as usize],
        depth: vec![f32::MAX; (wi * hi) as usize],
        depth_on: true,
        t0: now,
        last: now,
        mx: 0.0,
        my: 0.0,
        mdx: 0.0,
        mdy: 0.0,
        mdown: false,
        mprev: false,
        wheel: 0.0,
        slider: None,
        cam: [0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 60.0],
        light: [0.4, -1.0, 0.3],
        model: sp_m4_identity(),
        stack: Vec::new(),
    });
    SP_FRAMES.store(0, Ordering::SeqCst);

    #[cfg(windows)]
    unsafe {
        let class = sp_wide("SpassPlaygroundClass");
        let wide_title = sp_wide(title);
        let hinst = win::GetModuleHandleW(std::ptr::null());
        let wc = win::WndClassExW {
            cb_size: std::mem::size_of::<win::WndClassExW>() as win::UINT,
            style: 3, // CS_HREDRAW | CS_VREDRAW
            lpfn_wnd_proc: sp_wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: hinst,
            h_icon: std::ptr::null_mut(),
            h_cursor: std::ptr::null_mut(),
            hbr_background: std::ptr::null_mut(),
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: class.as_ptr(),
            h_icon_sm: std::ptr::null_mut(),
        };
        // Re-registering the class in the same process just fails quietly.
        win::RegisterClassExW(&wc);
        let hwnd = win::CreateWindowExW(
            0,
            class.as_ptr(),
            wide_title.as_ptr(),
            win::WS_OVERLAPPEDWINDOW | win::WS_VISIBLE,
            80,
            80,
            wi,
            hi,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
        *g = None;
            return false;
        }
        SP_HWND.store(hwnd as isize, Ordering::SeqCst);
        win::ShowWindow(hwnd, 1);
        win::UpdateWindow(hwnd);
    }
    #[cfg(not(windows))]
    {
        let _ = title;
    }
    SP_OPEN.store(true, Ordering::SeqCst);
    true
}

/// Blit the software framebuffer to the window (Windows only).
fn sp_blit(g: &mut Frame) {
    #[cfg(windows)]
    unsafe {
        let hwnd = SP_HWND.load(Ordering::SeqCst) as win::HWND;
        if !hwnd.is_null() {
            let dc = win::GetDC(hwnd);
            if !dc.is_null() {
                let bi = win::BitmapInfo {
                    header: win::BitmapInfoHeader {
                        bi_size: std::mem::size_of::<win::BitmapInfoHeader>() as win::DWORD,
                        bi_width: g.w,
                        bi_height: -g.h,
                        bi_planes: 1,
                        bi_bit_count: 32,
                        bi_compression: win::BI_RGB,
                        bi_size_image: 0,
                        bi_x_pels_per_meter: 0,
                        bi_y_pels_per_meter: 0,
                        bi_clr_used: 0,
                        bi_clr_important: 0,
                    },
                    colors: [win::RgbQuad { blue: 0, green: 0, red: 0, reserved: 0 }],
                };
                win::StretchDIBits(
                    dc, 0, 0, g.w, g.h, 0, 0, g.w, g.h,
                    g.out.as_ptr() as *const _, &bi,
                    win::DIB_RGB_COLORS, win::SRCCOPY,
                );
                win::ReleaseDC(hwnd, dc);
                win::UpdateWindow(hwnd);
            }
        }
    }
}

fn sp_dump_ppm(g: &Frame) {
    if let Ok(path) = std::env::var("SPASS_PLAYGROUND_PPM") {
        let mut out = format!("P6\n{} {}\n255\n", g.w, g.h).into_bytes();
        for p in &g.out {
            out.push((p & 0xFF) as u8);
            out.push(((p >> 8) & 0xFF) as u8);
            out.push(((p >> 16) & 0xFF) as u8);
        }
        let _ = std::fs::write(&path, out);
    }
}

/// `s2d:open` / `s3d:open` -pump messages, refresh input, and
/// report whether the app should keep running.
fn sp_open() -> bool {
    sp_pump();
    let mut g = sp_lock();
    let frame = match g.as_mut() {
        Some(f) => f,
        None => return false,
    };
    #[cfg(windows)]
    unsafe {
        let hwnd = SP_HWND.load(Ordering::SeqCst) as win::HWND;
        if !hwnd.is_null() {
            let mut pt = win::Point { x: 0, y: 0 };
            if win::GetCursorPos(&mut pt) == 1 {
                win::ScreenToClient(hwnd, &mut pt);
                let nx = pt.x as f64;
                let ny = pt.y as f64;
                frame.mdx = nx - frame.mx;
                frame.mdy = ny - frame.my;
                frame.mx = nx;
                frame.my = ny;
            }
            frame.mprev = frame.mdown;
            frame.mdown = win::GetAsyncKeyState(0x01) as u16 & 0x8000 != 0;
        }
    }
    let limit = sp_frame_limit();
    let frames = SP_FRAMES.load(Ordering::SeqCst);
    SP_OPEN.load(Ordering::SeqCst) && (limit == 0 || frames < limit)
}

/// `s2d:present` / `s3d:present` -show the frame and account for
/// frame timing. Returns false once the window is gone.
fn sp_present() -> bool {
    sp_pump();
    let mut g = sp_lock();
    let frame = match g.as_mut() {
        Some(f) => f,
        None => return false,
    };
    let frames = SP_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
    // The 2D overlay is composited over the base layer here, so the
    // order in which the program drew them does not matter.
    sp_composite(frame);
    sp_blit(frame);
    let limit = sp_frame_limit();
    if limit != 0 && frames >= limit {
        sp_dump_ppm(frame);
        SP_OPEN.store(false, Ordering::SeqCst);
        #[cfg(windows)]
        unsafe {
            let hwnd = SP_HWND.load(Ordering::SeqCst) as win::HWND;
            if !hwnd.is_null() {
                win::DestroyWindow(hwnd);
                SP_HWND.store(0, Ordering::SeqCst);
            }
        }
        return false;
    }
    frame.last = Instant::now();
    drop(g);
    // Cap the frame rate so a playground app doesn't spin a core.
    if limit == 0 {
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    true
}

// ---------------------------------------------------------------------------
// spass2d -software rasterizer primitives.
// ---------------------------------------------------------------------------

/// Write one pixel of the 2D overlay.
///
/// The 2D layer is separate from the base layer, so a primitive drawn
/// here never overwrites the 3D scene: the two are composited at
/// present time.
fn sp_px(g: &mut Frame, x: i32, y: i32, color: u32) {
    if x >= 0 && y >= 0 && x < g.w && y < g.h {
        g.ov[(y * g.w + x) as usize] = color;
    }
}

fn sp_clear_rgb(r: f64, gg: f64, b: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let c = sp_pack(r, gg, b);
            for p in f.px.iter_mut() {
            *p = c;
            }
            // Clearing the screen clears both layers: the 2D overlay is
            // part of the frame, not a separate screen.
            for p in f.ov.iter_mut() {
            *p = 0;
            }
            for d in f.depth.iter_mut() {
            *d = f32::MAX;
            }
            true
        }
        None => false,
    }
}

fn sp_pixel(x: f64, y: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            sp_px(f, x as i32, y as i32, sp_pack_a(r, gg, b, a));
            true
        }
        None => false,
    }
}

fn sp_line2d(x0: f64, y0: f64, x1: f64, y1: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let color = sp_pack_a(r, gg, b, a);
            let (mut x, mut y) = (x0 as i32, y0 as i32);
            let (xe, ye) = (x1 as i32, y1 as i32);
            let dx = (xe - x).abs();
            let dy = -(ye - y).abs();
            let sx = if x < xe { 1 } else { -1 };
            let sy = if y < ye { 1 } else { -1 };
            let mut err = dx + dy;
            loop {
                sp_px(f, x, y, color);
                if x == xe && y == ye {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
            true
        }
        None => false,
    }
}

fn sp_rect(x: f64, y: f64, w: f64, h: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let color = sp_pack_a(r, gg, b, a);
            let x0 = x as i32;
            let y0 = y as i32;
            let x1 = (x + w) as i32;
            let y1 = (y + h) as i32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    sp_px(f, xx, yy, color);
                }
            }
            true
        }
        None => false,
    }
}

fn sp_frame_rect(x: f64, y: f64, w: f64, h: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    let (x1, y1) = (x + w - 1.0, y + h - 1.0);
    sp_line2d(x, y, x1, y, r, gg, b, a)
        && sp_line2d(x1, y, x1, y1, r, gg, b, a)
        && sp_line2d(x1, y1, x, y1, r, gg, b, a)
        && sp_line2d(x, y1, x, y, r, gg, b, a)
}

/// `s2d:circle(cx, cy, rad,...)` - a filled circle.
///
/// The circle is the ellipse with equal radii, so both go through one
/// implementation and the two cannot drift apart.
fn sp_circle(cx: f64, cy: f64, rad: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    sp_ellipse(cx, cy, rad, rad, r, gg, b, a)
}

/// `s2d:ellipse(cx, cy, rx, ry,...)` - a filled axis-aligned ellipse.
///
/// Both radii are clamped to one pixel, so a degenerate ellipse is a
/// dot rather than a division by zero.
fn sp_ellipse(cx: f64, cy: f64, rx: f64, ry: f64, r: f64, gg: f64, b: f64, a: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let color = sp_pack_a(r, gg, b, a);
            let rx = if rx < 1.0 { 1.0 } else { rx };
            let ry = if ry < 1.0 { 1.0 } else { ry };
            // Scanline by row: at the row offset fy the half width is
            // rx * sqrt(1 - (fy/ry)^2). With rx == ry that is exactly
            // the circle's sqrt(r^2 - fy^2).
            for dy in -(ry as i32)..=(ry as i32) {
                let fy = dy as f64 + 0.5;
                let t = 1.0 - (fy * fy) / (ry * ry);
                let half = rx * t.max(0.0).sqrt();
                let x0 = (cx - half) as i32;
                let x1 = (cx + half) as i32 + 1;
                let y = (cy + fy) as i32;
                for xx in x0..x1 {
                    sp_px(f, xx, y, color);
                }
            }
            true
        }
        None => false,
    }
}

/// `s2d:tri(x0, y0, x1, y1, x2, y2,...)` - a filled triangle.
///
/// The winding order does not matter: the point-in-triangle test is
/// the barycentric one, and the weights are normalised by the signed
/// area, so both orientations fill the same pixels.
fn sp_tri2d(
    x0: f64, y0: f64,
    x1: f64, y1: f64,
    x2: f64, y2: f64,
    r: f64, gg: f64, b: f64, a: f64,
) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            let color = sp_pack_a(r, gg, b, a);
            let area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
            if area.abs() < 1e-12 {
                // A degenerate triangle covers no pixels.
                return true;
            }
            let inv = 1.0 / area;
            let min_x = x0.min(x1).min(x2).floor().max(0.0).min(f.w as f64) as i32;
            let max_x = x0.max(x1).max(x2).ceil().max(-1.0).min((f.w - 1) as f64) as i32;
            let min_y = y0.min(y1).min(y2).floor().max(0.0).min(f.h as f64) as i32;
            let max_y = y0.max(y1).max(y2).ceil().max(-1.0).min((f.h - 1) as f64) as i32;
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let px = x as f64 + 0.5;
                    let py = y as f64 + 0.5;
                    let w0 = ((x1 - px) * (y2 - py) - (y1 - py) * (x2 - px)) * inv;
                    let w1 = ((x2 - px) * (y0 - py) - (y2 - py) * (x0 - px)) * inv;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    sp_px(f, x, y, color);
                }
            }
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// spass2d text -classic 5x7 bitmap font (ASCII 32..126). The table is the
// long-standard "glcd" 5x7 font (redistributed with Adafruit_GFX, BSD).
// Column-major: 5 bytes per glyph, LSB of each byte = top row.
// ---------------------------------------------------------------------------

const SP_FONT5X7: [u8; 475] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5F, 0x00, 0x00, 0x00, 0x07,
    0x00, 0x07, 0x00, 0x14, 0x7F, 0x14, 0x7F, 0x14, 0x24, 0x2A, 0x7F, 0x2A,
    0x12, 0x23, 0x13, 0x08, 0x64, 0x62, 0x36, 0x49, 0x56, 0x20, 0x50, 0x00,
    0x08, 0x07, 0x03, 0x00, 0x00, 0x1C, 0x22, 0x41, 0x00, 0x00, 0x41, 0x22,
    0x1C, 0x00, 0x2A, 0x1C, 0x7F, 0x1C, 0x2A, 0x08, 0x08, 0x3E, 0x08, 0x08,
    0x00, 0x80, 0x70, 0x30, 0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x00, 0x00,
    0x60, 0x60, 0x00, 0x20, 0x10, 0x08, 0x04, 0x02, 0x3E, 0x51, 0x49, 0x45,
    0x3E, 0x00, 0x42, 0x7F, 0x40, 0x00, 0x72, 0x49, 0x49, 0x49, 0x46, 0x21,
    0x41, 0x49, 0x4D, 0x33, 0x18, 0x14, 0x12, 0x7F, 0x10, 0x27, 0x45, 0x45,
    0x45, 0x39, 0x3C, 0x4A, 0x49, 0x49, 0x31, 0x41, 0x21, 0x11, 0x09, 0x07,
    0x36, 0x49, 0x49, 0x49, 0x36, 0x46, 0x49, 0x49, 0x29, 0x1E, 0x00, 0x00,
    0x14, 0x00, 0x00, 0x00, 0x40, 0x34, 0x00, 0x00, 0x00, 0x08, 0x14, 0x22,
    0x41, 0x14, 0x14, 0x14, 0x14, 0x14, 0x00, 0x41, 0x22, 0x14, 0x08, 0x02,
    0x01, 0x59, 0x09, 0x06, 0x3E, 0x41, 0x5D, 0x59, 0x4E, 0x7C, 0x12, 0x11,
    0x12, 0x7C, 0x7F, 0x49, 0x49, 0x49, 0x36, 0x3E, 0x41, 0x41, 0x41, 0x22,
    0x7F, 0x41, 0x41, 0x41, 0x3E, 0x7F, 0x49, 0x49, 0x49, 0x41, 0x7F, 0x09,
    0x09, 0x09, 0x01, 0x3E, 0x41, 0x41, 0x51, 0x73, 0x7F, 0x08, 0x08, 0x08,
    0x7F, 0x00, 0x41, 0x7F, 0x41, 0x00, 0x20, 0x40, 0x41, 0x3F, 0x01, 0x7F,
    0x08, 0x14, 0x22, 0x41, 0x7F, 0x40, 0x40, 0x40, 0x40, 0x7F, 0x02, 0x1C,
    0x02, 0x7F, 0x7F, 0x04, 0x08, 0x10, 0x7F, 0x3E, 0x41, 0x41, 0x41, 0x3E,
    0x7F, 0x09, 0x09, 0x09, 0x06, 0x3E, 0x41, 0x51, 0x21, 0x5E, 0x7F, 0x09,
    0x19, 0x29, 0x46, 0x26, 0x49, 0x49, 0x49, 0x32, 0x03, 0x01, 0x7F, 0x01,
    0x03, 0x3F, 0x40, 0x40, 0x40, 0x3F, 0x1F, 0x20, 0x40, 0x20, 0x1F, 0x3F,
    0x40, 0x38, 0x40, 0x3F, 0x63, 0x14, 0x08, 0x14, 0x63, 0x03, 0x04, 0x78,
    0x04, 0x03, 0x61, 0x59, 0x49, 0x4D, 0x43, 0x00, 0x7F, 0x41, 0x41, 0x41,
    0x02, 0x04, 0x08, 0x10, 0x20, 0x00, 0x41, 0x41, 0x41, 0x7F, 0x04, 0x02,
    0x01, 0x02, 0x04, 0x40, 0x40, 0x40, 0x40, 0x40, 0x00, 0x03, 0x07, 0x08,
    0x00, 0x20, 0x54, 0x54, 0x78, 0x40, 0x7F, 0x28, 0x44, 0x44, 0x38, 0x38,
    0x44, 0x44, 0x44, 0x28, 0x38, 0x44, 0x44, 0x28, 0x7F, 0x38, 0x54, 0x54,
    0x54, 0x18, 0x00, 0x08, 0x7E, 0x09, 0x02, 0x18, 0xA4, 0xA4, 0x9C, 0x78,
    0x7F, 0x08, 0x04, 0x04, 0x78, 0x00, 0x44, 0x7D, 0x40, 0x00, 0x20, 0x40,
    0x40, 0x3D, 0x00, 0x7F, 0x10, 0x28, 0x44, 0x00, 0x00, 0x41, 0x7F, 0x40,
    0x00, 0x7C, 0x04, 0x78, 0x04, 0x78, 0x7C, 0x08, 0x04, 0x04, 0x78, 0x38,
    0x44, 0x44, 0x44, 0x38, 0xFC, 0x18, 0x24, 0x24, 0x18, 0x18, 0x24, 0x24,
    0x18, 0xFC, 0x7C, 0x08, 0x04, 0x04, 0x08, 0x48, 0x54, 0x54, 0x54, 0x24,
    0x04, 0x04, 0x3F, 0x44, 0x24, 0x3C, 0x40, 0x40, 0x20, 0x7C, 0x1C, 0x20,
    0x40, 0x20, 0x1C, 0x3C, 0x40, 0x30, 0x40, 0x3C, 0x44, 0x28, 0x10, 0x28,
    0x44, 0x4C, 0x90, 0x90, 0x90, 0x7C, 0x44, 0x64, 0x54, 0x4C, 0x44, 0x00,
    0x08, 0x36, 0x41, 0x00, 0x00, 0x00, 0x77, 0x00, 0x00, 0x00, 0x41, 0x36,
    0x08, 0x00, 0x02, 0x01, 0x02, 0x04, 0x02,
];

/// The pixel size of one text cell: five columns of ink, one column of
/// spacing, eight rows.
const SP_CELL_W: f64 = 6.0;
const SP_CELL_H: f64 = 8.0;

/// The scale is a whole number of pixels per cell, at least one.
fn sp_text_scale(scale: f64) -> i32 {
    if scale >= 1.0 { scale as i32 } else { 1 }
}

/// Draw `text` at (x, y) scaled by `scale` (1 = 6x8 cell). Returns the
/// advance of the widest line in pixels so callers can lay out runs.
fn sp_text_into(f: &mut Frame, x: f64, y: f64, scale: f64, color: u32, text: &str) -> f64 {
    let s = sp_text_scale(scale);
    let mut cx = x as i32;
    let mut cy = y as i32;
    let mut widest = 0i32;
    for ch in text.chars() {
        if ch == '\n' {
            // A newline starts a new line: the column returns to the
            // left margin and the row moves down by one cell.
            widest = widest.max(cx - x as i32);
            cx = x as i32;
            cy += SP_CELL_H as i32 * s;
            continue;
        }
        let c = ch as u32;
        let idx = if (32..=126).contains(&c) { (c - 32) as usize } else { ('?' as u32 - 32) as usize };
        for col in 0..5u32 {
            let bits = SP_FONT5X7[idx * 5 + col as usize];
            for row in 0..8u32 {
                if bits & (1 << row) != 0 {
                    for sy in 0..s {
                        for sx in 0..s {
                            sp_px(f, cx + col as i32 * s + sx, cy + row as i32 * s + sy, color);
                        }
                    }
                }
            }
        }
        cx += SP_CELL_W as i32 * s;
    }
    widest.max(cx - x as i32) as f64
}

/// `s2d:text_width(text, scale)` - the advance width of the widest line.
///
/// A glyph cell is 6 pixels wide at scale 1 (five columns of ink and
/// one of spacing), so a line of n characters is `n * 6 * scale` wide.
fn sp_text_width(text: &str, scale: f64) -> f64 {
    let s = sp_text_scale(scale) as f64;
    let mut widest = 0usize;
    let mut line = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            widest = widest.max(line);
            line = 0;
        } else {
            line += 1;
        }
    }
    widest.max(line) as f64 * SP_CELL_W * s
}

/// `s2d:text_height(scale)` - the height of one line of text.
fn sp_text_height(scale: f64) -> f64 {
    SP_CELL_H * sp_text_scale(scale) as f64
}

fn sp_text(x: f64, y: f64, scale: f64, r: f64, gg: f64, b: f64, a: f64, text: &str) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            sp_text_into(f, x, y, scale, sp_pack_a(r, gg, b, a), text);
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// spass2d immediate-mode UI. Widgets draw themselves every frame and
// return their state; all state lives in Spass, not in the runtime.
// ---------------------------------------------------------------------------

fn sp_in_rect(mx: f64, my: f64, x: f64, y: f64, w: f64, h: f64) -> bool {
    mx >= x && my >= y && mx < x + w && my < y + h
}

/// `s2d:button(x, y, w, h, "Label")` -true on the frame the button is
/// clicked (mouse press edge inside the button).
fn sp_button(x: f64, y: f64, w: f64, h: f64, label: &str, a: f64) -> bool {
    let mut g = sp_lock();
    let f = match g.as_mut() {
        Some(f) => f,
        None => return false,
    };
    let hover = sp_in_rect(f.mx, f.my, x, y, w, h);
    let clicked = hover && f.mdown && !f.mprev;
    let (br, bg, bb) = if hover { (0.36, 0.42, 0.55) } else { (0.25, 0.28, 0.34) };
    let color = sp_pack_a(br, bg, bb, a);
    let (x0, y0) = (x as i32, y as i32);
    let (x1, y1) = ((x + w) as i32, (y + h) as i32);
    for yy in y0..y1 {
        for xx in x0..x1 {
            sp_px(f, xx, yy, color);
        }
    }
    let border = sp_pack_a(0.6, 0.65, 0.75, a);
    for xx in x0..x1 {
        sp_px(f, xx, y0, border);
        sp_px(f, xx, y1 - 1, border);
    }
    for yy in y0..y1 {
        sp_px(f, x0, yy, border);
        sp_px(f, x1 - 1, yy, border);
    }
    let tc = sp_pack_a(0.95, 0.95, 0.95, a);
    sp_text_into(f, x + 6.0, y + ((h - 8.0) / 2.0).max(0.0), 1.0, tc, label);
    clicked
}

/// `s2d:slider(x, y, w, h, value, lo, hi)` -returns the new value.
/// Dragging works by press-start position identity.
fn sp_slider(x: f64, y: f64, w: f64, h: f64, value: f64, lo: f64, hi: f64, a: f64) -> f64 {
    let mut g = sp_lock();
    let f = match g.as_mut() {
        Some(f) => f,
        None => return value,
    };
    let span = if (hi - lo).abs() < 1e-12 { 1.0 } else { hi - lo };
    let mut v = value.max(lo).min(hi);
    let key = (x as i32, y as i32);
    if f.mdown {
        let started = f.slider.is_none();
        if started && sp_in_rect(f.mx, f.my, x - 4.0, y - 4.0, w + 8.0, h + 8.0) {
            f.slider = Some(key);
        }
        if f.slider == Some(key) {
            let t = ((f.mx - x) / w).max(0.0).min(1.0);
            v = lo + t * span;
        }
    } else {
        f.slider = None;
    }
    // Track.
    let track = sp_pack_a(0.2, 0.22, 0.26, a);
    let (x0, y0) = (x as i32, y as i32);
    let (x1, y1) = ((x + w) as i32, (y + h) as i32);
    for yy in y0..y1 {
        for xx in x0..x1 {
            sp_px(f, xx, yy, track);
        }
    }
    // Knob.
    let t = ((v - lo) / span).max(0.0).min(1.0);
    let kx = (x + t * (w - 6.0)) as i32;
    let knob = sp_pack_a(0.75, 0.8, 0.9, a);
    for yy in y0..y1 {
        for xx in kx..(kx + 6) {
            sp_px(f, xx, yy, knob);
        }
    }
    v
}

/// `s2d:checkbox(x, y, "Label", on)` -returns the new state.
fn sp_checkbox(x: f64, y: f64, label: &str, on: bool, a: f64) -> bool {
    let mut g = sp_lock();
    let f = match g.as_mut() {
        Some(f) => f,
        None => return on,
    };
    let mut state = on;
    let box_sz = 14.0;
    if f.mdown && !f.mprev && sp_in_rect(f.mx, f.my, x, y, box_sz + 4.0 + label.len() as f64 * 6.0, box_sz) {
        state = !on;
    }
    let fill = if state { (0.3, 0.65, 0.4) } else { (0.2, 0.22, 0.26) };
    let (x0, y0) = (x as i32, y as i32);
    let (x1, y1) = ((x + box_sz) as i32, (y + box_sz) as i32);
    for yy in y0..y1 {
        for xx in x0..x1 {
            sp_px(f, xx, yy, sp_pack_a(fill.0, fill.1, fill.2, a));
        }
    }
    let border = sp_pack_a(0.6, 0.65, 0.75, a);
    for xx in x0..x1 {
        sp_px(f, xx, y0, border);
        sp_px(f, xx, y1 - 1, border);
    }
    for yy in y0..y1 {
        sp_px(f, x0, yy, border);
        sp_px(f, x1 - 1, yy, border);
    }
    if state {
        let tick = sp_pack_a(0.95, 0.98, 0.95, a);
        for d in 0..6 {
            sp_px(f, x0 + 3 + d, y0 + 6, tick);
            sp_px(f, x0 + 3 + d, y0 + 7, tick);
            sp_px(f, x0 + 8 - d, y0 + 6 + d / 2, tick);
        }
    }
    sp_text_into(f, x + box_sz + 6.0, y + 3.0, 1.0, sp_pack_a(0.92, 0.92, 0.92, a), label);
    state
}

// Input queries (shared by s2d and s3d namespaces).
fn sp_mouse_x() -> f64 {
    sp_lock().as_ref().map(|f| f.mx).unwrap_or(0.0)
}
fn sp_mouse_y() -> f64 {
    sp_lock().as_ref().map(|f| f.my).unwrap_or(0.0)
}
fn sp_mouse_dx() -> f64 {
    sp_lock().as_ref().map(|f| f.mdx).unwrap_or(0.0)
}
fn sp_mouse_dy() -> f64 {
    sp_lock().as_ref().map(|f| f.mdy).unwrap_or(0.0)
}
fn sp_mouse_down() -> bool {
    sp_lock().as_ref().map(|f| f.mdown).unwrap_or(false)
}
fn sp_wheel() -> f64 {
    sp_lock().as_ref().map(|f| f.wheel).unwrap_or(0.0)
}
fn sp_key_down(key: &str) -> bool {
    #[cfg(windows)]
    unsafe {
        let vk = sp_keycode(key);
        vk != 0 && win::GetAsyncKeyState(vk) as u16 & 0x8000 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        false
    }
}

// ---------------------------------------------------------------------------
// spass3d -camera, near clipping, z-buffered rasterizer, lighting.
// World is right-handed, y-up. The camera looks down its forward axis.
// ---------------------------------------------------------------------------

const SP_NEAR: f64 = 0.05;

#[derive(Clone, Copy)]
struct V3 {
    x: f64,
    y: f64,
    z: f64,
}

impl V3 {
    fn new(x: f64, y: f64, z: f64) -> V3 {
        V3 { x, y, z }
    }
    fn dot(self, o: V3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        V3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    fn norm(self) -> V3 {
        let l = (self.dot(self)).sqrt();
        if l > 1e-12 {
            V3::new(self.x / l, self.y / l, self.z / l)
        } else {
            V3::new(0.0, 0.0, 0.0)
        }
    }
}

impl std::ops::Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        V3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

// ---------------------------------------------------------------------------
// The model transform.
//
// A scene is easier to build out of local frames than out of world
// coordinates: a box at the origin, rotated and moved, is three calls
// and stays readable. The transform is a 4x4 affine matrix that maps
// local coordinates to world coordinates; the camera then maps world
// coordinates to the screen. So the order is always
// local -> model -> world -> camera -> screen.
//
// The matrix is row-major: `m[row * 4 + col]`.
// ---------------------------------------------------------------------------

type M4 = [f64; 16];

/// The deepest transform stack a program may build.
const SP_STACK_MAX: usize = 64;

/// The most segments a sphere or a cylinder may be built from.
const SP_MAX_SEGS: i32 = 64;

fn sp_m4_identity() -> M4 {
    let mut m = [0.0f64; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

/// `a * b` - the transform `b` first, then `a`.
fn sp_m4_mul(a: &M4, b: &M4) -> M4 {
    let mut out = [0.0f64; 16];
    for r in 0..4 {
        for c in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[r * 4 + k] * b[k * 4 + c];
            }
            out[r * 4 + c] = sum;
        }
    }
    out
}

fn sp_m4_translate(tx: f64, ty: f64, tz: f64) -> M4 {
    let mut m = sp_m4_identity();
    m[3] = tx;
    m[7] = ty;
    m[11] = tz;
    m
}

fn sp_m4_scale(sx: f64, sy: f64, sz: f64) -> M4 {
    let mut m = sp_m4_identity();
    m[0] = sx;
    m[5] = sy;
    m[10] = sz;
    m
}

/// A rotation about the three axes, in degrees.
///
/// The axes turn in the order X, then Y, then Z: the rotation about X
/// reaches the vertex first.
fn sp_m4_rotate(rx: f64, ry: f64, rz: f64) -> M4 {
    let (ax, ay, az) = (rx.to_radians(), ry.to_radians(), rz.to_radians());
    let (sx, cx) = (ax.sin(), ax.cos());
    let (sy, cy) = (ay.sin(), ay.cos());
    let (sz, cz) = (az.sin(), az.cos());
    let mut x = sp_m4_identity();
    x[5] = cx;
    x[6] = -sx;
    x[9] = sx;
    x[10] = cx;
    let mut y = sp_m4_identity();
    y[0] = cy;
    y[2] = sy;
    y[8] = -sy;
    y[10] = cy;
    let mut z = sp_m4_identity();
    z[0] = cz;
    z[1] = -sz;
    z[4] = sz;
    z[5] = cz;
    sp_m4_mul(&z, &sp_m4_mul(&y, &x))
}

/// Map a local vertex through the transform.
fn sp_m4_apply(m: &M4, p: V3) -> V3 {
    V3::new(
        m[0] * p.x + m[1] * p.y + m[2] * p.z + m[3],
        m[4] * p.x + m[5] * p.y + m[6] * p.z + m[7],
        m[8] * p.x + m[9] * p.y + m[10] * p.z + m[11],
    )
}

/// Save the current transform; `s3d:pop` puts it back.
///
/// The stack holds at most `SP_STACK_MAX` levels: a program that pushes
/// without popping gets `false` rather than an unbounded stack.
fn sp_push() -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            if f.stack.len() >= SP_STACK_MAX {
                return false;
            }
            f.stack.push(f.model);
            true
        }
        None => false,
    }
}

/// Restore the transform saved by the matching `s3d:push`.
///
/// Popping an empty stack is an error, not a silent no-op: the program
/// asked for a frame that is not there.
fn sp_pop() -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => match f.stack.pop() {
            Some(m) => {
                f.model = m;
                true
            }
            None => false,
        },
        None => false,
    }
}

/// Compose a transform onto the current one.
fn sp_compose(op: M4) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            f.model = sp_m4_mul(&f.model, &op);
            true
        }
        None => false,
    }
}

fn sp_translate(tx: f64, ty: f64, tz: f64) -> bool {
    sp_compose(sp_m4_translate(tx, ty, tz))
}

fn sp_scale(sx: f64, sy: f64, sz: f64) -> bool {
    sp_compose(sp_m4_scale(sx, sy, sz))
}

fn sp_rotate(rx: f64, ry: f64, rz: f64) -> bool {
    sp_compose(sp_m4_rotate(rx, ry, rz))
}

/// View-space vertex: sx/sy screen-ready, iz = 1/z_view (depth key).
#[derive(Clone, Copy)]
struct SV {
    sx: f64,
    sy: f64,
    iz: f64,
    zv: f64,
}

fn sp_cam_basis(f: &Frame) -> (V3, V3, V3, V3) {
    let eye = V3::new(f.cam[0], f.cam[1], f.cam[2]);
    let target = V3::new(f.cam[3], f.cam[4], f.cam[5]);
    let fwd = (target - eye).norm();
    let right = fwd.cross(V3::new(0.0, 1.0, 0.0)).norm();
    let up = right.cross(fwd);
    (eye, right, up, fwd)
}

/// Sutherland-Hodgman near-plane clip of a world-space polygon, then
/// project the surviving vertices to screen space.
fn sp_clip_project(f: &Frame, poly: &[V3]) -> Vec<SV> {
    let (eye, right, up, fwd) = sp_cam_basis(f);
    let fov = f.cam[6].max(1.0).min(170.0);
    let focal = (f.h as f64 * 0.5) / (fov.to_radians() * 0.5).tan();
    let to_view = |p: V3| {
        let v = p - eye;
        (v.dot(right), v.dot(up), v.dot(fwd))
    };
    let mut view: Vec<(f64, f64, f64)> = poly.iter().map(|p| to_view(*p)).collect();
    let mut out: Vec<(f64, f64, f64)> = Vec::with_capacity(view.len() + 1);
    for i in 0..view.len() {
        let a = view[i];
        let b = view[(i + 1) % view.len()];
        let ain = a.2 >= SP_NEAR;
        let bin = b.2 >= SP_NEAR;
        if ain {
            out.push(a);
        }
        if ain != bin {
            let t = (SP_NEAR - a.2) / (b.2 - a.2);
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, SP_NEAR));
        }
    }
    view = out;
    view.into_iter()
        .map(|(xv, yv, zv)| SV {
            sx: f.w as f64 * 0.5 + xv * focal / zv,
            sy: f.h as f64 * 0.5 - yv * focal / zv,
            iz: 1.0 / zv,
            zv,
        })
        .collect()
}

/// Rasterize one screen-space triangle with 1/z-correct depth testing.
fn sp_raster(f: &mut Frame, a: SV, b: SV, c: SV, color: u32) {
    let min_x = a.sx.min(b.sx).min(c.sx).floor().max(0.0) as i32;
    let max_x = a.sx.max(b.sx).max(c.sx).ceil().min((f.w - 1) as f64) as i32;
    let min_y = a.sy.min(b.sy).min(c.sy).floor().max(0.0) as i32;
    let max_y = a.sy.max(b.sy).max(c.sy).ceil().min((f.h - 1) as f64) as i32;
    if min_x > max_x || min_y > max_y {
        return;
    }
    let area = (b.sx - a.sx) * (c.sy - a.sy) - (b.sy - a.sy) * (c.sx - a.sx);
    if area.abs() < 1e-12 {
        return;
    }
    let inv = 1.0 / area;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f64 + 0.5;
            let py = y as f64 + 0.5;
            let w0 = ((b.sx - px) * (c.sy - py) - (b.sy - py) * (c.sx - px)) * inv;
            let w1 = ((c.sx - px) * (a.sy - py) - (c.sy - py) * (a.sx - px)) * inv;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let iz = w0 * a.iz + w1 * b.iz + w2 * c.iz;
            let idx = (y * f.w + x) as usize;
            if f.depth_on {
                if iz <= 0.0 || (1.0 / iz) >= f.depth[idx] as f64 {
                    continue;
                }
                f.depth[idx] = (1.0 / iz) as f32;
            }
            f.px[idx] = color;
        }
    }
}

/// Shaded color for a world-space triangle under the directional light
/// (two-sided: |n dot l|, so winding never turns a face black).
fn sp_shade(light: [f64; 3], a: V3, b: V3, c: V3, r: f64, g: f64, bl: f64) -> u32 {
    let n = (b - a).cross(c - a).norm();
    let l = V3::new(light[0], light[1], light[2]).norm();
    let k = n.dot(l).abs();
    let inten = 0.35 + 0.65 * k;
    sp_pack(r * inten, g * inten, bl * inten)
}

fn sp_tri3d(
    ax: f64, ay: f64, az: f64,
    bx: f64, by: f64, bz: f64,
    cx: f64, cy: f64, cz: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    sp_tri3d_at(
        V3::new(ax, ay, az),
        V3::new(bx, by, bz),
        V3::new(cx, cy, cz),
        r,
        g,
        b,
    )
}

/// A triangle given by its three local vertices.
///
/// The model transform is applied here, so every 3D primitive goes
/// through the same path from local coordinates to the screen.
fn sp_tri3d_at(a: V3, b: V3, c: V3, r: f64, g: f64, bl: f64) -> bool {
    let mut guard = sp_lock();
    let f = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    let (wa, wb, wc) = (
        sp_m4_apply(&f.model, a),
        sp_m4_apply(&f.model, b),
        sp_m4_apply(&f.model, c),
    );
    let color = sp_shade(f.light, wa, wb, wc, r, g, bl);
    let sv = sp_clip_project(f, &[wa, wb, wc]);
    for i in 1..sv.len().saturating_sub(1) {
        sp_raster(f, sv[0], sv[i], sv[i + 1], color);
    }
    true
}

/// `s3d:sphere(cx, cy, cz, rad, segs, r, g, b)` - a flat-shaded sphere.
///
/// The sphere is a UV grid: `segs` columns around the equator and half
/// as many rows from pole to pole. The pole rows collapse to a point,
/// so the caps need no special case.
fn sp_sphere(cx: f64, cy: f64, cz: f64, rad: f64, segs: i32, r: f64, g: f64, b: f64) -> bool {
    let segs = segs.clamp(3, SP_MAX_SEGS);
    let rings = (segs / 2).max(2);
    let vertex = |j: i32, i: i32| -> V3 {
        let phi = std::f64::consts::PI * j as f64 / rings as f64;
        let theta = 2.0 * std::f64::consts::PI * i as f64 / segs as f64;
        V3::new(
            cx + rad * phi.sin() * theta.cos(),
            cy + rad * phi.cos(),
            cz + rad * phi.sin() * theta.sin(),
        )
    };
    let mut ok = true;
    for j in 0..rings {
        for i in 0..segs {
            let p00 = vertex(j, i);
            let p01 = vertex(j, i + 1);
            let p10 = vertex(j + 1, i);
            let p11 = vertex(j + 1, i + 1);
            ok &= sp_tri3d_at(p00, p10, p11, r, g, b);
            ok &= sp_tri3d_at(p00, p11, p01, r, g, b);
        }
    }
    ok
}

/// `s3d:cylinder(cx, cy, cz, rad, h, segs, r, g, b)` - a flat-shaded
/// cylinder with its axis along Y.
///
/// The body is a ring of quads; the two caps are triangle fans.
fn sp_cylinder(
    cx: f64, cy: f64, cz: f64,
    rad: f64, h: f64,
    segs: i32,
    r: f64, g: f64, b: f64,
) -> bool {
    let segs = segs.clamp(3, SP_MAX_SEGS);
    let (y0, y1) = (cy - h * 0.5, cy + h * 0.5);
    let ring = |i: i32| -> V3 {
        let theta = 2.0 * std::f64::consts::PI * i as f64 / segs as f64;
        V3::new(cx + rad * theta.cos(), 0.0, cz + rad * theta.sin())
    };
    let mut ok = true;
    for i in 0..segs {
        let a = ring(i);
        let c = ring(i + 1);
        let (a0, a1) = (V3::new(a.x, y0, a.z), V3::new(a.x, y1, a.z));
        let (c0, c1) = (V3::new(c.x, y0, c.z), V3::new(c.x, y1, c.z));
        ok &= sp_tri3d_at(a0, c0, c1, r, g, b);
        ok &= sp_tri3d_at(a0, c1, a1, r, g, b);
        // The caps: a fan around the centre of each end.
        let bottom = V3::new(cx, y0, cz);
        let top = V3::new(cx, y1, cz);
        ok &= sp_tri3d_at(bottom, c0, a0, r, g, b);
        ok &= sp_tri3d_at(top, a1, c1, r, g, b);
    }
    ok
}

/// `s3d:quad(a, b, c, d, color)` -two triangles (a,b,c) + (a,c,d).
/// Vertices in ring order; the diagonal a-c is used for splitting.
#[allow(clippy::too_many_arguments)]
fn sp_quad3d(
    x0: f64, y0: f64, z0: f64,
    x1: f64, y1: f64, z1: f64,
    x2: f64, y2: f64, z2: f64,
    x3: f64, y3: f64, z3: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    sp_tri3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, r, g, b)
        && sp_tri3d(x0, y0, z0, x2, y2, z2, x3, y3, z3, r, g, b)
}

/// Draw an already-shaded world triangle (shared by box/grid paths).
fn sp_tri_project(a: V3, b: V3, c: V3, color: u32) -> bool {
    let mut guard = sp_lock();
    let f = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    let sv = sp_clip_project(f, &[a, b, c]);
    for i in 1..sv.len().saturating_sub(1) {
        sp_raster(f, sv[0], sv[i], sv[i + 1], color);
    }
    true
}

/// `s3d:box(cx, cy, cz, sx, sy, sz, r, g, b)` -axis-aligned shaded box.
#[allow(clippy::too_many_arguments)]
fn sp_box3d(
    cx: f64, cy: f64, cz: f64,
    sx: f64, sy: f64, sz: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    let (light, model) = sp_lock()
        .as_ref()
        .map(|f| (f.light, f.model))
        .unwrap_or(([0.4, -1.0, 0.3], sp_m4_identity()));
    let hx = sx * 0.5;
    let hy = sy * 0.5;
    let hz = sz * 0.5;
    let (x0, y0, z0) = (cx - hx, cy - hy, cz - hz);
    let (x1, y1, z1) = (cx + hx, cy + hy, cz + hz);
    // 8 corners: bit0=x, bit1=y, bit2=z.
    let corner = |i: u8| {
        sp_m4_apply(
            &model,
            V3::new(
                if i & 1 == 0 { x0 } else { x1 },
                if i & 2 == 0 { y0 } else { y1 },
                if i & 4 == 0 { z0 } else { z1 },
            ),
        )
    };
    // 6 faces as quads (ring order around each face).
    let faces: [[u8; 4]; 6] = [
        [0, 2, 3, 1], // -z
        [4, 5, 7, 6], // +z
        [0, 1, 5, 4], // -y
        [2, 6, 7, 3], // +y
        [0, 4, 6, 2], // -x
        [1, 3, 7, 5], // +x
    ];
    let mut ok = true;
    for face in &faces {
        let a = corner(face[0]);
        let bb = corner(face[1]);
        let c = corner(face[2]);
        let d = corner(face[3]);
        let color = sp_shade(light, a, bb, c, r, g, b);
        ok &= sp_tri_project(a, bb, c, color);
        ok &= sp_tri_project(a, c, d, color);
    }
    ok
}

/// `s3d:grid(half, step, r, g, b)` -ground grid on the y=0 plane.
/// Lines are depth-tested per pixel via the line's interpolated z.
fn sp_grid3d(half: f64, step: f64, r: f64, g: f64, b: f64) -> bool {
    let step = if step <= 0.0 { 1.0 } else { step };
    let half = if half <= 0.0 { 1.0 } else { half };
    let color = sp_pack(r, g, b);
    let mut ok = true;
    let mut t = -half;
    while t <= half + 1e-9 {
        ok &= sp_line3d(t, 0.0, -half, t, 0.0, half, color);
        ok &= sp_line3d(-half, 0.0, t, half, 0.0, t, color);
        t += step;
    }
    ok
}

/// Depth-tested 3D line: project both ends (skip when either endpoint
/// is behind the near plane), then walk the 2D line testing 1/z.
fn sp_line3d(ax: f64, ay: f64, az: f64, bx: f64, by: f64, bz: f64, color: u32) -> bool {
    let mut guard = sp_lock();
    let f = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    let (pa, pb) = (
        sp_clip_project(f, &[sp_m4_apply(&f.model, V3::new(ax, ay, az))]),
        sp_clip_project(f, &[sp_m4_apply(&f.model, V3::new(bx, by, bz))]),
    );
    if pa.is_empty() || pb.is_empty() {
        return false;
    }
    let (a, b) = (pa[0], pb[0]);
    let (mut x, mut y) = (a.sx as i32, a.sy as i32);
    let (xe, ye) = (b.sx as i32, b.sy as i32);
    let dx = (xe - x).abs();
    let dy = -(ye - y).abs();
    let sx = if x < xe { 1 } else { -1 };
    let sy = if y < ye { 1 } else { -1 };
    let mut err = dx + dy;
    let steps = (dx + dy.abs()).max(1) as f64;
    let mut i = 0.0;
    loop {
        let t = i / steps;
        let iz = a.iz + (b.iz - a.iz) * t;
        if x >= 0 && y >= 0 && x < f.w && y < f.h {
            let idx = (y * f.w + x) as usize;
            if !f.depth_on || (iz > 0.0 && (1.0 / iz) < f.depth[idx] as f64) {
                f.px[idx] = color;
            }
        }
        if x == xe && y == ye {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
            i += 1.0;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
            i += 1.0;
        }
    }
    true
}

fn sp_set_camera(ex: f64, ey: f64, ez: f64, tx: f64, ty: f64, tz: f64, fov: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            f.cam = [ex, ey, ez, tx, ty, tz, fov];
            true
        }
        None => false,
    }
}

fn sp_set_light(dx: f64, dy: f64, dz: f64) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            f.light = [dx, dy, dz];
            true
        }
        None => false,
    }
}

fn sp_set_depth(on: bool) -> bool {
    let mut g = sp_lock();
    match g.as_mut() {
        Some(f) => {
            f.depth_on = on;
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// spio -native file I/O. The compiled Spass program itself reads,
// writes, edits and lists files through std::fs. No runner, no daemon,
// no helper process: spassio works by itself.
//
// Strings returned into Spass live for the lifetime of the process -
// the generated code's string ABI is `&'static str`.
//
// A program that reads the same file every frame would otherwise grow
// the heap once per read, so the contents are interned: the same text
// is stored once and every read returns a pointer to it.
//
// `ponytail`: strings that are read are still never freed, so a
// program that reads a million *different* files grows without bound.
// A real string lifetime (an owned `String` in the ABI, or an explicit
// free) is the upgrade path.
// ---------------------------------------------------------------------------

static SP_STRINGS: std::sync::Mutex<Option<std::collections::HashMap<String, &'static str>>> =
    std::sync::Mutex::new(None);

fn sp_leak(s: String) -> &'static str {
    let mut guard = SP_STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    if let Some(existing) = map.get(&s) {
        // The value is a `&'static str` (Copy), so handing it out does
        // not borrow the map.
        return *existing;
    }
    let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
    map.insert(s, leaked);
    leaked
}

fn sp_read_text(path: &str) -> &'static str {
    match std::fs::read_to_string(path) {
        Ok(s) => sp_leak(s),
        Err(_) => "",
    }
}

fn sp_write_text(path: &str, text: &str) -> bool {
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::write(path, text).is_ok()
}

fn sp_append_text(path: &str, text: &str) -> bool {
    use std::io::Write;
    if let Some(dir) = std::path::Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => f.write_all(text.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

fn sp_line_count(path: &str) -> i32 {
    match std::fs::read_to_string(path) {
        Ok(s) => s.lines().count() as i32,
        Err(_) => 0,
    }
}

fn sp_read_line(path: &str, idx: i32) -> &'static str {
    let i = if idx >= 0 { idx as usize } else { usize::MAX };
    match std::fs::read_to_string(path) {
        Ok(s) => match s.lines().nth(i) {
            Some(line) => sp_leak(line.to_string()),
            None => "",
        },
        Err(_) => "",
    }
}

/// In-place edit: replace every occurrence of `old` with `new`.
fn sp_replace_text(path: &str, old: &str, new: &str) -> bool {
    if old.is_empty() {
        return false;
    }
    match std::fs::read_to_string(path) {
        Ok(s) => {
            if !s.contains(old) {
                return false;
            }
            std::fs::write(path, s.replace(old, new)).is_ok()
        }
        Err(_) => false,
    }
}

fn sp_read_f64(path: &str) -> f64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn sp_write_f64(path: &str, v: f64) -> bool {
    sp_write_text(path, &format!("{}", v))
}

fn sp_append_f64(path: &str, v: f64) -> bool {
    sp_append_text(path, &format!("{}\n", v))
}

fn sp_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

fn sp_size(path: &str) -> f64 {
    std::fs::metadata(path).map(|m| m.len() as f64).unwrap_or(-1.0)
}

fn sp_remove(path: &str) -> bool {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).is_ok()
    } else {
        std::fs::remove_file(p).is_ok()
    }
}

fn sp_mkdir(path: &str) -> bool {
    std::fs::create_dir_all(path).is_ok()
}

fn sp_copy(from: &str, to: &str) -> bool {
    if let Some(dir) = std::path::Path::new(to).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::copy(from, to).is_ok()
}

fn sp_rename(from: &str, to: &str) -> bool {
    if let Some(dir) = std::path::Path::new(to).parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::rename(from, to).is_ok()
}

fn sp_list_count(dir: &str) -> i32 {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.count() as i32,
        Err(_) => 0,
    }
}

fn sp_list_name(dir: &str, idx: i32) -> &'static str {
    let i = if idx >= 0 { idx as usize } else { usize::MAX };
    match std::fs::read_dir(dir) {
        Ok(entries) => match entries.flatten().nth(i) {
            Some(e) => sp_leak(e.file_name().to_string_lossy().into_owned()),
            None => "",
        },
        Err(_) => "",
    }
}

fn sp_print(text: &str) -> bool {
    use std::io::Write;
    println!("{}", text);
    let _ = std::io::stdout().flush();
    true
}

fn sp_print_num(v: f64) -> bool {
    use std::io::Write;
    println!("{}", v);
    let _ = std::io::stdout().flush();
    true
}

// ---------------------------------------------------------------------------
// Public runtime surface -every `s2d:` / `s3d:` / `spio:` call in Spass
// compiles to one of these `spass_<ns>_<func>` functions (the C backend
// uses the same names; see playground.c).
// ---------------------------------------------------------------------------

// --- spass2d (s2d:) ---------------------------------------------------------

#[no_mangle]
pub fn spass_s2d_window(w: f64, h: f64, title: &str) -> bool {
    sp_window(w, h, title)
}
#[no_mangle]
pub fn spass_s2d_open() -> bool {
    sp_open()
}
#[no_mangle]
pub fn spass_s2d_present() -> bool {
    sp_present()
}
#[no_mangle]
pub fn spass_s2d_clear(r: f64, g: f64, b: f64) -> bool {
    sp_clear_rgb(r, g, b)
}
#[no_mangle]
pub fn spass_s2d_pixel(x: f64, y: f64, r: f64, g: f64, b: f64, a: f64) -> bool {
    sp_pixel(x, y, r, g, b, a)
}
#[no_mangle]
pub fn spass_s2d_line(x0: f64, y0: f64, x1: f64, y1: f64, r: f64, g: f64, b: f64, a: f64) -> bool {
    sp_line2d(x0, y0, x1, y1, r, g, b, a)
}
#[no_mangle]
pub fn spass_s2d_rect(x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) -> bool {
    sp_rect(x, y, w, h, r, g, b, a)
}
#[no_mangle]
pub fn spass_s2d_frame(x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) -> bool {
    sp_frame_rect(x, y, w, h, r, g, b, a)
}
#[no_mangle]
pub fn spass_s2d_circle(cx: f64, cy: f64, rad: f64, r: f64, g: f64, b: f64, a: f64) -> bool {
    sp_circle(cx, cy, rad, r, g, b, a)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s2d_ellipse(
    cx: f64, cy: f64, rx: f64, ry: f64, r: f64, g: f64, b: f64, a: f64,
) -> bool {
    sp_ellipse(cx, cy, rx, ry, r, g, b, a)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s2d_tri(
    x0: f64, y0: f64, x1: f64, y1: f64, x2: f64, y2: f64,
    r: f64, g: f64, b: f64, a: f64,
) -> bool {
    sp_tri2d(x0, y0, x1, y1, x2, y2, r, g, b, a)
}
#[no_mangle]
pub fn spass_s2d_text_width(text: &str, scale: f64) -> f64 {
    sp_text_width(text, scale)
}
#[no_mangle]
pub fn spass_s2d_text_height(scale: f64) -> f64 {
    sp_text_height(scale)
}
#[no_mangle]
pub fn spass_s2d_text(
    x: f64, y: f64, scale: f64, r: f64, g: f64, b: f64, a: f64, text: &str,
) -> bool {
    sp_text(x, y, scale, r, g, b, a, text)
}
#[no_mangle]
pub fn spass_s2d_label(x: f64, y: f64, text: &str, a: f64) -> bool {
    sp_text(x, y, 1.0, 0.92, 0.93, 0.95, a, text)
}
#[no_mangle]
pub fn spass_s2d_button(x: f64, y: f64, w: f64, h: f64, label: &str, a: f64) -> bool {
    sp_button(x, y, w, h, label, a)
}
#[no_mangle]
pub fn spass_s2d_slider(x: f64, y: f64, w: f64, h: f64, v: f64, lo: f64, hi: f64, a: f64) -> f64 {
    sp_slider(x, y, w, h, v, lo, hi, a)
}
#[no_mangle]
pub fn spass_s2d_checkbox(x: f64, y: f64, label: &str, on: bool, a: f64) -> bool {
    sp_checkbox(x, y, label, on, a)
}
#[no_mangle]
pub fn spass_s2d_mouse_x() -> f64 {
    sp_mouse_x()
}
#[no_mangle]
pub fn spass_s2d_mouse_y() -> f64 {
    sp_mouse_y()
}
#[no_mangle]
pub fn spass_s2d_mouse_dx() -> f64 {
    sp_mouse_dx()
}
#[no_mangle]
pub fn spass_s2d_mouse_dy() -> f64 {
    sp_mouse_dy()
}
#[no_mangle]
pub fn spass_s2d_mouse_down() -> bool {
    sp_mouse_down()
}
#[no_mangle]
pub fn spass_s2d_wheel() -> f64 {
    sp_wheel()
}
#[no_mangle]
pub fn spass_s2d_key(key: &str) -> bool {
    sp_key_down(key)
}
#[no_mangle]
pub fn spass_s2d_time() -> f64 {
    sp_time_dt().0
}
#[no_mangle]
pub fn spass_s2d_dt() -> f64 {
    sp_time_dt().1
}

// --- spass3d (s3d:) -same window/surface, 3D pipeline on top. --------------

#[no_mangle]
pub fn spass_s3d_window(w: f64, h: f64, title: &str) -> bool {
    sp_window(w, h, title)
}
#[no_mangle]
pub fn spass_s3d_open() -> bool {
    sp_open()
}
#[no_mangle]
pub fn spass_s3d_present() -> bool {
    sp_present()
}
#[no_mangle]
pub fn spass_s3d_clear(r: f64, g: f64, b: f64) -> bool {
    sp_clear_rgb(r, g, b)
}
#[no_mangle]
pub fn spass_s3d_camera(ex: f64, ey: f64, ez: f64, tx: f64, ty: f64, tz: f64, fov: f64) -> bool {
    sp_set_camera(ex, ey, ez, tx, ty, tz, fov)
}
#[no_mangle]
pub fn spass_s3d_light(dx: f64, dy: f64, dz: f64) -> bool {
    sp_set_light(dx, dy, dz)
}
#[no_mangle]
pub fn spass_s3d_depth(on: bool) -> bool {
    sp_set_depth(on)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s3d_tri(
    x0: f64, y0: f64, z0: f64,
    x1: f64, y1: f64, z1: f64,
    x2: f64, y2: f64, z2: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    sp_tri3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, r, g, b)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s3d_quad(
    x0: f64, y0: f64, z0: f64,
    x1: f64, y1: f64, z1: f64,
    x2: f64, y2: f64, z2: f64,
    x3: f64, y3: f64, z3: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    sp_quad3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, x3, y3, z3, r, g, b)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s3d_box(
    cx: f64, cy: f64, cz: f64,
    sx: f64, sy: f64, sz: f64,
    r: f64, g: f64, b: f64,
) -> bool {
    sp_box3d(cx, cy, cz, sx, sy, sz, r, g, b)
}
#[no_mangle]
pub fn spass_s3d_grid(half: f64, step: f64, r: f64, g: f64, b: f64) -> bool {
    sp_grid3d(half, step, r, g, b)
}
#[no_mangle]
pub fn spass_s3d_push() -> bool {
    sp_push()
}
#[no_mangle]
pub fn spass_s3d_pop() -> bool {
    sp_pop()
}
#[no_mangle]
pub fn spass_s3d_translate(tx: f64, ty: f64, tz: f64) -> bool {
    sp_translate(tx, ty, tz)
}
#[no_mangle]
pub fn spass_s3d_rotate(rx: f64, ry: f64, rz: f64) -> bool {
    sp_rotate(rx, ry, rz)
}
#[no_mangle]
pub fn spass_s3d_scale(sx: f64, sy: f64, sz: f64) -> bool {
    sp_scale(sx, sy, sz)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s3d_sphere(
    cx: f64, cy: f64, cz: f64, rad: f64, segs: i32, r: f64, g: f64, b: f64,
) -> bool {
    sp_sphere(cx, cy, cz, rad, segs, r, g, b)
}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn spass_s3d_cylinder(
    cx: f64, cy: f64, cz: f64, rad: f64, h: f64, segs: i32, r: f64, g: f64, b: f64,
) -> bool {
    sp_cylinder(cx, cy, cz, rad, h, segs, r, g, b)
}
#[no_mangle]
pub fn spass_s3d_mouse_x() -> f64 {
    sp_mouse_x()
}
#[no_mangle]
pub fn spass_s3d_mouse_y() -> f64 {
    sp_mouse_y()
}
#[no_mangle]
pub fn spass_s3d_mouse_dx() -> f64 {
    sp_mouse_dx()
}
#[no_mangle]
pub fn spass_s3d_mouse_dy() -> f64 {
    sp_mouse_dy()
}
#[no_mangle]
pub fn spass_s3d_mouse_down() -> bool {
    sp_mouse_down()
}
#[no_mangle]
pub fn spass_s3d_key(key: &str) -> bool {
    sp_key_down(key)
}
#[no_mangle]
pub fn spass_s3d_time() -> f64 {
    sp_time_dt().0
}
#[no_mangle]
pub fn spass_s3d_dt() -> f64 {
    sp_time_dt().1
}

// --- spio -native file I/O, executed by the program itself. ---------------

#[no_mangle]
pub fn spass_spio_print(text: &str) -> bool {
    sp_print(text)
}
#[no_mangle]
pub fn spass_spio_print_num(v: f64) -> bool {
    sp_print_num(v)
}
#[no_mangle]
pub fn spass_spio_read_text(path: &str) -> &'static str {
    sp_read_text(path)
}
#[no_mangle]
pub fn spass_spio_write_text(path: &str, text: &str) -> bool {
    sp_write_text(path, text)
}
#[no_mangle]
pub fn spass_spio_append_text(path: &str, text: &str) -> bool {
    sp_append_text(path, text)
}
#[no_mangle]
pub fn spass_spio_read_line(path: &str, idx: i32) -> &'static str {
    sp_read_line(path, idx)
}
#[no_mangle]
pub fn spass_spio_line_count(path: &str) -> i32 {
    sp_line_count(path)
}
#[no_mangle]
pub fn spass_spio_replace_text(path: &str, old: &str, new: &str) -> bool {
    sp_replace_text(path, old, new)
}
#[no_mangle]
pub fn spass_spio_read_f64(path: &str) -> f64 {
    sp_read_f64(path)
}
#[no_mangle]
pub fn spass_spio_write_f64(path: &str, v: f64) -> bool {
    sp_write_f64(path, v)
}
#[no_mangle]
pub fn spass_spio_append_f64(path: &str, v: f64) -> bool {
    sp_append_f64(path, v)
}
#[no_mangle]
pub fn spass_spio_exists(path: &str) -> bool {
    sp_exists(path)
}
#[no_mangle]
pub fn spass_spio_size(path: &str) -> f64 {
    sp_size(path)
}
#[no_mangle]
pub fn spass_spio_remove(path: &str) -> bool {
    sp_remove(path)
}
#[no_mangle]
pub fn spass_spio_mkdir(path: &str) -> bool {
    sp_mkdir(path)
}
#[no_mangle]
pub fn spass_spio_copy(from: &str, to: &str) -> bool {
    sp_copy(from, to)
}
#[no_mangle]
pub fn spass_spio_rename(from: &str, to: &str) -> bool {
    sp_rename(from, to)
}
#[no_mangle]
pub fn spass_spio_list_count(dir: &str) -> i32 {
    sp_list_count(dir)
}
#[no_mangle]
pub fn spass_spio_list_name(dir: &str, idx: i32) -> &'static str {
    sp_list_name(dir, idx)
}














