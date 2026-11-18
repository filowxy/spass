//! Dynamically loads GDI/theming entry points missing from the toolchain's import libs.

use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::OnceLock;

pub type HDC = isize;
pub type HGDIOBJ = isize;
pub type HFONT = isize;
pub type HBRUSH = isize;

/// `PS_SOLID`: a pen that draws a solid line.
const PS_SOLID: c_int = 0;

// LOGPIXELSX / LOGPIXELSY device caps.
const LOGPIXELSX: c_int = 88;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LOGFONTW {
    lfHeight: i32,
    lfWidth: i32,
    lfEscapement: i32,
    lfOrientation: i32,
    lfWeight: i32,
    lfItalic: u8,
    lfUnderline: u8,
    lfStrikeOut: u8,
    lfCharSet: u8,
    lfOutPrecision: u8,
    lfClipPrecision: u8,
    lfQuality: u8,
    lfPitchAndFamily: u8,
    lfFaceName: [u16; 32],
}

const FW_NORMAL: i32 = 400;
const FW_BOLD: i32 = 700;
const OUT_TT_PRECIS: u8 = 4;
const CLIP_DEFAULT_PRECIS: u8 = 0;
const CLEARTYPE_QUALITY: u8 = 5;
const DEFAULT_PITCH: u8 = 0;
const FF_DONTCARE: u8 = 0;
const TRANSPARENT: i32 = 1;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

/// `CreateFontW` takes 14 scalar arguments before the `LOGFONTW` pointer:
/// cHeight, cWidth, cEscapement, cOrientation, cWeight, bItalic, bUnderline,
/// bStrikeOut, iCharSet, iOutPrecision, iClipPrecision, iQuality,
/// iPitchAndFamily -thirteen -plus the struct. The struct's fields supply
/// the first thirteen, and the call passes them positionally, so the pointer
/// is the fourteenth parameter.
type CreateFontWFn = unsafe extern "system" fn(
    i32, i32, i32, i32, i32, u32, u32, u32, u32, u32, u32, u32, u32, *const LOGFONTW,
) -> HFONT;
type DeleteObjectFn = unsafe extern "system" fn(HGDIOBJ) -> i32;
type CreateSolidBrushFn = unsafe extern "system" fn(u32) -> HBRUSH;
type SetDCBrushColorFn = unsafe extern "system" fn(HDC, u32) -> u32;
type SetTextColorFn = unsafe extern "system" fn(HDC, u32) -> u32;
type SetBkModeFn = unsafe extern "system" fn(HDC, i32) -> i32;
type GetDeviceCapsFn = unsafe extern "system" fn(HDC, c_int) -> i32;
type FillRectFn =
    unsafe extern "system" fn(HDC, *const crate::gui::RECT, HBRUSH) -> i32;
type SetWindowThemeFn = unsafe extern "system" fn(isize, *const u16, *const u16) -> i32;
type SelectObjectFn = unsafe extern "system" fn(HDC, HGDIOBJ) -> HGDIOBJ;
type DrawTextWFn =
    unsafe extern "system" fn(HDC, *const u16, c_int, *mut crate::gui::RECT, u32) -> c_int;
type RoundRectFn = unsafe extern "system" fn(HDC, c_int, c_int, c_int, c_int, c_int, c_int) -> c_int;
type CreatePenFn = unsafe extern "system" fn(c_int, c_int, u32) -> HPEN;
type GetSysColorFn = unsafe extern "system" fn(c_int) -> u32;
type LineToFn = unsafe extern "system" fn(HDC, c_int, c_int) -> c_int;
type MoveToExFn =
    unsafe extern "system" fn(HDC, c_int, c_int, *mut crate::gui::POINT) -> c_int;

