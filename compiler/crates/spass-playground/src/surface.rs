//! Defines the playground builtin surface shared by the semantic pass and both runtimes.

use std::collections::BTreeMap;

/// The playground language surface: the Spass-level name of every
/// builtin and the number of parameters it takes.
pub const SURFACE: &[(&str, usize)] = &[
    // --- spass2d ---
    ("s2d:window", 3),
    ("s2d:open", 0),
    ("s2d:present", 0),
    ("s2d:clear", 3),
    ("s2d:pixel", 6),
    ("s2d:line", 8),
    ("s2d:rect", 8),
    ("s2d:frame", 8),
    ("s2d:circle", 7),
    ("s2d:ellipse", 8),
    ("s2d:tri", 10),
    ("s2d:text", 8),
    ("s2d:text_width", 2),
    ("s2d:text_height", 1),
    ("s2d:label", 4),
    ("s2d:button", 6),
    ("s2d:slider", 8),
    ("s2d:checkbox", 5),
    ("s2d:mouse_x", 0),
    ("s2d:mouse_y", 0),
    ("s2d:mouse_dx", 0),
    ("s2d:mouse_dy", 0),
    ("s2d:mouse_down", 0),
    ("s2d:wheel", 0),
    ("s2d:key", 1),
    ("s2d:time", 0),
    ("s2d:dt", 0),
    // --- spass3d ---
    ("s3d:window", 3),
    ("s3d:open", 0),
    ("s3d:present", 0),
    ("s3d:clear", 3),
    ("s3d:camera", 7),
    ("s3d:light", 3),
    ("s3d:depth", 1),
    ("s3d:tri", 12),
    ("s3d:quad", 15),
    ("s3d:box", 9),
    ("s3d:grid", 5),
    ("s3d:push", 0),
    ("s3d:pop", 0),
    ("s3d:translate", 3),
    ("s3d:rotate", 3),
    ("s3d:scale", 3),
    ("s3d:sphere", 8),
    ("s3d:cylinder", 9),
    ("s3d:mouse_x", 0),
    ("s3d:mouse_y", 0),
    ("s3d:mouse_dx", 0),
    ("s3d:mouse_dy", 0),
    ("s3d:mouse_down", 0),
    ("s3d:key", 1),
    ("s3d:time", 0),
    ("s3d:dt", 0),
    // --- spio ---
    ("spio:print", 1),
    ("spio:print_num", 1),
    ("spio:read_text", 1),
    ("spio:write_text", 2),
    ("spio:append_text", 2),
    ("spio:read_line", 2),
    ("spio:line_count", 1),
    ("spio:replace_text", 3),
    ("spio:read_f64", 1),
    ("spio:write_f64", 2),
    ("spio:append_f64", 2),
    ("spio:exists", 1),
    ("spio:size", 1),
    ("spio:remove", 1),
    ("spio:mkdir", 1),
    ("spio:copy", 2),
    ("spio:rename", 2),
    ("spio:list_count", 1),
    ("spio:list_name", 2),
];

/// The runtime symbol a Spass-level name maps to: `s2d:window` becomes
/// `spass_s2d_window`.
pub fn symbol(name: &str) -> String {
    format!("spass_{}", name.replace(':', "_"))
}

/// Strip comments and string literals, so a scan cannot trip over prose
/// that mentions a symbol.
pub fn strip_noise(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if c == '"' {
            i += 1;
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            out.push('"');
            continue;
        }
        if c == '\'' {
            // A character literal is a quote, one character (possibly
            // escaped) and a quote. Anything else is a lifetime.
            let len = if i + 1 < n && chars[i + 1] == '\\' { 4 } else { 3 };
            let closed = i + len <= n && chars[i + len - 1] == '\'';
            if closed {
                i += len;
                out.push('\'');
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The parameter count of the call whose opening parenthesis is at
/// `open`, and the index just past its closing parenthesis.
///
/// A trailing comma does not make another parameter, and C's `(void)`
/// means no parameters at all.
fn param_count(src: &str, open: usize) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut count = 0usize;
    let mut has_segment = false;
    let mut close = open;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                close = i;
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            b',' if depth == 1 => {
                if has_segment {
                    count += 1;
                }
                has_segment = false;
                i += 1;
            }
            _ => {
                if depth == 1 && !bytes[i].is_ascii_whitespace() {
                    has_segment = true;
                }
                i += 1;
            }
        }
    }
    if has_segment {
        count += 1;
    }
    if src[open + 1..close].trim() == "void" {
        count = 0;
    }
    (count, i)
}

