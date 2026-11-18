//! Runs the `spass-setup` installer entry point.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

mod assoc;
mod elevate;
mod gui;
mod rustup;
mod sha256;
mod theme;
mod ui;

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Who an installation applies to. This is the one decision that changes
/// both the target directory and which registry hive PATH / file-association
/// entries are written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Current user: `%LOCALAPPDATA%\Programs\Spass`, HKCU. No elevation.
    User,
    /// All users: `%ProgramFiles%\Spass`, HKLM. Needs elevation.
    Machine,
}

impl Scope {
    pub(crate) fn default_dir(self) -> PathBuf {
        match self {
            Scope::User => {
                let base = env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                base.join("Programs").join("Spass")
            }
            Scope::Machine => {
                let base = env::var_os("ProgramFiles")
                    .or_else(|| env::var_os("ProgramFiles(x86)"))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
                base.join("Spass")
            }
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Machine => "machine",
        }
    }
}

struct Opts {
    install_dir: PathBuf,
    add_to_path: bool,
    uninstall: bool,
    force: bool,
    silent: bool,
    install_rust: bool,
    scope: Scope,
    /// Set on the elevated relaunch so it skips asking for elevation again.
    elevated: bool,
    /// Set on the elevated GUI relaunch so it starts installing immediately.
    autostart: bool,
}

fn default_install_dir() -> PathBuf {
    Scope::User.default_dir()
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        install_dir: default_install_dir(),
        add_to_path: true,
        uninstall: false,
        force: false,
        silent: false,
        install_rust: false,
        scope: Scope::User,
        elevated: false,
        autostart: false,
    };
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                plain(&format!("spass-setup {}", VERSION));
                std::process::exit(0);
            }
            "--uninstall" => o.uninstall = true,
            "--no-path" => o.add_to_path = false,
            "--force" | "-y" => o.force = true,
            "--silent" => o.silent = true,
            "--install-rust" => o.install_rust = true,
            "--scope" => {
                i += 1;
                let v = args.get(i).ok_or("--scope needs a value (user|machine)")?;
                o.scope = parse_scope(v)?;
            }
            // Internal flags used by the elevated relaunch / GUI launch.
            "--elevated" => o.elevated = true,
            "--gui" | "/gui" => {}
            "--autostart" => o.autostart = true,
            "--install-dir" => {
                i += 1;
                let v = args.get(i).ok_or("--install-dir needs a value")?;
                o.install_dir = PathBuf::from(v);
            }
            other => {
                if let Some(v) = other.strip_prefix("--install-dir=") {
                    o.install_dir = PathBuf::from(v);
                } else if let Some(v) = other.strip_prefix("--scope=") {
                    o.scope = parse_scope(v)?;
                } else {
                    return Err(format!("unknown option `{}`", other));
                }
            }
        }
        i += 1;
    }
    // When no explicit directory was given, the default follows the scope.
    // Track this by comparing against the *user* default: any other choice
    // (including the machine default typed by hand) counts as explicit.
    if o.scope == Scope::Machine && o.install_dir == default_install_dir() {
        o.install_dir = Scope::Machine.default_dir();
    }
    Ok(o)
}

/// Join command-line arguments so `CommandLineToArgvW` reproduces them
/// exactly: quote any token containing whitespace or a quote, escaping
/// embedded quotes and trailing backslashes the way the Windows CRT does.
fn shell_join(args: &[String]) -> String {
    fn quote(s: &str) -> String {
        if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == '"') {
            return s.to_string();
        }
        let mut out = String::from("\"");
        let mut slashes = 0usize;
        for c in s.chars() {
            if c == '\\' {
                slashes += 1;
                continue;
            }
            if c == '"' {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
            } else {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
            }
            slashes = 0;
        }
        // Escape trailing backslashes so they do not escape the closing quote.
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }
    args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
}

fn parse_scope(v: &str) -> Result<Scope, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "user" | "me" | "current" | "per-user" => Ok(Scope::User),
        "machine" | "all" | "all-users" | "system" | "per-machine" => Ok(Scope::Machine),
        other => Err(format!(
            "unknown scope `{}` - expected `user` or `machine`",
            other
        )),
    }
}

