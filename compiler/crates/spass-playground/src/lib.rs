//! Provides the playground runtime embedded into compiled modules.

pub mod playground;
pub mod surface;

/// Rust playground runtime source, embedded verbatim into `module.rs`.
pub const PLAYGROUND_RS: &str = include_str!("playground.rs");

/// C playground runtime source, embedded verbatim into `module.c`.
pub const PLAYGROUND_C: &str = include_str!("playground.c");

#[cfg(test)]
mod tests {
    use super::surface::{
        c_exports, c_external_calls, c_forward_references, rust_exports, symbol,
        unbalanced_brackets, SURFACE,
    };

    /// The two backends have to offer the same language surface.
    ///
    /// The C backend is not built here - a C compiler is not a
    /// dependency of the toolchain - so the parity is checked on the
    /// source: every builtin of the surface has to be exported by both
    /// runtimes, with the same number of parameters. A missing export
    /// or a changed parameter count is a link error in the generated
    /// module, and nothing else in the workspace would notice it.
    #[test]
    fn both_backends_export_the_whole_surface() {
        let rust = rust_exports(super::PLAYGROUND_RS);
        let c = c_exports(super::PLAYGROUND_C);
        for (name, arity) in SURFACE {
            let sym = symbol(name);
            assert_eq!(
                rust.get(&sym),
                Some(arity),
                "the Rust runtime must export {sym} with {arity} parameters"
            );
            assert_eq!(
                c.get(&sym),
                Some(arity),
                "the C runtime must export {sym} with {arity} parameters"
            );
        }
        // Nothing beyond the surface: an export nobody can call is
        // dead weight in every generated module.
        for sym in rust.keys() {
            assert!(
                SURFACE.iter().any(|(name, _)| &symbol(name) == sym),
                "{sym} is exported by the Rust runtime but is not part of the surface"
            );
        }
        for sym in c.keys() {
            assert!(
                SURFACE.iter().any(|(name, _)| &symbol(name) == sym),
                "{sym} is exported by the C runtime but is not part of the surface"
            );
        }
    }

    /// The C runtime has no forward declarations, so a call to a helper
    /// that is defined further down does not compile.
    #[test]
    fn the_c_runtime_calls_no_helper_before_it_defines_it() {
        let forward = c_forward_references(super::PLAYGROUND_C);
        assert!(forward.is_empty(), "forward references in playground.c: {forward:?}");
    }

    /// The checker has to catch a forward reference, or it is
    /// decoration.
    #[test]
    fn the_forward_reference_checker_works() {
        let bad = "static int spg_a(void) { return spg_b(); }\nstatic int spg_b(void) { return 1; }\n";
        assert_eq!(c_forward_references(bad).len(), 1);
        let good = "static int spg_b(void) { return 1; }\nstatic int spg_a(void) { return spg_b(); }\n";
        assert!(c_forward_references(good).is_empty());
    }

    /// The functions the C runtime calls from outside itself.
    ///
    /// A call to a function the file does not define needs a
    /// declaration, and the includes are the only place one can come
    /// from - a missing header is a compile error nobody sees, because
    /// the C backend is not built here. Keeping the set in the open
    /// means a new external call has to be declared deliberately.
    const C_EXTERNALS: &[&str] = &[
        // <stdio.h>
        "fclose", "fflush", "fgetc", "fopen", "fprintf", "fputc", "fread", "fscanf", "fseek",
        "ftell", "fwrite", "printf", "remove", "rename", "snprintf",
        // <stdlib.h>
        "abs", "calloc", "free", "getenv", "malloc", "strtoull",
        // <string.h>
        "memcpy", "memset", "strcmp", "strcpy", "strlen", "strrchr", "strstr",
        // <math.h>
        "ceil", "cos", "fabs", "floor", "sin", "sqrt", "tan",
        // <time.h>
        "clock",
        // <direct.h> on Windows, <sys/stat.h> elsewhere
        "_mkdir", "mkdir",
        // <windows.h>
        "CreateWindowExW", "DefWindowProcW", "DestroyWindow", "DispatchMessageW",
        "GetAsyncKeyState", "GetCursorPos", "GetDC", "GetModuleHandleW", "GetTickCount64",
        "PeekMessageW", "PostQuitMessage", "RegisterClassExW", "ReleaseDC", "ScreenToClient",
        "ShowWindow", "Sleep", "StretchDIBits", "TranslateMessage", "UpdateWindow",
    ];

    #[test]
    fn the_c_runtime_only_calls_what_it_declares() {
        let external = c_external_calls(super::PLAYGROUND_C);
        for name in &external {
            assert!(
                C_EXTERNALS.contains(&name.as_str()),
                "{name} is called but not declared: add it to C_EXTERNALS together with the \
                 header that declares it"
            );
        }
        for name in C_EXTERNALS {
            assert!(
                external.contains(*name),
                "{name} is on the list but the runtime does not call it any more"
            );
        }
    }

    /// The headers the runtime needs: the standard library, and the
    /// platform API.
    #[test]
    fn the_c_runtime_includes_the_headers_it_needs() {
        let src = super::PLAYGROUND_C;
        for header in [
            "<limits.h>", "<math.h>", "<stdint.h>", "<stdio.h>", "<stdlib.h>", "<string.h>",
            "<time.h>",
        ] {
            assert!(src.contains(header), "playground.c does not include {header}");
        }
        assert!(src.contains("<windows.h>"), "playground.c does not include <windows.h>");
        assert!(
            src.contains("<direct.h>") && src.contains("<sys/stat.h>"),
            "playground.c needs both spellings of the directory call: <direct.h> and <sys/stat.h>"
        );
    }

    /// A source file that does not balance its brackets does not
    /// compile either.
    #[test]
    fn both_runtime_sources_balance_their_brackets() {
        assert!(
            unbalanced_brackets(super::PLAYGROUND_RS).is_empty(),
            "playground.rs has unbalanced brackets"
        );
        assert!(
            unbalanced_brackets(super::PLAYGROUND_C).is_empty(),
            "playground.c has unbalanced brackets"
        );
    }
}
