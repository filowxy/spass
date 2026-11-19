//! Implements the installer's native Win32 GUI.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::os::raw::c_int;
use std::sync::{Arc, Mutex};

use crate::theme;
use crate::ui::Reporter;
use crate::Scope;

// ---------------------------------------------------------------------------
// Win32 types
// ---------------------------------------------------------------------------

pub type HWND = isize;
pub type HINSTANCE = isize;
pub type HBRUSH = isize;
pub type HCURSOR = isize;
pub type HICON = isize;
pub type HDC = isize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type WPARAM = usize;
pub type UINT = u32;
#[allow(non_camel_case_types)]
pub type BOOL_I = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RECT {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// Minimal `PAINTSTRUCT`. Field order and sizes match Win32 on x64; only
/// `hdc` and `fErase` are read.
#[repr(C)]
struct PAINTSTRUCT {
    hdc: HDC,
    fErase: BOOL_I,
    rcPaint: RECT,
    fRestore: BOOL_I,
    fIncUpdate: BOOL_I,
    rgbReserved: [u8; 32],
}

const TRUE_: BOOL_I = 1;
const FALSE_: BOOL_I = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct POINT {
    x: c_int,
    y: c_int,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    time: UINT,
    pt: POINT,
}

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: UINT,
    style: UINT,
    lpfnWndProc: unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT,
    cbClsExtra: c_int,
    cbWndExtra: c_int,
    hInstance: HINSTANCE,
    hIcon: HICON,
    hCursor: HCURSOR,
    hbrBackground: HBRUSH,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HICON,
}

// Window styles
const WS_OVERLAPPED: u32 = 0x0000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_OVERLAPPEDWINDOW: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

// Control styles
const BS_PUSHBUTTON: u32 = 0x0000_0000;
const BS_DEFPUSHBUTTON: u32 = 0x0000_0001;
/// Mask that isolates the button *type* from the rest of the `BS_*` state.
const BS_TYPEMASK: u32 = 0x0000_000F;
const BS_AUTOCHECKBOX: u32 = 0x0000_0003;
// Standard button styles. All controls are drawn by Windows itself: an
// owner-drawn approach was tried and repeatedly broke (wrong DRAWITEMSTRUCT
// layout, wrong ODT_* value, wrong font pointer), leaving blank grey boxes.
// Native controls cannot fail to paint.
const BS_PUSHBUTTON_N: u32 = 0x0000_0000;
const BS_AUTOCHECKBOX_N: u32 = 0x0000_0003;
const BS_AUTORADIOBUTTON_N: u32 = 0x0000_0009;
const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_READONLY: u32 = 0x0800;
const SS_LEFT: u32 = 0x0000_0000;
const SS_ENDELLIPSIS: u32 = 0x2000_0000;
const SS_SUNKEN: u32 = 0x0000_0003;
const SS_CENTERIMAGE: u32 = 0x0000_0200;

// Edit / button messages
const EM_SETSEL: u32 = 0x00B1;
const EM_REPLACESEL: u32 = 0x00C2;
const EM_SCROLLCARET: u32 = 0x00B7;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;

// Window messages
const WM_DESTROY: u32 = 0x0002;
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;
const WM_PAINT: u32 = 0x000F;
/// `TrackMouseEvent` request, used to learn when the pointer leaves a
/// custom-painted button so the hover highlight can be dropped.
#[repr(C)]
struct TRACKMOUSEEVENT {
    cb_size: u32,
    dw_flags: u32,
    hwnd_track: HWND,
    dw_hover_time: u32,
}

/// `WM_DRAWITEM`'s payload. Field order, types and x64 padding are fixed by
/// the SDK - every field must be present or the ones after it are read from
/// the wrong offset and the owner-draw callback paints into a garbage HDC.
#[repr(C)]
struct DRAWITEMSTRUCT {
    ctl_type: UINT,      // CtlType
    ctl_id: UINT,        // CtlID - the control id we look the button up by
    item_id: UINT,       // itemID
    item_action: UINT,   // itemAction
    item_state: UINT,    // itemState (ODS_* flags)
    hwnd_item: HWND,     // hwndItem
    hdc: HDC,            // hDC
    rc_item: RECT,       // rcItem
    item_data: usize,    // itemData
}

#[cfg(test)]
mod drawitem_layout_tests {
    use super::*;

    /// The field offsets are fixed by the Windows SDK. Getting one wrong
    /// makes owner-draw paint into a garbage HDC and every button degrades
    /// to a flat grey system square, so pin all offsets down.
    #[test]
    #[cfg(windows)]
    fn drawitem_struct_field_offsets() {
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, ctl_type), 0);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, ctl_id), 4);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, item_id), 8);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, item_action), 12);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, item_state), 16);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, hwnd_item), 24);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, hdc), 32);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, rc_item), 40);
        assert_eq!(std::mem::offset_of!(DRAWITEMSTRUCT, item_data), 56);
        assert_eq!(std::mem::size_of::<DRAWITEMSTRUCT>(), 64);
    }
}

/// `ODS_*` flags carried in `DRAWITEMSTRUCT::itemState` (Winuser.h).
const ODS_FOCUS: u32 = 0x0000_0010;
const ODS_SELECTED: u32 = 0x0000_0001;
const ODS_DISABLED: u32 = 0x0000_0004;
const ODS_HOTLIGHT: u32 = 0x0000_0040;
/// `DRAWITEMSTRUCT::CtlType` for a button (Winuser.h: ODT_BUTTON == 4).
/// This was wrongly 1 (ODT_MENU), which made the owner-draw branch never run
/// and left every button as a flat system-grey square.
const ODT_BUTTON: u32 = 4;

/// `TME_LEAVE`
const TME_LEAVE: u32 = 0x0000_0002;

const WM_DRAWITEM: u32 = 0x02B1;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_ENABLE: u32 = 0x0009;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_CTLCOLORSTATIC: u32 = 0x0138;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_CTLCOLORBTN: u32 = 0x0139;
const WM_NCHITTEST: u32 = 0x0084;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_APP: u32 = 0x8000;
const WM_SETFONT: u32 = 0x0030;
const WM_GETFONT: u32 = 0x0031;
const WM_SETTEXT: u32 = 0x000C;
const WM_GETTEXT: u32 = 0x000D;
const WM_GETTEXTLENGTH: u32 = 0x000E;

// Custom messages: worker -> UI thread.
const WM_APP_LOG: u32 = WM_APP + 1;
const WM_APP_STATUS: u32 = WM_APP + 2;
const WM_APP_PROGRESS: u32 = WM_APP + 3;
const WM_APP_DONE: u32 = WM_APP + 4;

const BST_CHECKED: WPARAM = 0x0001;
/// `BM_GETCHECK` returns the state in the `LRESULT`, so the comparison
/// constant needs that type as well as the `WPARAM` used by `BM_SETCHECK`.
const BST_CHECKED_L: LRESULT = 0x0001;

const SW_SHOW: c_int = 5;
const CW_USEDEFAULT: c_int = -2147483648;

const MB_YESNO: u32 = 0x0000_0004;
const MB_ICONQUESTION: u32 = 0x0000_0020;
const MB_ICONINFORMATION: u32 = 0x0000_0040;
const IDYES: c_int = 6;