fn print_help() {
    println!(
        "spass-setup {}\n\n\
         Install or uninstall the Spass language toolchain.\n\n\
         USAGE:\n    spass-setup [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --scope <SCOPE>       Who to install for: `user` (default, no admin)\n\
         \x20                       or `machine` (all users, prompts for admin)\n\
         \x20   --install-dir <DIR>  Install location (follows --scope by default)\n\
         \x20   --no-path            Do not modify PATH\n\
         \x20   --install-rust       Also download the official Rust installer (rustup),\n\
         \x20                       which `spass test` and `spass run` need\n\
         \x20   --silent             No prompts; assume yes\n\
         \x20   --force, -y          Overwrite an existing install without asking\n\
         \x20   --uninstall          Remove the installation\n\
         \x20   --gui                Force the window (a double-click opens it automatically)\n\
         \x20   --help, -h           Show this help\n\
         \x20   --version, -V        Show the version\n\n\
         EXAMPLES:\n\
         \x20   spass-setup                          install for the current user\n\
         \x20   spass-setup --scope machine          install for all users (elevates)\n\
         \x20   spass-setup --install-rust           install Spass and fetch Rust too\n\
         \x20   spass-setup --uninstall              remove Spass\n",
        VERSION
    );
}

/// Ask a yes/no question, honouring the non-interactive flags first.
///
/// The prompt is delegated to the active reporter so the GUI shows a real
/// dialog instead of trying to read stdin (which a double-clicked windowed
/// process has no console for).
fn confirm(o: &Opts, prompt: &str) -> bool {
    if o.force || o.silent {
        return true;
    }
    ui::reporter().confirm(prompt)
}

fn main() {
    // `--gui` forces the window; otherwise a double-click from Explorer is
    // detected via the parent process. An explicit flag on the command line
    // always means "run the CLI".
    let args: Vec<String> = env::args().skip(1).collect();
    // `--autostart` is the elevated GUI relaunch: it must open the window so
    // the worker can report progress there instead of in a stray console.
    let force_gui = args
        .iter()
        .any(|a| a == "--gui" || a == "/gui" || a == "--autostart");
    let other_args: Vec<String> = args
        .iter()
        .filter(|a| {
            a.as_str() != "--gui"
                && a.as_str() != "/gui"
                && a.as_str() != "--autostart"
                && a.as_str() != "--elevated"
        })
        .cloned()
        .collect();

    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            error(&format!("{}\nRun spass-setup --help for usage.", e));
            std::process::exit(2);
        }
    };

    if force_gui || (other_args.is_empty() && launched_by_explorer()) {
        let reporter = Arc::new(gui::GuiReporter::new());
        ui::set(reporter.clone());
        std::process::exit(gui::run(&opts, reporter));
    }

    // A machine-wide install writes to HKLM and Program Files. When we are
    // not elevated yet, restart ourselves once with "runas" so the real
    // install runs in an elevated process. `--elevated` breaks the loop.
    if opts.scope == Scope::Machine && !opts.elevated && !elevate::is_admin() {
        let mut relaunch: Vec<String> = vec!["--elevated".into()];
        relaunch.extend(other_args.iter().cloned());
        if elevate::relaunch_elevated(&shell_join(&relaunch)) {
            plain("Requesting administrator rights - continuing in the elevated window.");
            std::process::exit(0);
        }
        error("Could not obtain administrator rights. Run spass-setup.exe as administrator.");
        std::process::exit(1);
    }

    // The Rust step runs first when requested: `spass test` and `spass run`
    // need `rustc`, so it helps to settle that before laying down the
    // toolchain. A failure here aborts rather than installing something the
    // user cannot finish using.
    let mut code = 0;
    if opts.install_rust {
        code = run_install_rust(&opts);
    }
    if code == 0 {
        code = if opts.uninstall { run_uninstall(&opts) } else { run_install(&opts) };
    }
    std::process::exit(code);
}