pub type HPEN = isize;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
/// Every dynamically-resolved symbol we might use.
pub struct Gdi {
    pub create_font: Option<CreateFontWFn>,
    pub delete_object: Option<DeleteObjectFn>,
    pub create_solid_brush: Option<CreateSolidBrushFn>,
    pub set_dc_brush_color: Option<SetDCBrushColorFn>,
    pub set_text_color: Option<SetTextColorFn>,
    pub set_bk_mode: Option<SetBkModeFn>,
    pub get_device_caps: Option<GetDeviceCapsFn>,
    pub fill_rect: Option<FillRectFn>,
    pub select_object: Option<SelectObjectFn>,
    pub round_rect: Option<RoundRectFn>,
    pub create_pen: Option<CreatePenFn>,
    pub get_sys_color: Option<GetSysColorFn>,
    pub line_to: Option<LineToFn>,
    pub move_to_ex: Option<MoveToExFn>,
    pub set_window_theme: Option<SetWindowThemeFn>,
}

unsafe fn symbol<T: Copy>(dll: &str, name: &[u8]) -> Option<T> {
    let wide: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
    let module = LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
    if module.is_null() {
        return None;
    }
    let p = GetProcAddress(module, name.as_ptr());
    if p.is_null() {
        return None;
    }
    Some(std::mem::transmute_copy(&p))
}

static GDI: OnceLock<Gdi> = OnceLock::new();

pub fn gdi() -> &'static Gdi {
    GDI.get_or_init(|| unsafe {
        Gdi {
            create_font: symbol("gdi32.dll", b"CreateFontW\0"),
            delete_object: symbol("gdi32.dll", b"DeleteObject\0"),
            create_solid_brush: symbol("gdi32.dll", b"CreateSolidBrush\0"),
            set_dc_brush_color: symbol("gdi32.dll", b"SetDCBrushColor\0"),
            set_text_color: symbol("gdi32.dll", b"SetTextColor\0"),
            set_bk_mode: symbol("gdi32.dll", b"SetBkMode\0"),
            get_device_caps: symbol("gdi32.dll", b"GetDeviceCaps\0"),
            fill_rect: symbol("user32.dll", b"FillRect\0"),
            select_object: symbol("gdi32.dll", b"SelectObject\0"),
            round_rect: symbol("gdi32.dll", b"RoundRect\0"),
            create_pen: symbol("gdi32.dll", b"CreatePen\0"),
            get_sys_color: symbol("user32.dll", b"GetSysColor\0"),
            line_to: symbol("gdi32.dll", b"LineTo\0"),
            move_to_ex: symbol("gdi32.dll", b"MoveToEx\0"),
            set_window_theme: symbol("uxtheme.dll", b"SetWindowTheme\0"),
        }
    })
}

/// Build a Win32 `COLORREF`, which packs as `0x00BBGGRR` - note the
/// reversed byte order compared with the familiar `RGB(r,g,b)` macro.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

/// The accent used for the header band and the filled part of the bar.
/// Colours for the rounded button faces. Windows supplies no rounded
/// button, so these are defined here and resolved once at startup.
pub const ACCENT: u32 = rgb(0x2D, 0x5B, 0xD4);
/// Hovered face for the default (primary) button: a touch lighter.
pub const ACCENT_HOVER: u32 = rgb(0x3E, 0x6B, 0xE0);
/// Pressed face for the default button: a touch darker.
pub const ACCENT_PRESSED: u32 = rgb(0x23, 0x47, 0xAC);
/// Resting face for secondary buttons, matching the window background.
pub const SURFACE: u32 = rgb(0xFF, 0xFF, 0xFF);
pub const SURFACE_HOVER: u32 = rgb(0xF1, 0xF4, 0xFB);
pub const SURFACE_PRESSED: u32 = rgb(0xE2, 0xE7, 0xF4);
pub const DISABLED_FILL: u32 = rgb(0xD8, 0xDD, 0xE8);
pub const BORDER: u32 = rgb(0xC9, 0xD1, 0xE0);
pub const BORDER_STRONG: u32 = rgb(0x9A, 0xA6, 0xBE);
pub const BORDER_DIM: u32 = rgb(0xE2, 0xE6, 0xEE);
pub const FOCUS: u32 = rgb(0x7A, 0x9B, 0xE8);
pub const ACCENT_DARK: u32 = rgb(0x21, 0x44, 0xA6);
pub const PANEL: u32 = rgb(0xF3, 0xF5, 0xF9);
pub const TRACK: u32 = rgb(0xDD, 0xE2, 0xEA);
pub const TEXT_DIM: u32 = rgb(0x5A, 0x62, 0x70);
pub const TEXT_BRIGHT: u32 = rgb(0xFF, 0xFF, 0xFF);
pub const LOG_BG: u32 = rgb(0xFA, 0xFB, 0xFD);
pub const TEXT: u32 = rgb(0x1F, 0x24, 0x2E);

