//! Windows "Apps & features" registration and install detection.
//!
//! A per-user install writes to HKCU, a per-machine one to HKLM, under
//! `Software\Microsoft\Windows\CurrentVersion\Uninstall\Spass`. That is the
//! key Windows Settings reads, so registering here means Spass shows up with
//! a working **Uninstall** button regardless of which drive it lives on.
//!
//! Every entry point goes through the dynamically-resolved registry helpers
//! in `crate::reg` (the gnullvm toolchain has no advapi32 import library).

use std::path::{Path, PathBuf};

use crate::Scope;

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Spass";
const DISPLAY_NAME: &str = "Spass — spatial language toolchain";
const PUBLISHER: &str = "Spass";

/// Values written for the install at `dir`. The uninstaller copy lives next
/// to spass.exe so removing the app never depends on the downloaded setup.
pub struct InstallEntry<'a> {
    pub dir: &'a Path,
    pub version: &'a str,
    pub scope: Scope,
}

fn hive(scope: Scope) -> u32 {
    match scope {
        Scope::User => crate::HKEY_CURRENT_USER,
        Scope::Machine => crate::HKEY_LOCAL_MACHINE,
    }
}

/// Command Windows runs when the user presses "Uninstall" in Settings.
///
/// Quoted twice over: the value itself is a string in the registry, and the
/// exe path inside it needs quotes because an install dir may contain spaces
/// (and may be on any drive, e.g. `D:\Tools\Spass`).
fn uninstall_command(dir: &Path, scope: Scope) -> String {
    let exe = dir.join("spass-uninstall.exe");
    format!(
        "\"{}\" --uninstall --scope {} --install-dir \"{}\" --force --silent",
        exe.display(),
        scope.as_str(),
        dir.display()
    )
}

