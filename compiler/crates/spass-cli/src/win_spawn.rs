//! Spawns child processes without leaking inheritable handles.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Start `program` with `args`, detached from this process's console.
///
/// The child gets the NUL device as its standard handles and inherits no
/// other handle of this process, so it can outlive the call without
/// keeping the caller's redirected output alive.
///
/// `env` holds the variables that differ from this process's own
/// environment; they replace the inherited ones. Windows environment
/// names are case-insensitive, and so is the replacement.
///
/// The returned value is the child's process id.
pub fn spawn_detached(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> std::io::Result<u32> {
    #[cfg(windows)]
    {
        imp::spawn_detached(program, args, env, cwd)
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        Ok(cmd.spawn()?.id())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        lp_reserved: *mut u16,
        lp_desktop: *mut u16,
        lp_title: *mut u16,
        dw_x: u32,
        dw_y: u32,
        dw_x_size: u32,
        dw_y_size: u32,
        dw_x_count_chars: u32,
        dw_y_count_chars: u32,
        dw_fill_attribute: u32,
        dw_flags: u32,
        w_show_window: u16,
        cb_reserved2: u16,
        lp_reserved2: *mut u8,
        h_std_input: *mut c_void,
        h_std_output: *mut c_void,
        h_std_error: *mut c_void,
    }

    #[repr(C)]
    struct ProcessInformation {
        h_process: *mut c_void,
        h_thread: *mut c_void,
        dw_process_id: u32,
        dw_thread_id: u32,
    }

    #[repr(C)]
    struct SecurityAttributes {
        n_length: u32,
        lp_security_descriptor: *mut c_void,
        b_inherit_handle: i32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn CreateProcessW(
            application: *const u16,
            command_line: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit_handles: i32,
            flags: u32,
            environment: *mut c_void,
            current_directory: *const u16,
            startup_info: *mut StartupInfoW,
            process_information: *mut ProcessInformation,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;

    /// The NUL device, opened so that it can be handed to a child as a
    /// standard handle.
    fn nul_device(access: u32) -> std::io::Result<*mut c_void> {
        let security = SecurityAttributes {
            n_length: std::mem::size_of::<SecurityAttributes>() as u32,
            lp_security_descriptor: std::ptr::null_mut(),
            b_inherit_handle: 1,
        };
        let name: Vec<u16> = "NUL\0".encode_utf16().collect();
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security as *const SecurityAttributes as *const c_void,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// The environment block of the child: this process's environment with
    /// `overrides` applied, as a double-null-terminated list of
    /// `NAME=VALUE` pairs.
    ///
    /// The block has to be sorted by name - Windows looks names up in a
    /// binary search over it.
    fn environment_block(overrides: &[(OsString, OsString)]) -> Vec<u16> {
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for (key, value) in overrides {
            let wanted = key.to_string_lossy().to_lowercase();
            vars.retain(|(existing, _)| existing.to_string_lossy().to_lowercase() != wanted);
            vars.push((key.clone(), value.clone()));
        }
        vars.sort_by(|a, b| {
            a.0.to_string_lossy()
                .to_lowercase()
                .cmp(&b.0.to_string_lossy().to_lowercase())
        });
        let mut block: Vec<u16> = Vec::new();
        for (key, value) in vars {
            block.extend(key.encode_wide());
            block.push(b'=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    /// One command-line argument, quoted the way the C runtime expects.
    fn quote(arg: &OsStr) -> OsString {
        let text = arg.to_string_lossy();
        if !text.is_empty()
            && !text.contains(' ')
            && !text.contains('\t')
            && !text.contains('"')
        {
            return arg.to_os_string();
        }
        let mut out = String::from("\"");
        let mut backslashes = 0usize;
        for c in text.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    out.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(backslashes * 2));
        out.push('"');
        OsString::from(out)
    }

    pub fn spawn_detached(
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        cwd: Option<&Path>,
    ) -> std::io::Result<u32> {
        let stdin = nul_device(GENERIC_READ)?;
        let stdout = nul_device(GENERIC_WRITE)?;
        let stderr = nul_device(GENERIC_WRITE)?;

        let mut startup = StartupInfoW {
            cb: std::mem::size_of::<StartupInfoW>() as u32,
            lp_reserved: std::ptr::null_mut(),
            lp_desktop: std::ptr::null_mut(),
            lp_title: std::ptr::null_mut(),
            dw_x: 0,
            dw_y: 0,
            dw_x_size: 0,
            dw_y_size: 0,
            dw_x_count_chars: 0,
            dw_y_count_chars: 0,
            dw_fill_attribute: 0,
            dw_flags: STARTF_USESTDHANDLES,
            w_show_window: 0,
            cb_reserved2: 0,
            lp_reserved2: std::ptr::null_mut(),
            h_std_input: stdin,
            h_std_output: stdout,
            h_std_error: stderr,
        };
        let mut info = ProcessInformation {
            h_process: std::ptr::null_mut(),
            h_thread: std::ptr::null_mut(),
            dw_process_id: 0,
            dw_thread_id: 0,
        };

        let mut command_line: Vec<u16> = Vec::new();
        command_line.extend(quote(program.as_os_str()).encode_wide());
        for arg in args {
            command_line.push(b' ' as u16);
            command_line.extend(quote(arg).encode_wide());
        }
        command_line.push(0);

        let mut environment = environment_block(env);
        let cwd_wide: Vec<u16> = cwd
            .map(|dir| dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect())
            .unwrap_or_default();

        let created = unsafe {
            CreateProcessW(
                // No application name: the first token of the command line
                // is the program, and Windows searches the standard
                // locations for it. Passing it here instead would look
                // for a literal file next to the current directory.
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                // The whole point: no handle of this process is copied.
                0,
                DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                environment.as_mut_ptr() as *mut c_void,
                if cwd_wide.is_empty() {
                    std::ptr::null()
                } else {
                    cwd_wide.as_ptr()
                },
                &mut startup,
                &mut info,
            )
        };
        unsafe {
            CloseHandle(stdin);
            CloseHandle(stdout);
            CloseHandle(stderr);
        }
        if created == 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            CloseHandle(info.h_thread);
            CloseHandle(info.h_process);
        }
        Ok(info.dw_process_id)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// A child that outlives its parent must not keep the parent's output
    /// open.
    ///
    /// The check is end-to-end on purpose: the leak is not about the three
    /// standard handles alone - a shell hands its own pipes down to the
    /// CLI, and those are inherited just the same. So the test points this
    /// process's own stdout at a file and starts a child the way the
    /// runner is started: the child inherits nothing, so its own standard
    /// output is the NUL device and the file stays empty.
    #[test]
    fn a_detached_child_does_not_inherit_the_callers_output() {
        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF7;
        const FILE_FLAG_INHERIT_HANDLE: u32 = 0x0000_0001;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const CREATE_ALWAYS: u32 = 2;

        let dir = std::env::var("CARGO_TARGET_TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let path = dir.join(format!("spass-inherit-{}.txt", std::process::id()));
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let file = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_FLAG_INHERIT_HANDLE,
                std::ptr::null_mut(),
            )
        };
        assert!(!file.is_null(), "the probe file must open");

        let previous = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, file);
        }
        let spawned = spawn_detached(
            Path::new("cmd.exe"),
            &[
                OsString::from("/c"),
                OsString::from("echo leaked"),
            ],
            &[],
            None,
        );
        // The child writes and exits on its own; give it time to do both.
        std::thread::sleep(std::time::Duration::from_millis(800));
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, previous);
            CloseHandle(file);
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        assert!(
            spawned.is_ok(),
            "the detached spawn must succeed: {:?}",
            spawned.err()
        );
        assert!(
            !content.contains("leaked"),
            "the child inherited the caller's stdout: {:?}",
            content
        );
    }
}