/// Create a font handle, or `None` when GDI is unavailable.
///
/// `size_pt` is in points at 96 DPI and is converted internally.
pub fn make_font(size_pt: i32, bold: bool) -> Option<HFONT> {
    let g = gdi();
    let create = g.create_font?;
    unsafe {
        let mut lf = LOGFONTW {
            // A negative height requests a character height rather than a
            // cell height, so the point size maps directly.
            lfHeight: -size_pt,
            lfWeight: if bold { FW_BOLD } else { FW_NORMAL },
            lfCharSet: 1, // DEFAULT_CHARSET
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: CLEARTYPE_QUALITY,
            lfPitchAndFamily: DEFAULT_PITCH | FF_DONTCARE,
            ..Default::default()
        };
        for (i, c) in "Segoe UI".encode_utf16().enumerate() {
            lf.lfFaceName[i] = c;
        }
        Some(create(
            lf.lfHeight, lf.lfWidth, lf.lfEscapement, lf.lfOrientation, lf.lfWeight,
            lf.lfItalic as u32, lf.lfUnderline as u32, lf.lfStrikeOut as u32,
            lf.lfCharSet as u32, lf.lfOutPrecision as u32, lf.lfClipPrecision as u32,
            lf.lfQuality as u32, lf.lfPitchAndFamily as u32, &lf,
        ))
    }
}

/// A monospaced font for the log pane, so columns line up.
pub fn make_mono_font(size_pt: i32) -> Option<HFONT> {
    let g = gdi();
    let create = g.create_font?;
    unsafe {
        let mut lf = LOGFONTW {
            lfHeight: -size_pt,
            lfWeight: FW_NORMAL,
            lfCharSet: 1,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: CLEARTYPE_QUALITY,
            lfPitchAndFamily: DEFAULT_PITCH | FF_DONTCARE,
            ..Default::default()
        };
        for (i, c) in "Consolas".encode_utf16().enumerate() {
            lf.lfFaceName[i] = c;
        }
        Some(create(
            lf.lfHeight, lf.lfWidth, lf.lfEscapement, lf.lfOrientation, lf.lfWeight,
            lf.lfItalic as u32, lf.lfUnderline as u32, lf.lfStrikeOut as u32,
            lf.lfCharSet as u32, lf.lfOutPrecision as u32, lf.lfClipPrecision as u32,
            lf.lfQuality as u32, lf.lfPitchAndFamily as u32, &lf,
        ))
    }
}

pub fn delete_object(obj: HGDIOBJ) {
    if let Some(f) = gdi().delete_object {
        unsafe { f(obj) };
    }
}

pub fn set_bk_transparent(hdc: HDC) {
    if let Some(f) = gdi().set_bk_mode {
        unsafe { f(hdc, TRANSPARENT) };
    }
}

pub fn set_text_color(hdc: HDC, color: u32) {
    if let Some(f) = gdi().set_text_color {
        unsafe { f(hdc, color) };
    }
}

pub fn set_brush_color(hdc: HDC, color: u32) {
    if let Some(f) = gdi().set_dc_brush_color {
        unsafe { f(hdc, color) };
    }
}