/// True when the process was started by Explorer (a double-click) rather
/// than from a terminal.
///
/// A console-subsystem executable is always given a console, even when
/// double-clicked, so the presence of a console says nothing. The reliable
/// signal is the parent process: Explorer launches double-clicked programs
/// directly, while a terminal launches them from `conhost.exe`,
/// `OpenConsole.exe`, `powershell.exe`, `cmd.exe`, `WindowsTerminal.exe`
/// and so on. We only treat it as a GUI launch when the parent is the shell
/// itself.
fn launched_by_explorer() -> bool {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct PROCESSENTRY32W {
            dwSize: u32,
            cntUsage: u32,
            th32ProcessID: u32,
            th32DefaultHeapID: usize,
            th32ModuleID: u32,
            cntThreads: u32,
            th32ParentProcessID: u32,
            pcPriClassBase: i32,
            dwFlags: u32,
            szExeFile: [u16; 260],
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentProcessId() -> u32;
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
            fn Process32FirstW(snap: isize, entry: *mut PROCESSENTRY32W) -> i32;
            fn Process32NextW(snap: isize, entry: *mut PROCESSENTRY32W) -> i32;
            fn CloseHandle(h: isize) -> i32;
        }
        const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
        const NAME_MAX: usize = 260;

        unsafe {
            let me = GetCurrentProcessId();
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == 0 || snap == -1 {
                // Cannot tell; fall back to the console so we never surprise
                // a CLI user with a window.
                return false;
            }
            let mut parent = 0u32;
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                cntUsage: 0,
                th32ProcessID: 0,
                th32DefaultHeapID: 0,
                th32ModuleID: 0,
                cntThreads: 0,
                th32ParentProcessID: 0,
                pcPriClassBase: 0,
                dwFlags: 0,
                szExeFile: [0; NAME_MAX],
            };
            let mut more = Process32FirstW(snap, &mut entry) != 0;
            while more {
                if entry.th32ProcessID == me {
                    parent = entry.th32ParentProcessID;
                    break;
                }
                more = Process32NextW(snap, &mut entry) != 0;
            }
            if parent == 0 {
                CloseHandle(snap);
                return false;
            }

            // Now find the parent's image name.
            let mut name = String::new();
            entry.th32ProcessID = 0;
            more = Process32FirstW(snap, &mut entry) != 0;
            while more {
                if entry.th32ProcessID == parent {
                    let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(NAME_MAX);
                    name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
                    break;
                }
                more = Process32NextW(snap, &mut entry) != 0;
            }
            CloseHandle(snap);

            // Only Explorer itself means "double-clicked". Anything else is a
            // terminal, a script, or another program driving us.
            name == "explorer.exe"
        }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Download, verify, and (with consent) launch the official rustup installer,
/// then print the commands that add the toolchain Spass builds with.
fn run_install_rust(o: &Opts) -> i32 {
    step("Rust toolchain");

    if rustup::rust_on_path() {
        ok("cargo is already on PATH");
        if !confirm(o, "    Download rustup anyway? [y/N] ") {
            note("Keeping the existing toolchain.");
            return 0;
        }
    }

    step("Downloading the official rustup installer");
    note("  https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe");
    let (path, sha) = match rustup::fetch_verified() {
        rustup::Verified::Ok { path, sha } => {
            ok("downloaded rustup-init.exe");
            (path, sha)
        }
        rustup::Verified::Failed(e) => {
            error(&e);
            note("Nothing was executed. Install Rust from https://rustup.rs instead.");
            return 1;
        }
    };

    step("Verifying the download");
    ok(&format!("sha256 {} (matches the published checksum)", &sha[..16]));

    let (install_cmd, default_cmd) = rustup::follow_up_commands();
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    plain("");
    plain("The Rust installer is downloaded and checksum-verified:");
    plain(&format!("  {}", path.display()));
    plain("");
    plain("Spass needs Rust for `spass test` and `spass run`, which compile your");
    plain("Spass code with rustc. To finish, run:");
    plain("");
    plain(&format!("  {}\\rustup-init.exe -y --default-toolchain stable --profile minimal", dir));
    plain(&format!("  {}", install_cmd));
    plain(&format!("  {}", default_cmd));
    plain("");
    plain("The last two select the toolchain Spass builds with. It uses the");
    plain("bundled rust-lld linker, so it needs neither Visual Studio nor MinGW.");
    plain("");

    if confirm(o, "    Launch rustup-init now? [y/N] ") {
        match Command::new(&path)
            .args(["-y", "--default-toolchain", "stable", "--profile", "minimal"])
            .status()
        {
            Ok(s) if s.success() => ok("rustup-init finished"),
            Ok(s) => {
                error(&format!("rustup-init exited with {}", s));
                return 1;
            }
            Err(e) => {
                error(&format!("could not launch rustup-init: {}", e));
                return 1;
            }
        }
        plain("");
        plain("Now run these two to add the Spass toolchain:");
        plain(&format!("  {}", install_cmd));
        plain(&format!("  {}", default_cmd));
    } else {
        note("Not launched. Run the commands above when you are ready.");
    }
    plain("");
    0
}

