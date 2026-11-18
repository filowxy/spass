//! Loads a native module into the runner process.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A native module loaded into this process.
pub struct Library {
    path: PathBuf,
    handle: imp::Handle,
}

impl Library {
    /// Load a native module from a file.
    pub fn load(path: &Path) -> Result<Library> {
        let handle = imp::open(path)
            .with_context(|| format!("loading the native module {}", path.display()))?;
        Ok(Library {
            path: path.to_path_buf(),
            handle,
        })
    }

    /// Resolve an exported symbol.
    pub fn symbol(&self, name: &str) -> Result<Symbol> {
        let ptr = imp::symbol(self.handle, name)
            .with_context(|| format!("resolving `{}` in {}", name, self.path.display()))?;
        Ok(Symbol { ptr })
    }

    /// The path the module was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        imp::close(self.handle);
    }
}

/// The address of an exported function.
pub struct Symbol {
    ptr: *mut c_void,
}

impl Symbol {
    /// The address as a raw pointer.
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::anyhow;
    use std::ffi::c_char;
    use std::os::windows::ffi::OsStrExt;

    /// A loaded module handle (`HMODULE`).
    pub type Handle = *mut c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    pub fn open(path: &Path) -> Result<Handle> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            // The code separates the two failures a caller has to tell
            // apart: the file is not there (2), or one of the modules it
            // depends on is not (126).
            let code = unsafe { GetLastError() };
            let reason = std::io::Error::from_raw_os_error(code as i32);
            return Err(anyhow!(
                "the module could not be loaded: Windows error {} ({})",
                code,
                reason
            ));
        }
        Ok(handle)
    }

    pub fn symbol(handle: Handle, name: &str) -> Result<*mut c_void> {
        let c_name =
            std::ffi::CString::new(name).map_err(|_| anyhow!("the symbol name contains a NUL byte"))?;
        let ptr = unsafe { GetProcAddress(handle, c_name.as_ptr()) };
        if ptr.is_null() {
            return Err(anyhow!("the module does not export `{}`", name));
        }
        Ok(ptr)
    }

    pub fn close(handle: Handle) {
        unsafe {
            FreeLibrary(handle);
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use anyhow::anyhow;
    use std::ffi::{c_char, c_int, CString};

    /// A loaded module handle.
    pub type Handle = *mut c_void;

    /// Resolve every undefined symbol when the module is loaded: a
    /// module with a missing dependency has to fail here, not at the
    /// first call into it.
    const RTLD_NOW: c_int = 2;

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
        fn dlerror() -> *mut c_char;
    }

    pub fn open(path: &Path) -> Result<Handle> {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| anyhow!("the module path contains a NUL byte"))?;
        let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return Err(anyhow!(
                "the module could not be loaded: {}",
                last_error()
            ));
        }
        Ok(handle)
    }

    pub fn symbol(handle: Handle, name: &str) -> Result<*mut c_void> {
        let c_name =
            std::ffi::CString::new(name).map_err(|_| anyhow!("the symbol name contains a NUL byte"))?;
        // The pending error has to be cleared before the lookup: a
        // successful `dlsym` does not reset it.
        unsafe {
            dlerror()
        };
        let ptr = unsafe { dlsym(handle, c_name.as_ptr()) };
        if ptr.is_null() {
            return Err(anyhow!("the module does not export `{}`", name));
        }
        Ok(ptr)
    }

    pub fn close(handle: Handle) {
        unsafe {
            dlclose(handle);
        }
    }

    /// The dynamic linker's last error, as text.
    fn last_error() -> String {
        let ptr = unsafe { dlerror() };
        if ptr.is_null() {
            return "no dynamic linker error".to_string();
        }
        unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}