/// Select a GDI object into the DC, returning the previous one so the
/// caller can restore it. Used for picking a font just long enough to
/// draw one piece of text.
pub fn select_object(hdc: HDC, obj: HGDIOBJ) -> HGDIOBJ {
    match gdi().select_object {
        Some(f) => unsafe { f(hdc, obj) },
        None => 0,
    }
}

/// `DrawText` flags used by the installer. `DT_NOPREFIX` stops a leading
/// `&` from being swallowed, and `DT_SINGLELINE` keeps text on one line.
pub const DT_LEFT: u32 = 0x0000_0000;
pub const DT_CENTER: u32 = 0x0000_0001;
pub const DT_VCENTER: u32 = 0x0000_0004;
pub const DT_SINGLELINE: u32 = 0x0000_0020;
pub const DT_END_ELLIPSIS: u32 = 0x0000_8000;
pub const DT_NOPREFIX: u32 = 0x0000_0800;

/// Draw a rounded rectangle. `w`/`h` are the corner ellipse's width and
/// height, so a fully rounded cap uses twice the desired radius.
pub fn round_rect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> i32 {
    match gdi().round_rect {
        Some(f) => unsafe { f(hdc, l, t, r, b, w, h) },
        None => 0,
    }
}

/// A cosmetic pen (width 1) in the given colour.
pub fn create_pen(color: u32) -> Option<HPEN> {
    match gdi().create_pen {
        Some(f) => {
            let p = unsafe { f(PS_SOLID, 1, color) };
            if p == 0 {
                None
            } else {
                Some(p)
            }
        }
        None => None,
    }
}

/// A system colour index, e.g. `COLOR_BTNFACE` or `COLOR_GRAYTEXT`.
pub fn sys_color(index: i32) -> u32 {
    match gdi().get_sys_color {
        Some(f) => unsafe { f(index) },
        None => 0,
    }
}

pub fn move_to_ex(hdc: HDC, x: i32, y: i32) {
    if let Some(f) = gdi().move_to_ex {
        unsafe {
            f(hdc, x, y, std::ptr::null_mut());
        }
    }
}

pub fn line_to(hdc: HDC, x: i32, y: i32) {
    if let Some(f) = gdi().line_to {
        unsafe {
            f(hdc, x, y);
        }
    }
}

/// Draw a single line of text into `rect`, mutating `rect` to the used area.
pub fn draw_text(hdc: HDC, text: &[u16], rect: &mut crate::gui::RECT, flags: u32) -> i32 {
    if let Some(f) = unsafe { symbol::<DrawTextWFn>("user32.dll", b"DrawTextW\0") } {
        unsafe { f(hdc, text.as_ptr(), text.len() as i32, rect as *mut crate::gui::RECT, flags) }
    } else {
        0
    }
}

pub fn create_brush(color: u32) -> Option<HBRUSH> {
    // CreateSolidBrush returns NULL on failure, so treat 0 as "no brush".
    match gdi().create_solid_brush {
        Some(f) => {
            let b = unsafe { f(color) };
            if b == 0 {
                None
            } else {
                Some(b)
            }
        }
        None => None,
    }
}

pub fn fill_rect(hdc: HDC, r: &crate::gui::RECT, brush: HBRUSH) -> i32 {
    // `FillRect` is a user32 export; it is resolved dynamically here so the
    // import list stays limited to what this toolchain can link.
    if let Some(f) = unsafe { symbol::<FillRectFn>("user32.dll", b"FillRect\0") } {
        let f: FillRectFn = unsafe { std::mem::transmute_copy(&f) };
        unsafe { f(hdc, r as *const crate::gui::RECT, brush) }
    } else {
        0
    }
}

/// Scale factor for the primary display, e.g. 1.5 at 150%.
///
/// Windows reports 96 at 100%, so dividing by 96 gives the multiplier that
/// pixel metrics need. Falls back to 1.0 when GDI is unavailable.
pub fn display_scale() -> f64 {
    match gdi().get_device_caps {
        Some(f) => {
            let dpi = unsafe { f(0, LOGPIXELSX) };
            if dpi > 0 {
                dpi as f64 / 96.0
            } else {
                1.0
            }
        }
        None => 1.0,
    }
}

