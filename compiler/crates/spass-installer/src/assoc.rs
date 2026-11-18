//! Registers and removes the `.saple` file association.
//!
//! The hive follows the install scope: a per-user install writes to HKCU and
//! needs no elevation, while a per-machine install writes the same keys to
//! HKLM from an elevated process.

use std::path::Path;

use crate::Scope;

/// The registry key that names the package class.
const CLASS_KEY: &str = r"Software\Classes\spassfile.saple";
/// The registry key that binds the extension to the class.
const EXT_KEY: &str = r"Software\Classes\.saple";
/// The name of the class the extension points at.
const CLASS_NAME: &str = "spassfile.saple";
/// The class's own name, the one Explorer shows.
const CLASS_LABEL: &str = "Spass Package";

/// The command that opens a package: the runner, with the file.
///
/// The path is quoted: an install directory may contain a space.
fn open_command(exe: &Path) -> String {
    format!("\"{}\" runner \"%1\"", exe.display())
}

fn hive(scope: Scope) -> u32 {
    match scope {
        Scope::User => crate::HKEY_CURRENT_USER,
        Scope::Machine => crate::HKEY_LOCAL_MACHINE,
    }
}

/// Register the association. Returns whether every key was written.
pub fn register(exe: &Path, scope: Scope) -> bool {
    write_default(EXT_KEY, CLASS_NAME, scope)
        && write_default(CLASS_KEY, CLASS_LABEL, scope)
        && write_default(
            &format!(r"{}\shell\open\command", CLASS_KEY),
            &open_command(exe),
            scope,
        )
}

/// Remove the association. Returns whether every key was removed.
///
/// The extension key is only ours if it still points at our class; a
/// user who re-pointed `.saple` at something else keeps that choice.
pub fn unregister(scope: Scope) -> bool {
    let mut ok = true;
    if read_default(EXT_KEY, scope).as_deref() == Some(CLASS_NAME) {
        ok &= delete_key(EXT_KEY, scope);
    }
    for key in [
        r"Software\Classes\spassfile.saple\shell\open\command",
        r"Software\Classes\spassfile.saple\shell\open",
        r"Software\Classes\spassfile.saple\shell",
        CLASS_KEY,
    ] {
        ok &= delete_key(key, scope);
    }
    ok
}

/// Write a key's default value, creating the key and its parents.
#[cfg(windows)]
fn write_default(sub_key: &str, value: &str, scope: Scope) -> bool {
    let Some((create, set, close, _)) = crate::reg::assoc_symbols() else {
        return false;
    };
    let sub: Vec<u16> = format!("{}\0", sub_key).encode_utf16().collect();
    let mut h: isize = 0;
    let mut disposition: u32 = 0;
    let rc = unsafe {
        create(
            hive(scope) as isize,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            0,
            crate::KEY_WRITE,
            std::ptr::null_mut(),
            &mut h,
            &mut disposition,
        )
    };
    if rc != 0 {
        return false;
    }
    let name: Vec<u16> = "\0".encode_utf16().collect();
    let mut data: Vec<u8> = value.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    data.extend_from_slice(&[0, 0]);
    let rc = unsafe { set(h, name.as_ptr(), 0, crate::REG_SZ, data.as_ptr(), data.len() as u32) };
    unsafe { close(h) };
    rc == 0
}

#[cfg(not(windows))]
fn write_default(_sub_key: &str, _value: &str, _scope: Scope) -> bool {
    false
}

/// Read a key's default value.
#[cfg(windows)]
fn read_default(sub_key: &str, scope: Scope) -> Option<String> {
    use std::mem::size_of;
    let (open, get, _set, close) = crate::reg::symbols()?;
    let sub: Vec<u16> = format!("{}\0", sub_key).encode_utf16().collect();
    let mut h: isize = 0;
    if unsafe { open(hive(scope), sub.as_ptr(), 0, crate::KEY_QUERY_VALUE, &mut h) } != 0 {
        return None;
    }
    let name: Vec<u16> = "\0".encode_utf16().collect();
    let mut size: u32 = 0;
    let rc = unsafe {
        get(h, name.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(),
            std::ptr::null_mut(), &mut size)
    };
    if rc != 0 || size < size_of::<u16>() as u32 {
        unsafe { close(h) };
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let mut ty: u32 = 0;
    let rc = unsafe {
        get(h, name.as_ptr(), std::ptr::null_mut(), &mut ty, buf.as_mut_ptr(), &mut size)
    };
    unsafe { close(h) };
    if rc != 0 {
        return None;
    }
    let utf16: Vec<u16> = buf[..size as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    Some(String::from_utf16_lossy(&utf16))
}

#[cfg(not(windows))]
fn read_default(_sub_key: &str, _scope: Scope) -> Option<String> {
    None
}

/// Remove a key. A key with subkeys is left alone.
#[cfg(windows)]
fn delete_key(sub_key: &str, scope: Scope) -> bool {
    let Some((_create, _set, _close, delete)) = crate::reg::assoc_symbols() else {
        return false;
    };
    let sub: Vec<u16> = format!("{}\0", sub_key).encode_utf16().collect();
    (unsafe { delete(hive(scope) as isize, sub.as_ptr()) }) == 0
}

#[cfg(not(windows))]
fn delete_key(_sub_key: &str, _scope: Scope) -> bool {
    false
}