const GWLP_USERDATA: c_int = -21;
const GWL_STYLE: c_int = -16;
/// The window-procedure slot. Subclassing a control means replacing this.
const GWL_WNDPROC: c_int = -4;
const IDC_ARROW: usize = 32512;

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        dwExStyle: u32,
        lpclassname: *const u16,
        lpwindowname: *const u16,
        dwstyle: u32,
        x: c_int,
        y: c_int,
        nwidth: c_int,
        nheight: c_int,
        hwndparent: HWND,
        hmenu: isize,
        hinstance: HINSTANCE,
        lparam: *mut c_void,
    ) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn DestroyWindow(hwnd: HWND) -> BOOL_I;
    fn ShowWindow(hwnd: HWND, cmd: c_int) -> BOOL_I;
    fn UpdateWindow(hwnd: HWND) -> BOOL_I;
    fn GetMessageW(lpmsg: *mut MSG, hwnd: HWND, min: UINT, max: UINT) -> BOOL_I;
    fn TranslateMessage(lpmsg: *mut MSG) -> BOOL_I;
    fn DispatchMessageW(lpmsg: *const MSG) -> LRESULT;
    fn PostQuitMessage(exitcode: c_int);
    fn PostMessageW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> BOOL_I;
    fn SendMessageW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn SetWindowTextW(hwnd: HWND, text: *const u16) -> BOOL_I;
    fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: c_int) -> c_int;
    fn GetWindowTextLengthW(hwnd: HWND) -> c_int;
    fn EnableWindow(hwnd: HWND, enable: BOOL_I) -> BOOL_I;
    fn SetFocus(hwnd: HWND) -> HWND;
    fn LoadCursorW(hinstance: HINSTANCE, name: *const u16) -> HCURSOR;
    fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, flags: u32) -> c_int;
    fn SetWindowLongPtrW(hwnd: HWND, index: c_int, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: HWND, index: c_int) -> isize;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> BOOL_I;
    fn CallWindowProcW(
        prev: unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT,
        hwnd: HWND,
        msg: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT;
    fn IsWindowEnabled(hwnd: HWND) -> BOOL_I;
    fn GetFocus() -> HWND;
    fn TrackMouseEvent(tme: *mut TRACKMOUSEEVENT) -> BOOL_I;
    fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL_I;
    fn MoveWindow(hwnd: HWND, x: c_int, y: c_int, w: c_int, h: c_int, repaint: BOOL_I) -> BOOL_I;
    fn BeginPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> HDC;
    fn EndPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> BOOL_I;
    fn FillRect(hdc: HDC, rect: *const RECT, brush: HBRUSH) -> BOOL_I;
    fn GetSysColor(index: c_int) -> u32;
    fn InvalidateRect(hwnd: HWND, rect: *const RECT, erase: BOOL_I) -> BOOL_I;
    fn GetDlgCtrlID(hwnd: HWND) -> c_int;
    fn GetDlgItem(hwnd: HWND, id: c_int) -> HWND;
    fn ClientToScreen(hwnd: HWND, pt: *mut POINT) -> BOOL_I;
    fn GetSysColorBrush(index: c_int) -> HBRUSH;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the W APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn set_text(hwnd: HWND, s: &str) {
    let buf = wide(s);
    SetWindowTextW(hwnd, buf.as_ptr());
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), (len + 1) as c_int);
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe fn message_box(hwnd: HWND, text: &str, caption: &str, flags: u32) -> bool {
    let t = wide(text);
    let c = wide(caption);
    MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), flags) == IDYES
}

/// Append one line to a read-only multiline `EDIT` and scroll to it.
unsafe fn append_line(hwnd: HWND, line: &str) {
    let mut text = String::with_capacity(line.len() + 2);
    text.push_str(line);
    text.push_str("\r\n");
    let buf = wide(&text);
    // EM_SETSEL with -1/-1 moves the caret to the end; EM_REPLACESEL then
    // appends. Selection is restored to the end so the view follows.
    SendMessageW(hwnd, EM_SETSEL, usize::MAX, LPARAM::MAX);
    SendMessageW(hwnd, EM_REPLACESEL, 1, buf.as_ptr() as LPARAM);
    SendMessageW(hwnd, EM_SETSEL, usize::MAX, LPARAM::MAX);
    SendMessageW(hwnd, EM_SCROLLCARET, 0, 0);
}

// ---------------------------------------------------------------------------
// Reporter: forwards worker-thread output to the window
// ---------------------------------------------------------------------------

/// A [`Reporter`] that posts to the window instead of printing.
///
/// String payloads are heap-boxed and their pointer moved into `lparam`;
/// the UI thread takes ownership back and frees them. That is the standard
/// way to carry a `String` across threads through a raw message.
pub struct GuiReporter {
    hwnd: Mutex<HWND>,
}

impl GuiReporter {
    /// A reporter with no window attached yet. The handle is published by
    /// [`run`] once the window exists; messages posted before then are
    /// dropped rather than crashing.
    pub fn new() -> Self {
        GuiReporter { hwnd: Mutex::new(0) }
    }

    fn post(&self, msg: UINT, payload: Box<String>) {
        let hwnd = *self.hwnd.lock().unwrap();
        if hwnd == 0 {
            return;
        }
        let ptr = Box::into_raw(payload) as LPARAM;
        if unsafe { PostMessageW(hwnd, msg, 0, ptr) } == FALSE_ {
            // The window is gone; reclaim the allocation ourselves.
            drop(unsafe { Box::from_raw(ptr as *mut String) });
        }
    }

    fn post_progress(&self, done: u64, total: u64) {
        let hwnd = *self.hwnd.lock().unwrap();
        if hwnd == 0 {
            return;
        }
        unsafe { PostMessageW(hwnd, WM_APP_PROGRESS, done as usize, total as LPARAM) };
    }
}

impl Reporter for GuiReporter {
    fn step(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(format!("== {}", msg)));
    }
    fn ok(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(format!("    {}", msg)));
    }
    fn note(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(format!("    {}", msg)));
    }
    fn error(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(format!("!! {}", msg)));
    }
    fn finish(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(String::new()));
        self.post(WM_APP_LOG, Box::new(msg.to_string()));
    }
    fn plain(&self, msg: &str) {
        self.post(WM_APP_LOG, Box::new(msg.to_string()));
    }
    fn confirm(&self, prompt: &str) -> bool {
        // Only reached if a question is deferred onto the worker; the GUI
        // asks its own questions up front, so this is a safety net.
        let hwnd = *self.hwnd.lock().unwrap();
        unsafe { message_box(hwnd, prompt, "Spass Setup", MB_YESNO | MB_ICONQUESTION) }
    }
    fn progress(&self, done: u64, total: u64) {
        self.post_progress(done, total);
    }
}

// ---------------------------------------------------------------------------
// Window state
// ---------------------------------------------------------------------------

// Control identifiers.
const ID_INSTALL: isize = 100;
const ID_UNINSTALL: isize = 101;
const ID_CLOSE: isize = 102;
const ID_PATH: isize = 103;
const ID_ADDPATH: isize = 104;
const ID_RUST: isize = 105;
const ID_LOG: isize = 106;
const ID_STATUS: isize = 107;
const ID_TRACK: isize = 108;
const ID_DEFAULT: isize = 110;
const ID_TITLE: isize = 111;
const ID_SUBTITLE: isize = 112;
const ID_SECTION_LOC: isize = 113;
const ID_SECTION_OPT: isize = 114;
const ID_SECTION_LOG: isize = 115;
const ID_SECTION_PROG: isize = 117;
const ID_RUST_NOTE: isize = 116;
// Scope selector cards.
const ID_SCOPE_SECTION: isize = 118;
const ID_SCOPE_USER: isize = 119;
const ID_SCOPE_MACHINE: isize = 120;
const ID_SCOPE_NOTE: isize = 121;
const ID_VERSION: isize = 122;

/// GDI brushes shared by the window.
///
/// They live in a `OnceLock` rather than only in [`Ui`] because
/// `WM_CTLCOLOR*` is sent while the child controls are being created, before
/// `GWLP_USERDATA` is published. Reading them through a process-wide slot
/// means the first paint of every control already gets the right colour.
struct Brushes {
    accent: isize,
    accent_dark: isize,
    panel: isize,
    track: isize,
    log: isize,
    soft: isize,
}

static BRUSHES: OnceLock<Brushes> = OnceLock::new();

/// Everything the window owns. Reachable from the WndProc through
/// `GWLP_USERDATA`, since Win32 gives callbacks no user pointer.
struct Ui {
    hwnd: HWND,
    log: HWND,
    status: HWND,
    btn_install: HWND,
    btn_uninstall: HWND,
    path: HWND,
    chk_path: HWND,
    chk_rust: HWND,
    scope_user: HWND,
    scope_machine: HWND,
    /// Installation scope chosen in the window. Mirrors the two radio cards.
    scope: Scope,
    /// Inherited from the command line: true when this process is already
    /// elevated and must not ask for UAC again.
    elevated: bool,
    /// True when the window should kick off the install on its own (the
    /// elevated relaunch uses this so the user never clicks twice).
    autostart: bool,
    busy: bool,
    /// Filled fraction of the progress bar, 0.0..=1.0.
    progress: f64,
    /// True once a fraction of the bar should be drawn.
    progress_active: bool,
    /// Where the progress bar lives, in our client coordinates. The parent
    /// paints this strip itself; there is deliberately no child control
    /// covering it, because a child would clip the parent's paint out.
    track_rect: RECT,
    /// The accent strip at the top, and its inner padding.
    banner_rect: RECT,
    banner_pad: i32,
    /// Every owner-drawn button, in client coordinates, so the parent can
    /// hit-test the pointer for the hover highlight. The stock button only
    /// reports `ODS_HOTLIGHT` when comctl32 visual styles are active, and
    /// we deliberately do not depend on them.
    buttons: Vec<BtnBox>,

