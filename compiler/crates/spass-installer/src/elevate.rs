//! Windows elevation helpers, resolved dynamically so this crate keeps
//! building with the `x86_64-pc-windows-gnullvm` toolchain (which ships no
//! `advapi32`/`shell32` import libraries).
//!
//! Two jobs:
//!   * tell whether the current process already has administrator rights;
//!   * relaunch ourselves through `ShellExecuteW(..., "runas", ...)` when the
//!     user asked for a machine-wide install and we are not elevated.

use std::env;
use std::ffi::c_void;
use std::os::raw::c_int;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeConsole() -> c_int;
}

const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

unsafe fn symbol<T: Copy>(dll: &str, name: &[u8]) -> Option<T> {
    let wide: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
    let module = LoadLibraryExW(
        wide.as_ptr(),
        std::ptr::null_mut(),
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    );
    if module.is_null() {
        return None;
    }
    let p = GetProcAddress(module, name.as_ptr());
    if p.is_null() {
        return None;
    }
    Some(std::mem::transmute_copy(&p))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Detach the console a double-clicked console-subsystem process inherits.
/// Called as soon as we know we are showing the GUI, so Explorer does not
/// leave an empty black window behind the installer.
pub fn free_console() {
    #[cfg(windows)]
    unsafe {
        let _ = FreeConsole();
    }
}

/// `SID_IDENTIFIER_AUTHORITY` for the NT authority (`SECURITY_NT_AUTHORITY`).
#[repr(C)]
struct SidAuthority {
    value: [u8; 6],
}

type AllocateSidFn = unsafe extern "system" fn(
    *const SidAuthority,
    u8,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    *mut *mut c_void,
) -> c_int;
type CheckMembershipFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_int) -> c_int;
type FreeSidFn = unsafe extern "system" fn(*mut c_void) -> *mut c_void;

/// True when the current token is a member of the local Administrators group.
///
/// Uses `CheckTokenMembership` rather than `GetTokenInformation(TokenElevation)`
/// because the latter reports "elevated" for a split token in a way that is
/// easy to misread; membership is the question we actually care about — can
/// this process write to HKLM and Program Files?
pub fn is_admin() -> bool {
    #[cfg(windows)]
    unsafe {
        let allocate: AllocateSidFn = match symbol("advapi32.dll", b"AllocateAndInitializeSid\0") {
            Some(f) => f,
            None => return false,
        };
        let check: CheckMembershipFn = match symbol("advapi32.dll", b"CheckTokenMembership\0") {
            Some(f) => f,
            None => return false,
        };
        let free: FreeSidFn = match symbol("advapi32.dll", b"FreeSid\0") {
            Some(f) => f,
            None => return false,
        };

        let authority = SidAuthority {
            value: [0, 0, 0, 0, 0, 5],
        };
        let mut sid: *mut c_void = std::ptr::null_mut();
        // SECURITY_BUILTIN_DOMAIN_RID = 0x20,
        // DOMAIN_ALIAS_RID_ADMINS     = 0x220
        if allocate(
            &authority,
            2,
            0x20,
            0x220,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut sid,
        ) == 0
        {
            return false;
        }
        let mut member: c_int = 0;
        let rc = check(std::ptr::null_mut(), sid, &mut member);
        free(sid);
        rc != 0 && member != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

type ShellExecuteFn = unsafe extern "system" fn(
    isize,
    *const u16,
    *const u16,
    *const u16,
    *const u16,
    c_int,
) -> isize;

/// Relaunch this executable elevated, handing it `args` verbatim.
///
/// Returns `false` when the shell call failed (most commonly because the user
/// declined the UAC prompt, in which case `ShellExecuteW` returns a value
/// <= 32).
pub fn relaunch_elevated(args: &str) -> bool {
    #[cfg(windows)]
    unsafe {
        let exec: ShellExecuteFn = match symbol("shell32.dll", b"ShellExecuteW\0") {
            Some(f) => f,
            None => return false,
        };
        let exe = match env::current_exe() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let exe_w = wide(&exe.to_string_lossy());
        let verb = wide("runas");
        let params = wide(args);
        // SW_SHOWNORMAL
        let rc = exec(
            0,
            verb.as_ptr(),
            exe_w.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1,
        );
        rc > 32
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        false
    }
}