/// The `spass_*` exports of the Rust runtime, with their parameter
/// counts.
pub fn rust_exports(src: &str) -> BTreeMap<String, usize> {
    let src = strip_noise(src);
    let mut out = BTreeMap::new();
    let mut i = 0;
    while let Some(rel) = src[i..].find("pub fn spass_") {
        let name_start = i + rel + "pub fn ".len();
        let open = match src[name_start..].find('(') {
            Some(p) => name_start + p,
            None => break,
        };
        let name = src[name_start..open].trim().to_string();
        let (count, end) = param_count(&src, open);
        out.insert(name, count);
        i = end;
    }
    out
}

/// The `spass_*` exports of the C runtime, with their parameter counts.
pub fn c_exports(src: &str) -> BTreeMap<String, usize> {
    let src = strip_noise(src);
    let bytes = src.as_bytes();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 6 <= bytes.len() {
        if &bytes[i..i + 6] == b"spass_" {
            let name_start = i + 6;
            let mut end = name_start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > name_start && end < bytes.len() && bytes[end] == b'(' {
                let name = src[i..end].to_string();
                let (count, after) = param_count(&src, end);
                out.insert(name, count);
                i = after;
                continue;
            }
            i = name_start;
            continue;
        }
        i += 1;
    }
    out
}

/// The C helpers the runtime calls before it defines them.
///
/// The C runtime has no forward declarations, so a call to a helper
/// that is defined further down is a compile error - and one that no
/// local test would catch, because the C backend is not built here.
pub fn c_forward_references(src: &str) -> Vec<String> {
    let src = strip_noise(src);
    let bytes = src.as_bytes();
    let mut defs: BTreeMap<String, usize> = BTreeMap::new();
    let mut refs: Vec<(String, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"spg_") {
            let name_start = i + 4;
            let mut end = name_start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > name_start && end < bytes.len() && bytes[end] == b'(' {
                let name = src[name_start..end].to_string();
                let (_, after) = param_count(&src, end);
                // A definition is followed by the body; a call is not.
                let mut j = after;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'{' {
                    defs.entry(name).or_insert(i);
                } else {
                    refs.push((name, i));
                }
                i = after;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::new();
    for (name, pos) in refs {
        match defs.get(&name) {
            Some(def) if *def < pos => {}
            _ => out.push(format!("{name} (offset {pos})")),
        }
    }
    out
}

/// The brackets that are left open in a source file.
pub fn unbalanced_brackets(src: &str) -> Vec<char> {
    let src = strip_noise(src);
    let mut stack: Vec<char> = Vec::new();
    for c in src.chars() {
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                let expected = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.pop() {
                    Some(open) if open == expected => {}
                    _ => return vec![c],
                }
            }
            _ => {}
        }
    }
    stack
}

/// The C keywords, so a scan does not mistake one for a call.
const C_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "do", "else", "case", "default", "return", "break",
    "continue", "goto", "sizeof", "defined", "static", "const", "struct", "union", "enum",
    "unsigned", "signed", "void", "int", "char", "long", "short", "float", "double",
    "typedef", "extern", "inline", "restrict", "volatile", "register", "auto",
];

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The functions the C runtime calls that it does not define itself.
///
/// A call to a function the file does not define needs a declaration
/// from a header, and the file's own includes are the only source of
/// one. The set is therefore worth keeping in the open: a new external
/// call has to be declared deliberately.
pub fn c_external_calls(src: &str) -> std::collections::BTreeSet<String> {
    let src = strip_noise(src);
    let bytes = src.as_bytes();
    let mut defined: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut called: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        let name = src[start..i].to_string();
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            continue;
        }
        let (_, after) = param_count(&src, j);
        let mut k = after;
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        // A definition is followed by its body; a call is not.
        if k < bytes.len() && bytes[k] == b'{' {
            defined.insert(name);
        } else {
            called.insert(name);
        }
    }
    called.retain(|n| !defined.contains(n));
    called.retain(|n| !C_KEYWORDS.contains(&n.as_str()));
    called
}