    // Fonts created once at startup and released with the window.
    f_title: isize,
    f_sub: isize,
    f_body: isize,
    f_section: isize,
    f_mono: isize,
}

/// A button, as the parent knows it.
///
/// The parent keeps its own copy of everything the painter needs -- caption,
/// font, checked state, pointer position -- so `WM_DRAWITEM` is pure
/// painting with no messages sent back into the control. That matters: a
/// `SendMessage` from inside `WM_DRAWITEM` back to the child that is
/// currently being created is a deadlock, and asking a button for its own
/// text is exactly that shape of mistake.
#[derive(Clone)]
struct BtnBox {
    id: isize,
    rect: RECT,
    caption: Vec<u16>,
    font: isize,
    checked: bool,
    /// True while the pointer is inside the button.
    hovered: bool,
}

impl Ui {
    unsafe fn from_hwnd(hwnd: HWND) -> Option<&'static mut Ui> {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if p == 0 {
            None
        } else {
            Some(&mut *(p as *mut Ui))
        }
    }

    /// Enable or disable every input while work is in flight.
    unsafe fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        for h in [
            self.btn_install, self.btn_uninstall, self.path, self.chk_path, self.chk_rust,
            self.scope_user, self.scope_machine,
        ] {
            EnableWindow(h, if busy { FALSE_ } else { TRUE_ });
        }
    }

    /// Update the bar and repaint just that strip.
    unsafe fn set_progress(&mut self, fraction: f64) {
        self.progress = fraction.clamp(0.0, 1.0);
        self.progress_active = true;
        // Repaint *our* strip in the parent. Invalidating a child control
        // would never reach the parent's WM_PAINT, which is what draws the
        // bar, so the bar would simply not update.
        InvalidateRect(self.hwnd, &self.track_rect, TRUE_);
    }

    unsafe fn status(&mut self, text: &str) {
        set_text(self.status, text);
    }

    unsafe fn log_line(&mut self, line: &str) {
        append_line(self.log, line);
    }
}

// ---------------------------------------------------------------------------
// Rounded buttons (owner-drawn)
// ---------------------------------------------------------------------------
//
// Windows has no rounded button, and this toolchain has no import library
// for comctl32's themed controls, so the buttons are *owner-drawn*: the
// style is BS_OWNERDRAW / BS_OWNERDRAWFIXED and the parent renders each one
// from `WM_DRAWITEM`.
//
// Owner-drawing is used in preference to subclassing the controls and
// swapping their window procedure. Subclassing worked, but it made the
// buttons the most fragile part of the window: a message arriving between
// the `SetWindowLongPtrW` and the bookkeeping sent the control through
// `DefWindowProcW`, which a `BUTTON` class does not tolerate, and window
// creation became intermittent. Owner-draw leaves the stock control's
// behaviour -- focus, keyboard activation, `BN_CLICKED`, auto-toggling
// checkboxes -- completely intact and only changes the pixels.

/// Palette indices, so one parallel pair of brush/pen arrays can serve
/// every role the buttons need.
const C_ACCENT: usize = 0;
const C_ACCENT_HOVER: usize = 1;
const C_ACCENT_PRESSED: usize = 2;
const C_SURFACE: usize = 3;
const C_SURFACE_HOVER: usize = 4;
const C_SURFACE_PRESSED: usize = 5;
const C_DISABLED_FILL: usize = 6;
const C_BORDER: usize = 7;
const C_BORDER_STRONG: usize = 8;
const C_BORDER_DIM: usize = 9;
const C_TEXT: usize = 10;
const C_TEXT_BRIGHT: usize = 11;
const C_TEXT_DIM: usize = 12;
const C_FOCUS: usize = 13;
const C_ON_ACCENT: usize = 14;
const C_ACCENT_SOFT: usize = 15;

const BTN_COLORS: [u32; 16] = [
    theme::ACCENT,          // C_ACCENT
    theme::ACCENT_HOVER,    // C_ACCENT_HOVER
    theme::ACCENT_PRESSED,  // C_ACCENT_PRESSED
    theme::SURFACE,         // C_SURFACE
    theme::SURFACE_HOVER,   // C_SURFACE_HOVER
    theme::SURFACE_PRESSED, // C_SURFACE_PRESSED
    theme::DISABLED_FILL,   // C_DISABLED_FILL
    theme::BORDER,          // C_BORDER
    theme::BORDER_STRONG,   // C_BORDER_STRONG
    theme::BORDER_DIM,      // C_BORDER_DIM
    theme::TEXT,            // C_TEXT
    theme::TEXT_BRIGHT,     // C_TEXT_BRIGHT
    theme::TEXT_DIM,        // C_TEXT_DIM
    theme::FOCUS,           // C_FOCUS
    theme::TEXT_BRIGHT,     // C_ON_ACCENT (the tick on a filled box)
    theme::ACCENT_SOFT,     // C_ACCENT_SOFT (selected scope card)
];

/// Brushes and pens, built once. Allocating them inside `WM_DRAWITEM`
/// would leak a GDI object for every repaint of every button.
struct BtnArt {
    fills: [isize; 16],
    pens: [isize; 16],
}

static BTN_ART: OnceLock<BtnArt> = OnceLock::new();

fn btn_art() -> &'static BtnArt {
    BTN_ART.get_or_init(|| BtnArt {
        fills: BTN_COLORS.map(|c| theme::create_brush(c).unwrap_or(0)),
        pens: BTN_COLORS.map(|c| theme::create_pen(c).unwrap_or(0)),
    })
}

/// The control id of the primary action, which gets the filled accent face.
fn is_primary(id: isize) -> bool {
    id == ID_INSTALL
}

/// The two option rows, which get a tick box instead of a filled face.
fn is_option(id: isize) -> bool {
    id == ID_ADDPATH || id == ID_RUST
}

/// The two scope cards ("Just me" / "All users") get a distinct card face.
fn is_scope_card(id: isize) -> bool {
    id == ID_SCOPE_USER || id == ID_SCOPE_MACHINE
}

/// Read a window's caption into a NUL-terminated UTF-16 buffer.
unsafe fn window_text(hwnd: HWND) -> Vec<u16> {
    let n = GetWindowTextLengthW(hwnd);
    if n <= 0 {
        return Vec::new();
    }
    let mut buf: Vec<u16> = vec![0; (n + 1) as usize];
    GetWindowTextW(hwnd, buf.as_mut_ptr(), n + 1);
    buf
}

/// Fill `rc` as a rounded rectangle: `fill` inside, outlined in `border`.
///
/// `radius` is the corner radius. `RoundRect` wants the corner ellipse's
/// width and height, which is why it is doubled.
unsafe fn fill_round(hdc: HDC, art: &BtnArt, fill: usize, border: usize, rc: &RECT, radius: i32) {
    let old_brush = theme::select_object(hdc, art.fills[fill]);
    let old_pen = theme::select_object(hdc, art.pens[border]);
    theme::round_rect(hdc, rc.left, rc.top, rc.right, rc.bottom, radius * 2, radius * 2);
    theme::select_object(hdc, old_brush);
    theme::select_object(hdc, old_pen);
}

/// Draw `text` into `tr` in `color`. The background is already painted, so
/// the glyphs sit straight on the face.
unsafe fn draw_label(hdc: HDC, text: &[u16], color: u32, tr: &mut RECT, flags: u32) {
    if text.is_empty() {
        return;
    }
    theme::set_text_color(hdc, color);
    theme::set_bk_transparent(hdc);
    theme::draw_text(hdc, text, tr, flags);
}