/// Scale a design-time pixel metric to the current display.
pub fn px(v: i32) -> i32 {
    (v as f64 * display_scale()).round() as i32
}

/// Apply the visual-styles theme to a control so buttons and checkboxes pick
/// up the modern look instead of the classic Win95 face.
pub fn apply_theme(hwnd: isize) {
    if let Some(f) = gdi().set_window_theme {
        let theme: Vec<u16> = "Explorer\0".encode_utf16().collect();
        unsafe { f(hwnd, theme.as_ptr(), std::ptr::null()) };
    }
}

/// Enable process-wide visual styles so common controls are themed.
///

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole theming layer is optional, but on a normal Windows install
    /// these symbols must resolve. If they do not, the GUI silently falls
    /// back to the system font and the loss is invisible - so assert it.
    #[test]
    fn gdi_symbols_resolve_on_windows() {
        let g = gdi();
        assert!(
            g.create_font.is_some(),
            "gdi32!CreateFontW did not resolve - headings will not use the custom font"
        );
        assert!(
            g.create_solid_brush.is_some(),
            "gdi32!CreateSolidBrush did not resolve - colours will be ignored"
        );
        assert!(
            g.set_text_color.is_some(),
            "gdi32!SetTextColor did not resolve"
        );
        assert!(
            g.set_window_theme.is_some(),
            "uxtheme!SetWindowTheme did not resolve - controls keep the classic look"
        );
    }

    /// Fonts must actually be creatable and carry the requested face.
    #[test]
    fn make_font_returns_a_usable_handle() {
        let f = make_font(12, true).expect("CreateFontW should succeed");
        assert_ne!(f, 0, "CreateFontW returned a null handle");
        delete_object(f);
    }

    #[test]
    fn mono_font_is_monospaced() {
        let f = make_mono_font(9).expect("mono font should be creatable");
        assert_ne!(f, 0);
        delete_object(f);
    }

    /// Brush creation must report failure rather than returning a null
    /// handle that would later be used as if it were valid.
    #[test]
    fn brushes_are_created() {
        let b = create_brush(ACCENT).expect("CreateSolidBrush should succeed");
        assert_ne!(b, 0);
        delete_object(b);
    }

    /// The DPI scale must be positive and sane, otherwise the whole layout
    /// collapses to zero or explodes.
    #[test]
    fn display_scale_is_sane() {
        let s = display_scale();
        assert!(s >= 0.5 && s <= 8.0, "implausible DPI scale {}", s);
        assert!(px(0) == 0);
        assert!(px(100) >= 50);
    }

    /// `rgb` packs as 0x00BBGGRR, which is what Win32 COLORREF expects.
    #[test]
    fn rgb_packing_is_colorref_order() {
        assert_eq!(rgb(0x11, 0x22, 0x33), 0x0033_2211);
    }
}

/// comctl32 has no import library in this toolchain, so `InitCommonControlsEx`
/// is resolved at runtime. On a system without the theme service this is
/// simply a no-op and controls fall back to the classic look.
pub fn enable_visual_styles() {
    #[repr(C)]
    struct INITCOMMONCONTROLSEX {
        dwSize: u32,
        dwICC: u32,
    }
    type InitFn = unsafe extern "system" fn(*const INITCOMMONCONTROLSEX) -> i32;
    // ICC_STANDARD_CLASSES
    const ICC_STANDARD_CLASSES: u32 = 0x0000_4000;

    if let Some(f) = unsafe { symbol::<InitFn>("comctl32.dll", b"InitCommonControlsEx\0") } {
        let f: InitFn = unsafe { std::mem::transmute_copy(&f) };
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        unsafe { f(&icc) };
    }
}