fn run_uninstall(o: &Opts) -> i32 {
    step("Uninstalling Spass");
    remove_from_path(o);
    if !o.install_dir.exists() {
        note(&format!("nothing to remove at {}", o.install_dir.display()));
        finish("Spass is not installed.");
        return 0;
    }
    if !confirm(o, &format!("    Remove {} and all its contents? [y/N] ", o.install_dir.display())) {
        note("aborted");
        return 1;
    }
    // A resident daemon holds spass.exe open; stop it first so
    // the delete succeeds instead of failing with a sharing violation.
    kill_daemons();
    // The file association points at the binary that is about to go.
    if assoc::unregister(o.scope) {
        ok("removed the .saple file association");
    } else {
        note("could not remove the .saple file association");
    }
    if let Err(e) = fs::remove_dir_all(&o.install_dir) {
        error(&format!("could not remove {}: {}", o.install_dir.display(), e));
        return 1;
    }
    ok(&format!("removed {}", o.install_dir.display()));
    finish("Spass uninstalled.");
    0
}

fn kill_daemons() {
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "spass.exe"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn run_install(o: &Opts) -> i32 {
    step("Installing Spass");

    if o.install_dir.join("spass.exe").exists()
        && !confirm(o, &format!("    Overwrite the existing install at {}? [y/N] ", o.install_dir.display()))
    {
        note("aborted");
        return 1;
    }

    kill_daemons();
    if o.install_dir.exists() {
        if let Err(e) = fs::remove_dir_all(&o.install_dir) {
            error(&format!("could not clear {}: {}", o.install_dir.display(), e));
            return 1;
        }
    }
    if let Err(e) = fs::create_dir_all(&o.install_dir) {
        error(&format!("could not create {}: {}", o.install_dir.display(), e));
        return 1;
    }

    let dest = o.install_dir.join("spass.exe");
    if let Err(e) = fs::write(&dest, SPASS_EXE) {
        error(&format!("could not write {}: {}", dest.display(), e));
        return 1;
    }
    ok("spass.exe");

    // `libunwind.dll` must land next to spass.exe or the binary dies before
    // main with STATUS_DLL_NOT_FOUND. See the note in build.rs.
    if let Some(dll) = SPASS_LIBUNWIND {
        let p = o.install_dir.join("libunwind.dll");
        if let Err(e) = fs::write(&p, dll) {
            error(&format!("could not write {}: {}", p.display(), e));
            return 1;
        }
        ok("libunwind.dll");
    } else {
        note("libunwind.dll not bundled - spass.exe needs the Rust toolchain's bin dir on PATH");
    }

    // The self-hosted numeric core travels with the install so
    // the bootstrap provenance is inspectable on the target machine.
    if let Some(src) = SPASS_BOOTSTRAP {
        let dir = o.install_dir.join("bootstrap");
        if fs::create_dir_all(&dir).is_ok() {
            let p = dir.join("bootstrap.spass");
            if fs::write(&p, src).is_ok() {
                ok("bootstrap\\bootstrap.spass");
            }
        }
    }

    // Verify the installed binary actually starts before declaring success.
    step("Verifying");
    match Command::new(&dest).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout);
            ok(&format!("version {}", v.trim()));
        }
        Ok(out) => {
            error(&format!(
                "spass.exe --version exited with {}",
                out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into())
            ));
            return 1;
        }
        Err(e) => {
            error(&format!("could not run the installed spass.exe: {}", e));
            return 1;
        }
    }

    if o.add_to_path {
        add_to_path(o);
    }

    // A `.saple` package is opened by the runner, so Explorer has to know
    // what to do with one.
    if assoc::register(&dest, o.scope) {
        ok("registered the .saple file association");
    } else {
        note("could not register the .saple file association");
    }

    plain("");
    plain("Spass is installed.");
    plain(&format!("  location : {}", o.install_dir.display()));
    if o.add_to_path {
        plain("  next     : open a new terminal, then run `spass --help`");
    } else {
        plain(&format!("  run now  : \"{}\" --help", dest.display()));
    }
    plain("");
    0
}