/// Which face a push button gets: filled for the primary action, flat for the
/// rest. Returns `(fill, border, text)` as palette indices.
fn push_button_colors(id: isize, state: u32) -> (usize, usize, usize) {
    let enabled = state & ODS_DISABLED == 0;
    let pressed = state & ODS_SELECTED != 0;
    let hovered = state & ODS_HOTLIGHT != 0;
    if is_primary(id) {
        let fill = if !enabled {
            C_DISABLED_FILL
        } else if pressed {
            C_ACCENT_PRESSED
        } else if hovered {
            C_ACCENT_HOVER
        } else {
            C_ACCENT
        };
        let text = if enabled { C_TEXT_BRIGHT } else { C_TEXT_DIM };
        (fill, fill, text)
    } else {
        let fill = if pressed {
            C_SURFACE_PRESSED
        } else if hovered {
            C_SURFACE_HOVER
        } else {
            C_SURFACE
        };
        let border = if enabled { C_BORDER } else { C_BORDER_DIM };
        let text = if enabled { C_TEXT } else { C_TEXT_DIM };
        (fill, border, text)
    }
}

/// Paint one owner-drawn button from state the parent already holds.
unsafe fn draw_button(hdc: HDC, art: &BtnArt, b: &BtnBox, state: u32) {
    if b.font != 0 {
        theme::select_object(hdc, b.font);
    }
    if is_scope_card(b.id) {
        draw_scope_card(hdc, art, b, state);
        return;
    }
    if is_option(b.id) {
        draw_option_row(hdc, art, b, state);
        return;
    }

    let (fill, border, text) = push_button_colors(b.id, state);
    fill_round(hdc, art, fill, border, &b.rect, theme::px(7));

    if state & ODS_FOCUS != 0 {
        // Inset so the keyboard cue never collides with the border.
        let inset = theme::px(3);
        let inner = RECT {
            left: b.rect.left + inset,
            top: b.rect.top + inset,
            right: b.rect.right - inset,
            bottom: b.rect.bottom - inset,
        };
        let r = theme::px(4);
        let old_brush = theme::select_object(hdc, art.fills[fill]);
        let old_pen = theme::select_object(hdc, art.pens[C_FOCUS]);
        theme::round_rect(hdc, inner.left, inner.top, inner.right, inner.bottom, r * 2, r * 2);
        theme::select_object(hdc, old_brush);
        theme::select_object(hdc, old_pen);
    }

    let mut tr = b.rect;
    let flags = theme::DT_CENTER | theme::DT_VCENTER | theme::DT_SINGLELINE | theme::DT_NOPREFIX;
    draw_label(hdc, &b.caption, BTN_COLORS[text], &mut tr, flags);
}

/// A scope card: a rounded, bordered panel with a radio dot, a bold title
/// and a caption. The selected card gets an accent border, a soft tint and a
/// filled radio dot; the other stays flat. This is the "who is this for?"
/// choice that used to be missing from the installer.
unsafe fn draw_scope_card(hdc: HDC, art: &BtnArt, b: &BtnBox, state: u32) {
    let enabled = state & ODS_DISABLED == 0;
    let pressed = state & ODS_SELECTED != 0;
    let hovered = state & ODS_HOTLIGHT != 0;
    let selected = b.checked;

    let (fill, border) = if !enabled {
        (C_SURFACE, C_BORDER_DIM)
    } else if selected {
        (C_ACCENT_SOFT, C_ACCENT)
    } else if pressed {
        (C_SURFACE_PRESSED, C_BORDER_STRONG)
    } else if hovered {
        (C_SURFACE_HOVER, C_BORDER_STRONG)
    } else {
        (C_SURFACE, C_BORDER)
    };
    fill_round(hdc, art, fill, border, &b.rect, theme::px(10));

    // Radio dot on the left.
    let d = theme::px(18);
    let dot_left = b.rect.left + theme::px(16);
    let dot_top = b.rect.top + (b.rect.bottom - b.rect.top - d) / 2;
    let dot = RECT { left: dot_left, top: dot_top, right: dot_left + d, bottom: dot_top + d };
    let ring = if enabled { C_BORDER_STRONG } else { C_BORDER_DIM };
    fill_round(hdc, art, C_SURFACE, ring, &dot, d / 2);
    if selected {
        let inner = theme::px(4);
        let core = RECT {
            left: dot.left + inner, top: dot.top + inner,
            right: dot.right - inner, bottom: dot.bottom - inner,
        };
        let f = if enabled { C_ACCENT } else { C_DISABLED_FILL };
        let old_brush = theme::select_object(hdc, art.fills[f]);
        theme::ellipse(hdc, core.left, core.top, core.right, core.bottom);
        theme::select_object(hdc, old_brush);
    }

    // Card title to the right of the dot, vertically centred. The detailed
    // explanation lives in the hint label under both cards.
    let text_left = dot.right + theme::px(14);
    let mut title_rc = RECT {
        left: text_left,
        top: b.rect.top,
        right: b.rect.right - theme::px(14),
        bottom: b.rect.bottom,
    };
    let title_color = if enabled { C_TEXT } else { C_TEXT_DIM };
    draw_label(hdc, &b.caption, BTN_COLORS[title_color], &mut title_rc,
        theme::DT_LEFT | theme::DT_VCENTER | theme::DT_SINGLELINE | theme::DT_NOPREFIX);
}

/// A checkbox row: a rounded tick box on the left, caption to its right.
///
/// The row itself stays flat, so a long option does not read as a slab.
unsafe fn draw_option_row(hdc: HDC, art: &BtnArt, b: &BtnBox, state: u32) {
    let enabled = state & ODS_DISABLED == 0;
    let pressed = state & ODS_SELECTED != 0;
    let hovered = state & ODS_HOTLIGHT != 0;

    let row = if !enabled {
        C_SURFACE
    } else if pressed {
        C_SURFACE_PRESSED
    } else if hovered {
        C_SURFACE_HOVER
    } else {
        C_SURFACE
    };
    theme::fill_rect(hdc, &b.rect, art.fills[row]);

    let side = theme::px(16);
    let left = b.rect.left + theme::px(2);
    let top = b.rect.top + (b.rect.bottom - b.rect.top - side) / 2;
    let box_rc = RECT { left, top, right: left + side, bottom: top + side };

    let (fill, border) = match (b.checked, pressed, enabled) {
        (true, _, false) => (C_DISABLED_FILL, C_BORDER_DIM),
        (true, true, _) => (C_ACCENT_PRESSED, C_ACCENT_PRESSED),
        (true, false, _) => (C_ACCENT, C_ACCENT),
        (false, true, true) => (C_SURFACE_PRESSED, C_BORDER_STRONG),
        (false, _, true) => (C_SURFACE, C_BORDER_STRONG),
        (false, _, false) => (C_SURFACE, C_BORDER_DIM),
    };
    fill_round(hdc, art, fill, border, &box_rc, theme::px(4));

    if b.checked {
        // A two-segment tick drawn with a pen, so it lines up with the
        // rounded outline instead of being a glyph dropped in the middle.
        let old_pen = theme::select_object(hdc, art.pens[C_ON_ACCENT]);
        let pad = side / 4;
        theme::move_to_ex(hdc, box_rc.left + pad, box_rc.top + side / 2);
        theme::line_to(hdc, box_rc.left + side / 2 - pad / 2, box_rc.bottom - pad);
        theme::line_to(hdc, box_rc.right - pad, box_rc.top + pad);
        theme::select_object(hdc, old_pen);
    }

    let mut tr = RECT {
        left: box_rc.right + theme::px(8),
        top: b.rect.top,
        right: b.rect.right,
        bottom: b.rect.bottom,
    };
    let color = if enabled { C_TEXT } else { C_TEXT_DIM };
    let flags = theme::DT_LEFT
        | theme::DT_VCENTER
        | theme::DT_SINGLELINE
        | theme::DT_END_ELLIPSIS
        | theme::DT_NOPREFIX;
    draw_label(hdc, &b.caption, BTN_COLORS[color], &mut tr, flags);
}


// ---------------------------------------------------------------------------
// Custom painting
// ---------------------------------------------------------------------------

