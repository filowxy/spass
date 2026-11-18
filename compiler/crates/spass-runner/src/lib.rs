//! Loads `.saple` modules and calls into them.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use anyhow::Result;

pub mod loader;
pub mod saple;

pub use loader::{Library, Symbol};
pub use saple::{Native, Saple};

/// The symbol a native module exports to start its sequences.
pub const START_SYMBOL: &str = "spass_module_start";

/// The symbol a native module exports to wait for its sequences.
pub const WAIT_SYMBOL: &str = "spass_module_wait";

/// The symbol a native module exports to call one of its functions.
pub const CALL_SYMBOL: &str = "spass_module_call";

/// Start the module: run the sequences it declares.
///
/// `0` means the module started. A non-zero value means it did not.
pub type StartFn = unsafe extern "C" fn() -> i32;

/// Wait for the module's sequences to finish.
pub type WaitFn = unsafe extern "C" fn();

/// Call an exported function by its mangled symbol name.
///
/// `symbol` is the mangled name (UTF-8, `symbol_len` bytes long, not
/// NUL-terminated). `args` holds the numeric arguments; `argc` is
/// their count. `outs` receives the results: the `out` parameters in
/// declaration order, then the return value. `out_cap` is how many
/// slots `outs` has.
///
/// The return value is `0` on success and `-1` when the symbol is
/// unknown. `out_cap` bounds every write, so a buffer that is too
/// small loses results but never overruns.
pub type CallFn = unsafe extern "C" fn(
    symbol: *const u8,
    symbol_len: usize,
    args: *const f64,
    argc: usize,
    outs: *mut f64,
    out_cap: usize,
) -> i32;

/// The platform tag of the native module this build produces.
///
/// `windows_x64`, `linux_x64`, `macos_arm64`,... - the stem of the
/// file name the native module has inside a `.saple`.
pub fn platform_tag() -> String {
    format!("{}_{}", std::env::consts::OS, arch_tag()).to_lowercase()
}

/// The name the native module has inside a `.saple`.
///
/// `native/<platform>.<ext>` - the extension follows the platform:
/// `.dll` on Windows, `.so` on Linux, `.dylib` on macOS.
pub fn native_entry_name(platform: &str) -> String {
    let extension = if platform.starts_with("windows") {
        "dll"
    } else if platform.starts_with("macos") {
        "dylib"
    } else {
        "so"
    };
    format!("native/{}.{}", platform, extension)
}

/// The directory the runner extracts native modules into.
///
/// `%LOCALAPPDATA%\Spass\run-cache` on Windows, `~/.cache/spass/run-cache`
/// elsewhere. `SPASS_RUNNER_CACHE` overrides it.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SPASS_RUNNER_CACHE") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("Spass").join("run-cache");
            }
        }
    } else if let Some(home) = home_dir() {
        return home.join(".cache").join("spass").join("run-cache");
    }
    // Neither the override nor the home directory is known: the
    // temporary directory is the last place that is writable.
    std::env::temp_dir().join("spass").join("run-cache")
}

/// A native Spass module, loaded and ready to call.
pub struct Module {
    library: Library,
    start: Option<StartFn>,
    wait: Option<WaitFn>,
    call: CallFn,
}

impl Module {
    /// Load a module from a file on disk.
    pub fn load(path: &Path) -> Result<Module> {
        let library = Library::load(path)?;
        let call = library.symbol(CALL_SYMBOL).map_err(|_| {
            anyhow::anyhow!(
                "the module {} does not export `{}`, so it cannot be called",
                path.display(),
                CALL_SYMBOL
            )
        })?;
        // A module without the start symbol has nothing to start, which
        // is not an error.
        let start = library.symbol(START_SYMBOL).ok().map(|symbol| {
            unsafe { std::mem::transmute::<*mut c_void, StartFn>(symbol.as_ptr()) }
        });
        let wait = library.symbol(WAIT_SYMBOL).ok().map(|symbol| {
            unsafe { std::mem::transmute::<*mut c_void, WaitFn>(symbol.as_ptr()) }
        });
        Ok(Module {
            library,
            start,
            wait,
            call: unsafe { std::mem::transmute::<*mut c_void, CallFn>(call.as_ptr()) },
        })
    }

    /// Start the module's sequences. A module without the start symbol
    /// is not an error - it simply has nothing to start.
    pub fn start(&self) -> Result<()> {
        let Some(start) = self.start else {
            return Ok(());
        };
        let code = unsafe { start() };
        if code != 0 {
            anyhow::bail!("the module refused to start (code {})", code);
        }
        Ok(())
    }

    /// Wait for the module's sequences to finish.
    ///
    /// A module without sequences, or without the wait symbol, has
    /// nothing to wait for; both cases return at once.
    pub fn wait(&self) -> Result<()> {
        if let Some(wait) = self.wait {
            unsafe { wait() };
        }
        Ok(())
    }

    /// Call a function by its mangled symbol name.
    ///
    /// Returns the `out` parameters and the return value, in that
    /// order.
    pub fn call(&self, symbol: &str, args: &[f64], out_slots: usize) -> Result<Vec<f64>> {
        let name = symbol.as_bytes();
        let mut outs = vec![0.0f64; out_slots];
        // The module checks both pointers for null: an empty argument
        // list and an empty result list are not a zero-length slice.
        let args_ptr = if args.is_empty() {
            std::ptr::null()
        } else {
            args.as_ptr()
        };
        let outs_ptr = if outs.is_empty() {
            std::ptr::null_mut()
        } else {
            outs.as_mut_ptr()
        };
        let code = unsafe {
            (self.call)(
                name.as_ptr(),
                name.len(),
                args_ptr,
                args.len(),
                outs_ptr,
                out_slots,
            )
        };
        match code {
            0 => Ok(outs),
            -1 => anyhow::bail!("the module does not export the symbol `{}`", symbol),
            other => anyhow::bail!("the call to `{}` failed (code {})", symbol, other),
        }
    }

    /// The path the module was loaded from.
    pub fn path(&self) -> &Path {
        self.library.path()
    }
}

/// The architecture part of the platform tag.
///
/// The tag names a file inside a package rather than a Rust target, so
/// it uses the ABI names `x64` and `arm64`.
fn arch_tag() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// The user's home directory, if the environment names one.
fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_tag_names_the_host() {
        if cfg!(windows) {
            assert_eq!(platform_tag(), "windows_x64");
        }
        if cfg!(target_arch = "x86_64") {
            assert!(platform_tag().ends_with("_x64"));
        }
    }

    #[test]
    fn the_native_entry_follows_the_platform() {
        assert_eq!(
            native_entry_name("windows_x64"),
            "native/windows_x64.dll"
        );
        assert_eq!(native_entry_name("linux_x64"), "native/linux_x64.so");
        assert_eq!(
            native_entry_name("macos_arm64"),
            "native/macos_arm64.dylib"
        );
    }

    #[test]
    fn the_cache_directory_can_be_overridden() {
        let dir = PathBuf::from("run-cache-test");
        std::env::set_var("SPASS_RUNNER_CACHE", &dir);
        let resolved = cache_dir();
        std::env::remove_var("SPASS_RUNNER_CACHE");
        assert_eq!(resolved, dir);
    }

    #[test]
    fn a_module_that_does_not_exist_is_an_error() {
        let path = PathBuf::from("no-such-module.dll");
        let error = match Module::load(&path) {
            Ok(_) => panic!("a module that does not exist must not load"),
            Err(e) => e,
        };
        assert!(
            format!("{:#}", error).contains("no-such-module.dll"),
            "the error has to name the module: {:#}",
            error
        );
    }
}