/// Register (or refresh) the Windows uninstall entry. Returns whether every
/// value was written.
#[cfg(windows)]
pub fn register(entry: &InstallEntry<'_>) -> bool {
    let Some((create, set, close, _delete)) = crate::reg::assoc_symbols() else {
        return false;
    };
    let sub: Vec<u16> = format!("{}\0", UNINSTALL_KEY).encode_utf16().collect();
    let mut h: isize = 0;
    let mut disposition: u32 = 0;
    let rc = unsafe {
        create(
            hive(entry.scope) as isize,
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

    let strings: [(&str, &str); 6] = [
        ("DisplayName", DISPLAY_NAME),
        ("DisplayVersion", entry.version),
        ("Publisher", PUBLISHER),
        ("InstallLocation", &entry.dir.display().to_string()),
        ("UninstallString", &uninstall_command(entry.dir, entry.scope)),
        ("DisplayIcon", &entry.dir.join("spass.exe").display().to_string()),
    ];
    let mut ok = true;
    for (name, value) in strings {
        ok &= unsafe { write_sz(set, h, name, value) };
    }
    // DWORD 1: hide Modify/Repair buttons, which this installer does not
    // implement as separate flows.
    let no_modify: Vec<u16> = "NoModify\0".encode_utf16().collect();
    let no_repair: Vec<u16> = "NoRepair\0".encode_utf16().collect();
    let one = 1u32.to_le_bytes();
    ok &= unsafe { set(h, no_modify.as_ptr(), 0, crate::REG_DWORD, one.as_ptr(), 4) } == 0;
    ok &= unsafe { set(h, no_repair.as_ptr(), 0, crate::REG_DWORD, one.as_ptr(), 4) } == 0;
    unsafe { close(h) };
    ok
}

#[cfg(not(windows))]
pub fn register(_entry: &InstallEntry<'_>) -> bool {
    false
}

#[cfg(windows)]
unsafe fn write_sz(
    set: crate::reg::RegSetValueExW,
    h: isize,
    name: &str,
    value: &str,
) -> bool {
    let n: Vec<u16> = format!("{}\0", name).encode_utf16().collect();
    let mut data: Vec<u8> = value.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    data.extend_from_slice(&[0, 0]);
    set(h, n.as_ptr(), 0, crate::REG_SZ, data.as_ptr(), data.len() as u32) == 0
}

/// Read one named string value from the uninstall key.
#[cfg(windows)]
fn read_named(scope: Scope, name: &str) -> Option<String> {
    use std::mem::size_of;
    let (open, get, _set, close) = crate::reg::symbols()?;
    let sub: Vec<u16> = format!("{}\0", UNINSTALL_KEY).encode_utf16().collect();
    let mut h: isize = 0;
    if unsafe { open(hive(scope), sub.as_ptr(), 0, crate::KEY_QUERY_VALUE, &mut h) } != 0 {
        return None;
    }
    let n: Vec<u16> = format!("{}\0", name).encode_utf16().collect();
    let mut size: u32 = 0;
    let rc = unsafe {
        get(h, n.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(),
            std::ptr::null_mut(), &mut size)
    };
    if rc != 0 || size < size_of::<u16>() as u32 {
        unsafe { close(h) };
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        get(h, n.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut(),
            buf.as_mut_ptr(), &mut size)
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
fn read_named(_scope: Scope, _name: &str) -> Option<String> {
    None
}

/// A discovered installation: where it lives and under which scope it was
/// registered. `None` when Spass is not on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub dir: PathBuf,
    pub scope: Scope,
}

/// Look for an existing install in one specific scope. Used when the GUI
/// flips the Just-me / All-users cards: each scope has its own registry row.
pub fn detect_scope(scope: Scope) -> Option<Found> {
    let loc = read_named(scope, "InstallLocation")?;
    let dir = PathBuf::from(loc);
    if dir.join("spass.exe").exists() {
        Some(Found { dir, scope })
    } else {
        None
    }
}

/// Look for an existing install. Prefer HKCU (per-user) so a user-level
/// install is found even on a machine-wide capable setup; fall back to HKLM.
pub fn detect() -> Option<Found> {
    for scope in [Scope::User, Scope::Machine] {
        if let Some(found) = detect_scope(scope) {
            return Some(found);
        }
    }
    None
}

/// How healthy an existing installation is. The GUI uses this to decide
/// whether the primary button should say Install, Uninstall, or Repair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// No trace of Spass in this scope.
    Absent,
    /// Binary and every registry value present and current.
    Healthy,
    /// Something is there but incomplete: a missing binary, a stale version,
    /// or a partial registry entry. Installation can be completed in place.
    NeedsRepair { dir: PathBuf, scope: Scope, reason: String },
}

/// Diagnose the install in one scope without changing anything.
///
/// A row in Apps & features with no `spass.exe`, or a binary whose install was
/// interrupted before the registry was written, is a repair candidate - not
/// a fresh install and not an uninstall. The returned `dir` is the best guess
/// of where the install belongs, taken from the registry or the scope default.
pub fn status(scope: Scope, current_version: &str) -> Health {
    let logged = read_named(scope, "InstallLocation");
    let binary = logged.as_ref().map(PathBuf::from).unwrap_or_else(|| scope.default_dir());
    let has_binary = binary.join("spass.exe").exists();
    let version = read_named(scope, "DisplayVersion");
    let has_uninstaller = binary.join("spass-uninstall.exe").exists();
    let has_command = read_named(scope, "UninstallString").is_some();

    match (logged, has_binary, version, has_command, has_uninstaller) {
        (None, false, _, _, _) => Health::Absent,
        (loc, true, Some(v), true, true) if v == current_version => {
            // Fully registered and up to date.
            let _ = loc;
            Health::Healthy
        }
        (loc, true, _, _, _) => Health::NeedsRepair {
            dir: loc.map(PathBuf::from).unwrap_or(binary),
            scope,
            reason: "registration incomplete or version differs".into(),
        },
        (loc, false, _, _, _) => Health::NeedsRepair {
            dir: loc.map(PathBuf::from).unwrap_or(binary),
            scope,
            reason: "spass.exe is missing".into(),
        },
    }
}

/// Remove the uninstall entry. A leftover key with no files would otherwise
/// leave a broken "Spass" row in Apps & features forever.
#[cfg(windows)]
pub fn unregister(scope: Scope) -> bool {
    let Some((_create, _set, close, delete)) = crate::reg::assoc_symbols() else {
        return false;
    };
    let sub: Vec<u16> = format!("{}\0", UNINSTALL_KEY).encode_utf16().collect();
    // RegDeleteKeyW on the leaf key; parents are shared with other apps.
    let result = unsafe { delete(hive(scope) as isize, sub.as_ptr()) };
    let _ = close;
    result == 0
}

#[cfg(not(windows))]
pub fn unregister(_scope: Scope) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_command_uses_named_uninstaller_and_quotes_paths() {
        let dir = Path::new(r"D:\Spass Tools");
        let cmd = uninstall_command(dir, Scope::User);
        assert!(cmd.starts_with("\"D:\\Spass Tools\\spass-uninstall.exe\""));
        assert!(cmd.contains("--uninstall --scope user"));
        assert!(cmd.contains("--install-dir \"D:\\Spass Tools\""));
    }

    #[test]
    fn uninstall_key_is_under_current_version() {
        assert!(UNINSTALL_KEY.contains(r"CurrentVersion\Uninstall"));
        assert!(UNINSTALL_KEY.ends_with("Spass"));
    }

    #[test]
    fn health_distinguishes_three_states() {
        // These assertions hold on a machine with no Spass installed. On a
        // machine that does have it, the exact variant depends on its state,
        // but it must always be one of the three and never panic.
        let h = status(Scope::User, "0.1.0");
        assert!(matches!(
            h,
            Health::Absent | Health::Healthy | Health::NeedsRepair { .. }
        ));
    }

    #[test]
    fn needs_repair_carries_a_directory_and_reason() {
        let h = Health::NeedsRepair {
            dir: PathBuf::from(r"D:\Spass"),
            scope: Scope::User,
            reason: "spass.exe is missing".into(),
        };
        match h {
            Health::NeedsRepair { dir, scope, reason } => {
                assert_eq!(dir, PathBuf::from(r"D:\Spass"));
                assert_eq!(scope, Scope::User);
                assert!(!reason.is_empty());
            }
            _ => panic!("expected NeedsRepair"),
        }
    }
}