/// Paint the accent banner: the product name and a one-line summary.
///
/// Drawing this in the parent keeps the strip and its text in a single
/// operation, so there is no chance of the label and the background
/// disagreeing about colour.
unsafe fn paint_banner(
    hdc: HDC,
    rect: &RECT,
    pad: i32,
    title_font: isize,
    sub_font: isize,
) {
    let brushes = match BRUSHES.get() {
        Some(b) => b,
        None => return,
    };
    if brushes.accent != 0 {
        theme::fill_rect(hdc, rect, brushes.accent);
    }
    theme::set_text_color(hdc, theme::TEXT_BRIGHT);
    // Opaque background would paint a box behind the glyphs; the strip is
    // already filled, so let the text sit directly on it.
    theme::set_bk_transparent(hdc);

    let flags = theme::DT_LEFT | theme::DT_SINGLELINE | theme::DT_VCENTER | theme::DT_NOPREFIX;
    let title = wide("Spass");
    if title_font != 0 {
        let old = theme::select_object(hdc, title_font);
        let mut r = RECT {
            left: rect.left + pad,
            top: rect.top,
            right: rect.right - pad,
            bottom: rect.top + rect.bottom / 2 + theme::px(2),
        };
        theme::draw_text(hdc, &title, &mut r, flags);
        theme::select_object(hdc, old);
    }

    let sub = wide("Install the Spass language toolchain");
    if sub_font != 0 {
        let old = theme::select_object(hdc, sub_font);
        let mut r = RECT {
            left: rect.left + pad,
            top: rect.top + rect.bottom / 2,
            right: rect.right - pad,
            bottom: rect.bottom,
        };
        theme::draw_text(hdc, &sub, &mut r, flags);
        theme::select_object(hdc, old);
    }

    // A quiet version badge on the right, inside a translucent-looking pill.
    // It is drawn from GDI primitives so it needs no extra control.
    if let Some(art) = BTN_ART.get() {
        let version = wide(concat!("v", env!("CARGO_PKG_VERSION")));
        let badge_w = theme::px(64);
        let badge_h = theme::px(24);
        let badge = RECT {
            left: rect.right - pad - badge_w,
            top: rect.top + (rect.bottom - rect.top - badge_h) / 2 - theme::px(2),
            right: rect.right - pad,
            bottom: rect.top + (rect.bottom - rect.top + badge_h) / 2 - theme::px(2),
        };
        fill_round(hdc, art, C_ACCENT_PRESSED, C_ACCENT_PRESSED, &badge, theme::px(12));
        if sub_font != 0 {
            let old = theme::select_object(hdc, sub_font);
            theme::draw_text(hdc, &version, &mut { badge },
                theme::DT_CENTER | theme::DT_VCENTER | theme::DT_SINGLELINE | theme::DT_NOPREFIX);
            theme::select_object(hdc, old);
        }
    }
}