// ---- PATH via the registry, resolved at runtime --------------------------
// We cannot link `advapi32` statically: the
// `x86_64-pc-windows-gnullvm` toolchain this project builds with ships a
// minimal self-contained import-library set (kernel32, user32, ws2_32,
// ntdll, dbghelp, userenv, mingw32/32) with no `libadvapi32.a`, so an
// `#[link(name = "advapi32")]` fails at build time with
// "unable to find library -ladvapi32".
//
// Instead we resolve the registry entry points at run time through
// `LoadLibraryExW` / `GetProcAddress`, which live in kernel32 - an import
// library we do have. This keeps spass-setup.exe a single dependency-free
// binary with no PowerShell or reg.exe subprocess.
//
// Only `HKEY_CURRENT_USER\Environment` is touched, so the installer never
// needs elevation and never triggers UAC.

#[cfg(windows)]
mod reg {
    use std::ffi::c_void;

    type Farproc = *mut c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(lpfilename: *const u16, hfile: *mut c_void, dwflags: u32) -> *mut c_void;
        fn GetProcAddress(hmodule: *mut c_void, lpprocname: *const u8) -> Farproc;
    }

    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    pub type RegQueryValueExW =
        unsafe extern "system" fn(isize, *const u16, *mut u32, *mut u32, *mut u8, *mut u32) -> i32;
    pub type RegSetValueExW =
        unsafe extern "system" fn(isize, *const u16, u32, u32, *const u8, u32) -> i32;
    pub type RegCloseKey = unsafe extern "system" fn(isize) -> i32;
    pub type RegOpenKeyExW =
        unsafe extern "system" fn(u32, *const u16, u32, u32, *mut isize) -> i32;
    pub type RegCreateKeyExW = unsafe extern "system" fn(
        isize,
        *const u16,
        u32,
        *const u16,
        u32,
        u32,
    *mut c_void,
    *mut isize,
    *mut u32,
    ) -> i32;
    pub type RegDeleteKeyW = unsafe extern "system" fn(isize, *const u16) -> i32;
    fn load_symbol<T: Copy>(name: &[u8]) -> Option<T> {
        unsafe {
            let lib: Vec<u16> = "advapi32.dll\0".encode_utf16().collect();
            let module = LoadLibraryExW(
                lib.as_ptr(),
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
    }

    /// `(RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, RegCloseKey)`.
    ///
    /// `RegQueryValueExW` is the raw reader: it returns the unexpanded
    /// REG_EXPAND_SZ bytes, so `%VAR%` entries survive the
    /// read-modify-write cycle in `current_user_path` / `set_current_user_path`.
    ///
    /// The previous code asked for `RegGetValueExW`, which is not a Windows
    /// API at all, so `GetProcAddress` failed with ERROR_PROC_NOT_FOUND (127)
    /// and *every* PATH read returned `None` - the install and uninstall
    /// paths silently skipped the PATH edit while still reporting success.
    /// (`RegGetValueW` is a real but different function: seven parameters,
    /// not six, and it expands `%VAR%` unless `RRF_NOEXPAND` is passed.)
    pub fn symbols() -> Option<(RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, RegCloseKey)> {
        let open: RegOpenKeyExW = load_symbol(b"RegOpenKeyExW\0")?;
        let get: RegQueryValueExW = load_symbol(b"RegQueryValueExW\0")?;
        let set: RegSetValueExW = load_symbol(b"RegSetValueExW\0")?;
        let close: RegCloseKey = load_symbol(b"RegCloseKey\0")?;
        Some((open, get, set, close))
    }

    /// `(RegCreateKeyExW, RegSetValueExW, RegCloseKey, RegDeleteKeyW)`.
    ///
    /// The file association needs keys that do not exist yet, and the
    /// uninstall has to take them away again; the PATH edit needs
    /// neither.
    pub fn assoc_symbols() -> Option<(RegCreateKeyExW, RegSetValueExW, RegCloseKey, RegDeleteKeyW)> {
        let create: RegCreateKeyExW = load_symbol(b"RegCreateKeyExW\0")?;
        let set: RegSetValueExW = load_symbol(b"RegSetValueExW\0")?;
        let close: RegCloseKey = load_symbol(b"RegCloseKey\0")?;
        let delete: RegDeleteKeyW = load_symbol(b"RegDeleteKeyW\0")?;
        Some((create, set, close, delete))
    }
}

pub(crate) const HKEY_CURRENT_USER: u32 = 0x8000_0001;
pub(crate) const HKEY_LOCAL_MACHINE: u32 = 0x8000_0002;
pub(crate) const KEY_QUERY_VALUE: u32 = 0x0001;
pub(crate) const KEY_SET_VALUE: u32 = 0x0002;
/// `KEY_SET_VALUE | KEY_CREATE_SUB_KEY`: what a key that has to be created
/// needs.
pub(crate) const KEY_WRITE: u32 = 0x0006;
pub(crate) const REG_EXPAND_SZ: u32 = 2;
pub(crate) const REG_SZ: u32 = 1;

/// The registry hive and key that hold PATH for a given scope.
fn path_hive(scope: Scope) -> (u32, &'static str) {
    match scope {
        Scope::User => (HKEY_CURRENT_USER, "Environment"),
        Scope::Machine => (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    }
}

/// Read the raw, *unexpanded* PATH for `scope`. `RegQueryValueExW` hands back
/// the raw REG_EXPAND_SZ bytes, so any `%VAR%` references survive a
/// read-modify-write cycle. Reading the `PATH` environment variable instead
/// would return the merged, already-expanded machine+user value, and
/// writing that back would permanently freeze those references.
fn current_path(scope: Scope) -> Option<String> {
    #[cfg(not(windows))]
    {
        let _ = scope;
        None
    }
    #[cfg(windows)]
    {
        use std::mem::size_of;
        let (open, get, _set, close) = reg::symbols()?;
        let (hive, sub_key) = path_hive(scope);
        let sub: Vec<u16> = format!("{}\0", sub_key).encode_utf16().collect();
        let mut h: isize = 0;
        if unsafe { open(hive, sub.as_ptr(), 0, KEY_QUERY_VALUE, &mut h) } != 0 {
            return None;
        }
        let name: Vec<u16> = "Path\0".encode_utf16().collect();
        let mut size: u32 = 0;
        // First call asks for the byte length. `RegQueryValueExW` takes
        // `lpReserved` (must be NULL) before the type out-param.
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
        let s = String::from_utf16_lossy(&utf16);
        if s.is_empty() { None } else { Some(s) }
    }
}

/// Write PATH for `scope` back as REG_EXPAND_SZ so `%VAR%` entries stay
/// expandable across future edits.
fn set_path(scope: Scope, value: &str) -> bool {
    #[cfg(not(windows))]
    {
        let _ = (scope, value);
        false
    }
    #[cfg(windows)]
    {
        let (open, _get, set, close) = match reg::symbols() {
            Some(s) => s,
            None => return false,
        };
        let (hive, sub_key) = path_hive(scope);
        let sub: Vec<u16> = format!("{}\0", sub_key).encode_utf16().collect();
        let mut h: isize = 0;
        if unsafe { open(hive, sub.as_ptr(), 0, KEY_SET_VALUE, &mut h) } != 0 {
            return false;
        }
        let name: Vec<u16> = "Path\0".encode_utf16().collect();
        let mut data: Vec<u8> = value.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        data.extend_from_slice(&[0, 0]);
        let rc = unsafe { set(h, name.as_ptr(), 0, REG_EXPAND_SZ, data.as_ptr(), data.len() as u32) };
        unsafe { close(h) };
        rc == 0
    }
}

/// Tell running apps (notably Explorer) that the environment changed so a
/// new shell picks up the PATH without a sign-out. Best-effort.
fn broadcast_env_change() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn SendMessageTimeoutW(
                hwnd: isize, msg: u32, wparam: usize, lparam: usize,
                flags: u32, timeout: u32, out: *mut usize,
            ) -> usize;
        }
        const HWND_BROADCAST: isize = 0xffff;
        const WM_SETTINGCHANGE: u32 = 0x001A;
        const SMTO_ABORTIFHUNG: u32 = 0x0002;
        let s: Vec<u16> = "Environment\0".encode_utf16().collect();
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST, WM_SETTINGCHANGE, 0, s.as_ptr() as usize,
                SMTO_ABORTIFHUNG, 2000, std::ptr::null_mut(),
            );
        }
    }
}


