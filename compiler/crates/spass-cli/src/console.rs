//! Draws terminal-safe progress output on Windows and Unix.

use std::io::Write;

/// The width of `text` in terminal cells.
///
/// Every glyph the readout uses is single-width, so the character count
/// is the display width - except the colour escapes, which take up
/// characters but no cells.
pub fn width(text: &str) -> usize {
    let mut cells = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // A control sequence: ESC '[' parameters, then one final byte.
            // It moves the cursor and sets attributes; it draws nothing.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        cells += 1;
    }
    cells
}

/// Whether the terminal can show colour.
///
/// The readout only highlights the part of the scale the head has
/// passed when the terminal understands the escapes; otherwise the
/// escapes would print as literal text.
pub fn supports_color() -> bool {
    #[cfg(windows)]
    {
        win::supports_color()
    }
    #[cfg(not(windows))]
    {
        use std::io::IsTerminal;
        let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        std::io::stderr().is_terminal() && !dumb
    }
}

/// Whether stderr is a terminal that can be rewritten in place.
pub fn is_live() -> bool {
    #[cfg(windows)]
    {
        win::is_console()
    }
    #[cfg(not(windows))]
    {
        use std::io::IsTerminal;
        std::io::stderr().is_terminal()
    }
}

/// Rewrite the current line of stderr, leaving the cursor at its end.
///
/// `previous` is the width of the line being replaced; the difference is
/// padded with spaces so no fragment of it survives.
pub fn rewrite(text: &str, previous: usize) -> usize {
    write_raw(&line_bytes(text, previous));
    width(text)
}

/// The bytes that replace the current line.
///
/// A carriage return puts the cursor at the start, and the padding
/// covers whatever the previous line was longer by.
pub fn line_bytes(text: &str, previous: usize) -> String {
    let pad = previous.saturating_sub(width(text));
    let mut line = String::with_capacity(text.len() + pad + 1);
    line.push('\r');
    line.push_str(text);
    line.extend(std::iter::repeat_n(' ', pad));
    line
}

/// Print a line to stderr.
pub fn print_line(text: &str) -> usize {
    let mut line = String::with_capacity(text.len() + 1);
    line.push_str(text);
    line.push('\n');
    write_raw(&line);
    width(text)
}

/// Erase the current line of stderr and leave the cursor at its start.
pub fn erase(previous: usize) {
    let mut line = String::with_capacity(previous + 2);
    line.push('\r');
    line.extend(std::iter::repeat_n(' ', previous));
    line.push('\r');
    write_raw(&line);
}

/// Write to stderr, in whatever encoding the destination understands.
fn write_raw(text: &str) {
    #[cfg(windows)]
    {
        if win::is_console() {
            // A console that refuses the write gets nothing rather than a
            // line of mis-decoded bytes.
            win::write_console(text);
            return;
        }
    }
    let mut err = std::io::stderr();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
        fn WriteConsoleW(
            handle: *mut c_void,
            buffer: *const u16,
            chars: u32,
            written: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
    }

    /// `STD_ERROR_HANDLE`.
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF8;

    /// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`: the console interprets the
    /// escape sequences instead of printing them.
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    fn stderr() -> *mut c_void {
        unsafe { GetStdHandle(STD_ERROR_HANDLE) }
    }

    pub fn is_console() -> bool {
        let handle = stderr();
        if handle.is_null() || handle as isize == -1 {
            return false;
        }
        let mut mode = 0u32;
        unsafe { GetConsoleMode(handle, &mut mode) != 0 }
    }

    /// Whether the console interprets escape sequences.
    ///
    /// The flag is off by default on a Windows console, so it is turned
    /// on here rather than assumed. A console that refuses the call
    /// (Windows 8, a restricted host) falls back to the plain form.
    pub fn supports_color() -> bool {
        let handle = stderr();
        if handle.is_null() || handle as isize == -1 {
            return false;
        }
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return false;
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return true;
        }
        unsafe {
            SetConsoleMode(
                handle,
                mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            ) != 0
        }
    }

    pub fn write_console(text: &str) -> bool {
        let handle = stderr();
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let mut written = 0u32;
        let ok = unsafe {
            WriteConsoleW(
                handle,
                utf16.as_ptr(),
                utf16.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        ok != 0 && written as usize == utf16.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rewritten line starts at the beginning of the line and covers
    /// whatever the previous line was longer by - otherwise the tail of
    /// a longer line stays on screen.
    #[test]
    fn a_rewritten_line_covers_the_previous_one() {
        let bytes = line_bytes("ab", 5);
        assert_eq!(bytes, "\rab   ");
        assert_eq!(line_bytes("abcdef", 3), "\rabcdef");
    }

    /// The width is counted in characters, not bytes: the glyphs the
    /// readout draws are multi-byte in UTF-8.
    #[test]
    fn the_width_is_counted_in_characters() {
        assert_eq!(width("▶▸·"), 3);
        assert_eq!(width("ab"), 2);
    }

    /// A colour escape occupies characters but no cells. Counting it
    /// would make the line look longer than it is and erase the wrong
    /// number of characters.
    #[test]
    fn the_width_skips_the_colour_escapes() {
        assert_eq!(width("\x1b[36m──\x1b[0m"), 2);
        assert_eq!(width("\x1b[1;97m▶\x1b[0m"), 1);
        assert_eq!(width("\x1b[2;36m•\x1b[0m"), 1);
    }
}