/// Draw a progress bar into `rect` on an already-begun DC.
///
/// There is no progress control to link against in this toolchain, so the
/// bar is composed from two `FillRect` calls: a track colour, and on top of
/// it the accent-coloured filled portion. The parent owns the strip (see
/// `Ui::track_rect`), so no child control clips the paint away, and taking
/// the DC as an argument keeps `WM_PAINT` down to one BeginPaint/EndPaint.
unsafe fn draw_progress(hdc: HDC, rect: &RECT, active: bool, fraction: f64, busy: bool) {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let brushes = match BRUSHES.get() {
        Some(b) => b,
        None => return,
    };
    // A 1px inset leaves a border so the bar reads as a groove.
    let inset = theme::px(1).max(1);
    let bar = RECT {
        left: rect.left + inset,
        top: rect.top + inset,
        right: rect.right - inset,
        bottom: rect.bottom - inset,
    };
    if brushes.track != 0 {
        theme::fill_rect(hdc, &bar, brushes.track);
    }
    if active && fraction > 0.0 {
        let track_w = bar.right - bar.left;
        // Keep a minimum visible nub so 1% is still perceptible.
        let fill_w = ((track_w as f64 * fraction).round() as i32).max(inset * 2);
        let filled = RECT { right: bar.left + fill_w.min(track_w), ..bar };
        let brush = if busy { brushes.accent_dark } else { brushes.accent };
        if brush != 0 {
            theme::fill_rect(hdc, &filled, brush);
        }
    }
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as isize;
            // The checkboxes auto-toggle, so refresh our cached copy of the
            // state and repaint. Reading it here is safe: this arrives from
            // a user click, long after every control exists.
            if id == ID_ADDPATH || id == ID_RUST {
                let ctrl = GetDlgItem(hwnd, id as c_int);
                if let Some(ui) = Ui::from_hwnd(hwnd) {
                    let checked = ctrl != 0
                        && SendMessageW(ctrl, BM_GETCHECK, 0, 0) == BST_CHECKED_L;
                    if let Some(b) = ui.buttons.iter_mut().find(|b| b.id == id) {
                        b.checked = checked;
                    }
                    if ctrl != 0 {
                        InvalidateRect(ctrl, std::ptr::null(), FALSE_);
                    }
                }
            }
            // Scope cards behave like a radio pair: selecting one clears the
            // other and moves the default install location accordingly.
            if id == ID_SCOPE_USER || id == ID_SCOPE_MACHINE {
                if let Some(ui) = Ui::from_hwnd(hwnd) {
                    select_scope(ui, if id == ID_SCOPE_MACHINE { Scope::Machine } else { Scope::User });
                }
            }
            match id {
                ID_INSTALL => {
                    if let Some(ui) = Ui::from_hwnd(hwnd) {
                        do_install(ui);
                    }
                }
                ID_UNINSTALL => {
                    if let Some(ui) = Ui::from_hwnd(hwnd) {
                        do_uninstall(ui);
                    }
                }
                ID_DEFAULT => {
                    if let Some(ui) = Ui::from_hwnd(hwnd) {
                        let d = ui.scope.default_dir();
                        set_text(ui.path, &d.display().to_string());
                    }
                }
                ID_CLOSE => {
                    let busy = Ui::from_hwnd(hwnd).map(|u| u.busy).unwrap_or(false);
                    if busy {
                        message_box(
                            hwnd,
                            "An operation is still running. Please wait for it to finish.",
                            "Spass Setup",
                            MB_ICONINFORMATION,
                        );
                    } else {
                        DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_PAINT => {
            // One BeginPaint/EndPaint pair for the whole window. Calling
            // BeginPaint a second time after EndPaint asks Windows for a
            // second update region that does not exist, which leaves the
            // window unpainted and can wedge the message loop -- so the
            // banner and the bar are drawn inside a single pass.
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                let mut ps = PAINTSTRUCT {
                    hdc: 0,
                    fErase: 1,
                    rcPaint: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    fRestore: 0,
                    fIncUpdate: 0,
                    rgbReserved: [0; 32],
                };
                let hdc = BeginPaint(hwnd, &mut ps);
                if hdc != 0 {
                    paint_banner(
                        hdc,
                        &ui.banner_rect,
                        ui.banner_pad,
                        ui.f_title,
                        ui.f_sub,
                    );
                    draw_progress(hdc, &ui.track_rect, ui.progress_active, ui.progress, ui.busy);
                }
                EndPaint(hwnd, &mut ps);
            }
            0
        }
        WM_DRAWITEM => {
            // No owner-drawn controls remain: Windows paints every button,
            // radio and checkbox itself. Ignore this message.
            0
        }
        WM_MOUSELEAVE => {
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                for b in ui.buttons.iter_mut() {
                    b.hovered = false;
                }
            }
            // Repaint every button so the highlight is actually removed.
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                let ids: Vec<isize> = ui.buttons.iter().map(|b| b.id).collect();
                for id in ids {
                    let h = GetDlgItem(hwnd, id as c_int);
                    if h != 0 {
                        InvalidateRect(h, std::ptr::null(), FALSE_);
                    }
                }
            }
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            // Without this, every STATIC paints its own grey button-face
            // rectangle, which reads as a stack of stray boxes. Returning a
            // brush that matches the surface the label sits on makes the
            // label background invisible.
            if lparam == 0 {
                return 0;
            }
            let hdc = wparam as HDC;
            let id = GetDlgCtrlID(lparam as HWND) as isize;
            let brushes = BRUSHES.get();
            theme::set_bk_transparent(hdc);
            if id == ID_STATUS {
                theme::set_text_color(hdc, theme::TEXT_DIM);
                if let Some(b) = brushes {
                    if b.panel != 0 {
                        return b.panel as LRESULT;
                    }
                }
            } else if id == ID_LOG {
                if let Some(b) = brushes {
                    if b.log != 0 {
                        return b.log as LRESULT;
                    }
                }
            }
            // A shared system brush: nothing to free, and it matches the
            // window background.
            GetSysColorBrush(5) as LRESULT
        }
        WM_APP_LOG => {
            // Reclaim the String the worker boxed for us.
            let text = Box::from_raw(lparam as *mut String);
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                ui.log_line(&text);
            }
            0
        }
        WM_APP_STATUS => {
            let text = Box::from_raw(lparam as *mut String);
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                ui.status(&text);
            }
            0
        }
        WM_APP_PROGRESS => {
            let done = wparam as u64;
            let total = lparam as u64;
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                let frac = if total == 0 { 0.0 } else { done as f64 / total as f64 };
                ui.set_progress(frac);
                if total > 0 {
                    let msg = format!("Downloading... {:.0}%", frac * 100.0);
                    ui.status(&msg);
                }
            }
            0
        }
        WM_APP_DONE => {
            let code = wparam as i32;
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                ui.set_busy(false);
                ui.set_progress(if code == 0 { 1.0 } else { 0.0 });
                ui.status(if code == 0 {
                    "Done."
                } else {
                    "Finished with errors - see the log."
                });
            }
            0
        }
        WM_CLOSE => {
            // Refuse to close mid-operation: the worker holds a raw window
            // handle and would post to freed memory.
            let busy = Ui::from_hwnd(hwnd).map(|u| u.busy).unwrap_or(false);
            if busy {
                message_box(
                    hwnd,
                    "An operation is still running. Please wait for it to finish.",
                    "Spass Setup",
                    MB_ICONINFORMATION,
                );
            } else {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            // Release the GDI objects we created; leaking them would slowly
            // exhaust the per-process quota over repeated runs.
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                for f in [ui.f_title, ui.f_sub, ui.f_body, ui.f_section, ui.f_mono] {
                    if f != 0 {
                        theme::delete_object(f);
                    }
                }
            }
            // The brushes live in the process-wide slot rather than in `Ui`,
            // so they are released here and stay valid for the whole window.
            if let Some(b) = BRUSHES.get() {
                for b in [b.accent, b.accent_dark, b.panel, b.track, b.log, b.soft] {
                    if b != 0 {
                        theme::delete_object(b);
                    }
                }
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Read the current form values into an `Opts`.
unsafe fn read_opts(ui: &Ui) -> crate::Opts {
    let dir = get_text(ui.path);
    let dir = if dir.trim().is_empty() {
        crate::default_install_dir()
    } else {
        std::path::PathBuf::from(dir.trim())
    };
    let add_path = SendMessageW(ui.chk_path, BM_GETCHECK, 0, 0) == BST_CHECKED_L;
    let install_rust = SendMessageW(ui.chk_rust, BM_GETCHECK, 0, 0) == BST_CHECKED_L;
    crate::Opts {
        install_dir: dir,
        add_to_path: add_path,
        uninstall: false,
        // The GUI asks its own questions up front, so the worker must never
        // try to read stdin.
        force: true,
        silent: true,
        install_rust,
        scope: ui.scope,
        elevated: ui.elevated,
        autostart: ui.autostart,
    }
}

/// Apply a scope choice: update cached radio state, repaint both cards, and
/// move the location field to that scope's default *only if* the user has not
/// typed a custom path (i.e. it still matches a known default).
unsafe fn select_scope(ui: &mut Ui, scope: Scope) {
    ui.scope = scope;
    for (id, is_sel) in [
        (ID_SCOPE_USER, scope == Scope::User),
        (ID_SCOPE_MACHINE, scope == Scope::Machine),
    ] {
        let h = GetDlgItem(ui.hwnd, id as c_int);
        if h != 0 {
            let st = if is_sel { BST_CHECKED } else { 0 };
            SendMessageW(h, BM_SETCHECK, st, 0);
            InvalidateRect(h, std::ptr::null(), FALSE_);
        }
        if let Some(b) = ui.buttons.iter_mut().find(|b| b.id == id) {
            b.checked = is_sel;
        }
    }
    let current = get_text(ui.path);
    let user_default = Scope::User.default_dir().display().to_string();
    let machine_default = Scope::Machine.default_dir().display().to_string();
    let next_default = scope.default_dir().display().to_string();
    if current.trim() == user_default || current.trim() == machine_default {
        set_text(ui.path, &next_default);
    }
    let note = GetDlgItem(ui.hwnd, ID_SCOPE_NOTE as c_int);
    if note != 0 {
        set_text(note, if scope == Scope::Machine {
            "Installed for everyone on this PC. Needs administrator rights."
        } else {
            "Installed for your account only. No administrator rights needed."
        });
    }
    ui.status(if scope == Scope::Machine {
        "All users: writes to Program Files and the system PATH (admin required)."
    } else {
        "Just me: writes to your user profile and user PATH."
    });
}

/// Spawn the install on a worker thread and return immediately.
fn do_install(ui: &mut Ui) {
    unsafe {
        let mut opts = read_opts(ui);

        // A machine-wide install needs write access to HKLM and Program
        // Files. If we are not elevated yet, hand the exact same choices to
        // an elevated copy of ourselves, which reopens the window and starts
        // the install straight away (`--autostart`).
        if opts.scope == Scope::Machine && !opts.elevated && !crate::elevate::is_admin() {
            let mut relaunch = vec![
                "--scope".to_string(), "machine".to_string(),
                "--elevated".to_string(), "--autostart".to_string(),
                "--install-dir".to_string(), opts.install_dir.display().to_string(),
            ];
            if !opts.add_to_path {
                relaunch.push("--no-path".to_string());
            }
            if opts.install_rust {
                relaunch.push("--install-rust".to_string());
            }
            if crate::elevate::relaunch_elevated(&crate::shell_join(&relaunch)) {
                ui.status("Administrator rights granted - continuing in the elevated window.");
                ui.log_line("    elevated install started in a new window");
                // Ask this (non-elevated) window to close. The elevated copy
                // owns the rest of the operation.
                PostMessageW(ui.hwnd, WM_CLOSE, 0, 0);
            } else {
                ui.status("Administrator rights were not granted.");
                ui.log_line("!! elevation was cancelled or failed");
            }
            return;
        }
        // We are the elevated copy: never ask for confirmation again.
        if opts.elevated {
            opts.force = true;
        }

        // Ask up front what the worker would otherwise ask on stdin.
        if opts.install_dir.join("spass.exe").exists()
            && !message_box(
                ui.hwnd,
                &format!("Spass is already installed in:\n\n{}\n\nReplace it?", opts.install_dir.display()),
                "Replace existing install?",
                MB_YESNO | MB_ICONQUESTION,
            )
        {
            ui.log_line("    cancelled");
            return;
        }

        ui.set_busy(true);
        ui.set_progress(0.0);
        ui.status("Installing...");

        let hwnd = ui.hwnd;
        std::thread::spawn(move || {
            let mut code = 0;
            if opts.install_rust {
                code = crate::run_install_rust(&opts);
            }
            if code == 0 {
                code = crate::run_install(&opts);
            }
            PostMessageW(hwnd, WM_APP_DONE, code as usize, 0);
        });
    }
}

/// Spawn the uninstall on a worker thread, after confirming.
fn do_uninstall(ui: &mut Ui) {
    unsafe {
        let mut opts = read_opts(ui);
        opts.uninstall = true;

        if opts.scope == Scope::Machine && !opts.elevated && !crate::elevate::is_admin() {
            let mut relaunch = vec![
                "--scope".to_string(), "machine".to_string(),
                "--elevated".to_string(), "--autostart".to_string(),
                "--uninstall".to_string(),
                "--install-dir".to_string(), opts.install_dir.display().to_string(),
            ];
            if crate::elevate::relaunch_elevated(&crate::shell_join(&relaunch)) {
                ui.status("Administrator rights granted - continuing in the elevated window.");
                PostMessageW(ui.hwnd, WM_CLOSE, 0, 0);
            } else {
                ui.status("Administrator rights were not granted.");
            }
            return;
        }
        if opts.elevated {
            opts.force = true;
        }

        if !opts.install_dir.exists() {
            ui.log_line("    nothing to remove - Spass is not installed there");
            return;
        }
        let msg = format!(
            "Remove Spass from:\n\n{}\n\nThe install directory will be deleted.",
            opts.install_dir.display()
        );
        if !message_box(ui.hwnd, &msg, "Uninstall Spass?", MB_YESNO | MB_ICONQUESTION) {
            ui.log_line("    cancelled");
            return;
        }

        ui.set_busy(true);
        ui.set_progress(0.0);
        ui.status("Removing...");

        let hwnd = ui.hwnd;
        std::thread::spawn(move || {
            let code = crate::run_uninstall(&opts);
            PostMessageW(hwnd, WM_APP_DONE, code as usize, 0);
        });
    }
}

// ---------------------------------------------------------------------------
// Window construction
// ---------------------------------------------------------------------------

unsafe fn make_control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    x: c_int,
    y: c_int,
    w: c_int,
    h: c_int,
    id: isize,
) -> HWND {
    let cls = wide(class);
    let txt = wide(text);
    CreateWindowExW(
        0,
        cls.as_ptr(),
        txt.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        w,
        h,
        parent,
        id,
        0 as HINSTANCE,
        std::ptr::null_mut(),
    )
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(modname: *const u16) -> HINSTANCE;
}

const WM_QUIT_: UINT = 0x0012;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;

/// Build the window and run the message loop. Returns a process exit code.
pub fn run(opts: &crate::Opts, reporter: Arc<GuiReporter>) -> i32 {
    unsafe {
        // The installer is a console-subsystem binary so the CLI works from a
        // terminal. A double-click would otherwise leave an empty console
        // window behind the GUI; detach it now that we know we are windowed.
        crate::elevate::free_console();
        theme::enable_visual_styles();
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("SpassSetupWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
            style: 0x0003, // CS_HREDRAW | CS_VREDRAW
            lpfnWndProc: wnd_proc,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: 0,
            hCursor: LoadCursorW(0, IDC_ARROW as *const u16),
            hbrBackground: 0,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: 0,
        };
        RegisterClassExW(&wc);

        // The layout is authored at 96 DPI and scaled at runtime, so it holds
        // up on high-DPI displays without a per-monitor manifest.
        let s = |v: i32| theme::px(v);
        let pad = s(24);
        let banner_h = s(96);
        let win_w = s(680);
        let win_h = s(720);

        let title = wide("Spass Setup");
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            win_w,
            win_h,
            0,
            0,
            hinstance,
            std::ptr::null_mut(),
        );
        if hwnd == 0 {
            return 1;
        }

        // Fonts, then brushes, then controls. The fonts must exist before
        // any control asks for one.
        let f_title = theme::make_font(22, true).unwrap_or(0);
        let f_sub = theme::make_font(12, false).unwrap_or(0);
        let f_body = theme::make_font(12, false).unwrap_or(0);
        let f_section = theme::make_font(11, true).unwrap_or(0);
        let f_mono = theme::make_mono_font(9).unwrap_or(0);

        let b_accent = theme::create_brush(theme::ACCENT).unwrap_or(0);
        let b_accent_dark = theme::create_brush(theme::ACCENT_DARK).unwrap_or(0);
        let b_panel = theme::create_brush(theme::PANEL).unwrap_or(0);
        let b_track = theme::create_brush(theme::TRACK).unwrap_or(0);
        let b_log = theme::create_brush(theme::LOG_BG).unwrap_or(0);
        let b_soft = theme::create_brush(theme::ACCENT_SOFT).unwrap_or(0);

        // Publish the brushes before any child exists. `WM_CTLCOLOR*` fires
        // while children are still being created, which is long before
        // `GWLP_USERDATA` can be set, and the banner/title need the accent
        // brush for their very first paint.
        let _ = BRUSHES.set(Brushes {
            accent: b_accent,
            accent_dark: b_accent_dark,
            panel: b_panel,
            track: b_track,
            log: b_log,
            soft: b_soft,
        });

        let set_font = |h: HWND, f: isize| {
            if f != 0 {
                SendMessageW(h, WM_SETFONT, f as WPARAM, 1);
            }
        };
        let lbl = |text: &str, x: i32, y: i32, w: i32, h: i32, id: isize, font: isize| {
            let c = make_control(hwnd, "STATIC", text, SS_LEFT, x, y, w, h, id);
            set_font(c, font);
            c
        };

        // ---- Banner --------------------------------------------------
        // Lay out against the *client* area, not the window size we asked
        // for: the title bar and border eat into the height, and budgeting
        // against `win_h` would push the buttons off the bottom edge.
        let mut cr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut cr);
        let client_w = if cr.right - cr.left > 0 { cr.right - cr.left } else { win_w };
        // `win_h` includes the title bar and borders; using it as a fallback
        // for the *client* height pushes the footer controls below the real
        // client edge, where they get clipped. Reserve space for the frame.
        let client_h = if cr.bottom - cr.top > 0 {
            cr.bottom - cr.top
        } else {
            win_h - s(56)
        };
        // Control columns follow the real width so the row still lines up
        // if the user resizes the window.
        let field_w = client_w - pad * 2;

        // ---- Banner ----------------------------------------------------
        //
        // The banner is painted by the parent rather than built from
        // `STATIC` controls. `WM_CTLCOLOR*` is sent while children are being
        // created -- long before `GWLP_USERDATA` is published -- so a
        // label sitting on a coloured strip would get a white brush on its
        // first paint and end up white-on-white. Drawing it here means the
        // HDC, font, and colours are all under our direct control.
        let banner_rect = RECT { left: 0, top: 0, right: client_w, bottom: banner_h };

        // ---- Who is this for? -----------------------------------------
        let y_scope = banner_h + s(18);
        lbl("WHO IS THIS FOR?", pad, y_scope, s(300), s(18), ID_SCOPE_SECTION, f_section);
        let card_gap = s(12);
        let card_w = (field_w - card_gap) / 2;
        let card_y = y_scope + s(24);
        let card_h = s(58);
        let mk_card = |text: &str, x: i32, id: isize, checked: bool| {
            // WS_GROUP on the first radio groups both cards so selecting one
            // clears the other automatically.
            let c = make_control(hwnd, "BUTTON", text, BS_AUTORADIOBUTTON_N | WS_TABSTOP | WS_GROUP, x, card_y, card_w, card_h, id);
            set_font(c, f_body);
            theme::apply_theme(c);
            if checked {
                SendMessageW(c, BM_SETCHECK, BST_CHECKED, 0);
            }
            c
        };
        let scope_user = mk_card("Just me", pad, ID_SCOPE_USER, opts.scope == Scope::User);
        let scope_machine = make_control(hwnd, "BUTTON", "All users", BS_AUTORADIOBUTTON_N | WS_TABSTOP,
            pad + card_w + card_gap, card_y, card_w, card_h, ID_SCOPE_MACHINE);
        set_font(scope_machine, f_body);
        theme::apply_theme(scope_machine);
        if opts.scope == Scope::Machine {
            SendMessageW(scope_machine, BM_SETCHECK, BST_CHECKED, 0);
        }
        lbl(
            if opts.scope == Scope::Machine {
                "Installed for everyone on this PC. Needs administrator rights."
            } else {
                "Installed for your account only. No administrator rights needed."
            },
            pad + s(2), card_y + card_h + s(4), field_w - s(4), s(18), ID_SCOPE_NOTE, f_sub,
        );

        // ---- Install location ----------------------------------------
        let y_loc = card_y + card_h + s(30);
        lbl("INSTALL LOCATION", pad, y_loc, s(300), s(18), ID_SECTION_LOC, f_section);
        // The path box and the reset button share the row, so the box gets
        // the column minus the button and the gap between them.
        let reset_w = s(120);
        let row_gap = s(10);
        let path_w = (field_w - reset_w - row_gap).max(s(120));
        let path = make_control(
            hwnd,
            "EDIT",
            &opts.install_dir.display().to_string(),
            ES_AUTOVSCROLL | WS_BORDER | WS_TABSTOP,
            pad,
            y_loc + s(24),
            path_w,
            s(26),
            ID_PATH,
        );
        set_font(path, f_body);
        theme::apply_theme(path);

        let btn_default = make_control(
            hwnd,
            "BUTTON",
            "Reset to default",
            BS_PUSHBUTTON_N | WS_TABSTOP,
            pad + path_w + row_gap,
            y_loc + s(24),
            reset_w,
            s(26),
            ID_DEFAULT,
        );
        set_font(btn_default, f_body);
        theme::apply_theme(btn_default);

        // ---- Options --------------------------------------------------
        let y_opt = y_loc + s(68);
        lbl("OPTIONS", pad, y_opt, s(300), s(18), ID_SECTION_OPT, f_section);
        let mk_check = |text: &str, x: i32, y: i32, w: i32, id: isize, checked: bool| {
            let c = make_control(hwnd, "BUTTON", text, BS_AUTOCHECKBOX_N | WS_TABSTOP, x, y, w, s(24), id);
            set_font(c, f_body);
            theme::apply_theme(c);
            if checked {
                SendMessageW(c, BM_SETCHECK, BST_CHECKED, 0);
            }
            c
        };
        let chk_path = mk_check(
            "Add the install location to my PATH",
            pad + s(4),
            y_opt + s(26),
            s(500),
            ID_ADDPATH,
            true,
        );
        let chk_rust = mk_check(
            "Also install the Rust toolchain",
            pad + s(4),
            y_opt + s(54),
            s(500),
            ID_RUST,
            true,
        );
        // Explains *why* Rust matters in its own line, rather than cramming
        // it into the checkbox label where it gets truncated.
        lbl(
            "Needed by `spass test` and `spass run`, which compile your code with rustc. \
             `spass build` works without it.",
            pad + s(26),
            y_opt + s(78),
            s(520),
            s(34),
            ID_RUST_NOTE,
            f_sub,
        );

        // ---- Progress --------------------------------------------------
        let y_prog = y_opt + s(126);
        lbl("PROGRESS", pad, y_prog, s(300), s(18), ID_SECTION_PROG, f_section);
        // No child control here: a child would sit on top of the strip and
        // clip the parent's own painting out of it. The parent keeps the
        // rect and paints the groove and fill itself.
        let track_rect = RECT {
            left: pad,
            top: y_prog + s(24),
            right: pad + field_w,
            bottom: y_prog + s(24) + s(10),
        };

        // ---- Log -------------------------------------------------------
        let y_log = y_prog + s(48);
        lbl("ACTIVITY LOG", pad, y_log, s(300), s(18), ID_SECTION_LOG, f_section);
        // Anchor the footer to the *bottom* of the client area and work
        // upwards, instead of deriving it from the log height. Anchoring
        // this way keeps the status line and the button row from ever
        // overlapping or falling off the bottom edge, at any window size.
        let bh = s(34);
        let status_h = s(20);
        let by = client_h - s(14) - bh;
        let y_status = by - s(10) - status_h;
        let log_bottom = y_status - s(10);
        let log_h = (log_bottom - (y_log + s(24))).max(s(60));
        let log = make_control(
            hwnd,
            "EDIT",
            "",
            ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY | WS_VSCROLL | WS_BORDER,
            pad,
            y_log + s(24),
            field_w,
            log_h,
            ID_LOG,
        );
        set_font(log, f_mono);
        theme::apply_theme(log);

        // ---- Status + buttons ------------------------------------------
        let y_status = by - s(10) - status_h;
        let status = make_control(
            hwnd,
            "STATIC",
            "Ready.",
            SS_LEFT | SS_ENDELLIPSIS | SS_CENTERIMAGE,
            pad,
            y_status,
            field_w,
            status_h,
            ID_STATUS,
        );
        set_font(status, f_body);

        let bw = s(140);
        let btn_install = make_control(
            hwnd, "BUTTON", "Install", BS_PUSHBUTTON_N | WS_TABSTOP,
            pad, by, bw, bh, ID_INSTALL,
        );
        let btn_uninstall = make_control(
            hwnd, "BUTTON", "Uninstall", BS_PUSHBUTTON_N | WS_TABSTOP,
            pad + bw + s(10), by, bw, bh, ID_UNINSTALL,
        );
        let btn_close = make_control(
            hwnd, "BUTTON", "Close", BS_PUSHBUTTON_N | WS_TABSTOP,
            pad + field_w - bw, by, bw, bh, ID_CLOSE,
        );
        for b in [btn_install, btn_uninstall, btn_close] {
            set_font(b, f_body);
            theme::apply_theme(b);
        }

        // Register every owner-drawn button so the parent can hit-test the
        // pointer for the hover highlight.
        let mut buttons: Vec<BtnBox> = Vec::new();
        {
            // `GetWindowRect` on a child is screen-based, so we need our own
            // client origin in screen coordinates to convert from.
            let mut origin = POINT { x: 0, y: 0 };
            ClientToScreen(hwnd, &mut origin);
            for id in [ID_ADDPATH, ID_RUST, ID_DEFAULT, ID_INSTALL, ID_UNINSTALL, ID_CLOSE,
                       ID_SCOPE_USER, ID_SCOPE_MACHINE] {
                let h = GetDlgItem(hwnd, id as c_int);
                if h != 0 {
                    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetWindowRect(h, &mut r);
                    // Cache the caption and font now, so `WM_DRAWITEM` never
                    // has to ask the control for them mid-creation.
                    let caption = window_text(h);
                    let font = SendMessageW(h, WM_GETFONT, 0, 0) as isize;
                    let checked = SendMessageW(h, BM_GETCHECK, 0, 0) == BST_CHECKED_L;
                    buttons.push(BtnBox {
                        id,
                        rect: RECT {
                            left: r.left - origin.x,
                            top: r.top - origin.y,
                            right: r.right - origin.x,
                            bottom: r.bottom - origin.y,
                        },
                        caption,
                        font,
                        checked,
                        hovered: false,
                    });
                }
            }
        }

        let ui = Box::new(Ui {
            hwnd, log, status,
            btn_install, btn_uninstall, path, chk_path, chk_rust,
            scope_user, scope_machine, scope: opts.scope,
            elevated: opts.elevated, autostart: opts.autostart,
            busy: false,
            progress: 0.0,
            progress_active: false,
            track_rect,
            banner_rect,
            banner_pad: pad,
            buttons,
            f_title, f_sub, f_body, f_section, f_mono,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(ui) as isize);

        // Publish the handle so the reporter can post to it.
        *reporter.hwnd.lock().unwrap() = hwnd;

        ShowWindow(hwnd, SW_SHOW);
        // `UpdateWindow` sends WM_PAINT straight away, and that one pass now
        // paints the banner and the empty groove. A second BeginPaint here
        // would be a second, non-existent update region.
        UpdateWindow(hwnd);

        // Owner-drawn children sent WM_DRAWITEM during CreateWindowExW, long
        // before GWLP_USERDATA existed, so that first paint was discarded.
        // Windows does not repaint an owner-draw button on its own unless it
        // is invalidated, so force one now that the window is visible and the
        // Ui is reachable. Without this every button stays a flat grey square.
        for id in [ID_ADDPATH, ID_RUST, ID_DEFAULT, ID_INSTALL, ID_UNINSTALL, ID_CLOSE,
                   ID_SCOPE_USER, ID_SCOPE_MACHINE] {
            let h = GetDlgItem(hwnd, id as c_int);
            if h != 0 {
                InvalidateRect(h, std::ptr::null(), TRUE_);
                UpdateWindow(h);
            }
        }

        // The elevated relaunch hands its choices back through the command
        // line and asks us to begin without a second click.
        if opts.autostart {
            if let Some(ui) = Ui::from_hwnd(hwnd) {
                if opts.uninstall {
                    do_uninstall(ui);
                } else {
                    do_install(ui);
                }
            }
        }

        let mut msg = MSG { hwnd: 0, message: 0, wparam: 0, lparam: 0, time: 0, pt: POINT { x: 0, y: 0 } };
        loop {
            let r = GetMessageW(&mut msg, 0, 0, 0);
            if r <= 0 {
                break;
            }
            if msg.message == WM_QUIT_ {
                break;
            }
            TranslateMessage(&mut msg);
            DispatchMessageW(&msg);
        }
        0
    }
}