fn same_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('\\').to_ascii_lowercase();
    norm(a) == norm(&b.display().to_string())
}

fn add_to_path(o: &Opts) {
    let which = match o.scope {
        Scope::User => "your user PATH",
        Scope::Machine => "the system PATH",
    };
    step(&format!("Adding to {}", which));
    let current = current_path(o.scope).unwrap_or_default();
    let present: Vec<&str> = current.split(';').filter(|s| !s.trim().is_empty()).collect();
    if present.iter().any(|e| same_dir(e, &o.install_dir)) {
        ok("already on PATH");
        return;
    }
    let mut entries: Vec<String> = present.iter().map(|s| s.to_string()).collect();
    entries.push(o.install_dir.display().to_string());
    if set_path(o.scope, &entries.join(";")) {
        broadcast_env_change();
        ok(&format!("added to {}", which));
    } else {
        note("could not update PATH automatically (registry write denied)");
        note(&format!("add it manually: {}", o.install_dir.display()));
    }
}

fn remove_from_path(o: &Opts) {
    let current = match current_path(o.scope) {
        Some(c) => c,
        None => return,
    };
    let before: Vec<&str> = current.split(';').filter(|s| !s.trim().is_empty()).collect();
    let kept: Vec<&str> = before.iter().copied().filter(|s| !same_dir(s, &o.install_dir)).collect();
    if kept.len() == before.len() {
        return;
    }
    if set_path(o.scope, &kept.join(";")) {
        broadcast_env_change();
        ok("removed from PATH");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_values_parse() {
        assert_eq!(parse_scope("user").unwrap(), Scope::User);
        assert_eq!(parse_scope("ME").unwrap(), Scope::User);
        assert_eq!(parse_scope("machine").unwrap(), Scope::Machine);
        assert_eq!(parse_scope("All-Users").unwrap(), Scope::Machine);
        assert!(parse_scope("bogus").is_err());
    }

    #[test]
    fn shell_join_quotes_spaced_args() {
        let args = vec![
            "--install-dir".to_string(),
            r"C:\Program Files\Spass".to_string(),
            "--scope".to_string(),
            "machine".to_string(),
        ];
        assert_eq!(
            shell_join(&args),
            "--install-dir \"C:\\Program Files\\Spass\" --scope machine"
        );
    }

    #[test]
    fn shell_join_escapes_quotes() {
        let args = vec!["say\"hi\"".to_string()];
        assert_eq!(shell_join(&args), "\"say\\\"hi\\\"\"");
    }

    #[test]
    fn scope_defaults_differ() {
        let u = Scope::User.default_dir();
        let m = Scope::Machine.default_dir();
        assert_ne!(u, m);
        assert!(u.ends_with("Spass"));
        assert!(m.ends_with("Spass"));
    }
}

fn step(m: &str) { ui::reporter().step(m); }
fn ok(m: &str) { ui::reporter().ok(m); }
fn note(m: &str) { ui::reporter().note(m); }
fn error(m: &str) { ui::reporter().error(m); }
fn finish(m: &str) { ui::reporter().finish(m); }
fn plain(m: &str) { ui::reporter().plain(m); }



