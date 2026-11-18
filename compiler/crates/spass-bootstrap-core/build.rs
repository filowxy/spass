//! Compiles the Spass numeric core into Rust with the compiler itself.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // The front-end + codegen pipeline recurses over the AST; run it on
    // a dedicated thread with a large stack so a build script's default
    // stack can never be the limiting factor. `ponytail`: this mirrors
    // what rustc itself does for its own compile loop; the real fix for
    // any future genuine deep-recursion is bounding the AST depth.
    let child = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(generate)
        .expect("spawn build-script worker");
    match child.join() {
        Ok(()) => {}
        Err(panic) if panic.downcast_ref::<&str>().is_some() => {
            panic!("{}", panic.downcast_ref::<&str>().unwrap())
        }
        Err(panic) if panic.downcast_ref::<String>().is_some() => {
            panic!("{}", panic.downcast_ref::<String>().unwrap())
        }
        Err(_) => panic!("build-script worker panicked"),
    }
}

fn generate() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // compiler/crates/spass-bootstrap-core -> repo root -> examples/.
    let src_path = manifest.join("../../../examples/bootstrap/src/bootstrap.spass");
    println!("cargo:rerun-if-changed={}", src_path.display());

    let src = fs::read_to_string(&src_path)
        .unwrap_or_else(|e| panic!("cannot read Spass core library {}: {}", src_path.display(), e));

    let module = spass_parser::Parser::parse_source(&src, "bootstrap.spass")
        .unwrap_or_else(|e| panic!("Spass core library failed to parse: {}", e));
    let typed = spass_semantic::analyze(&module)
        .unwrap_or_else(|e| panic!("Spass core library failed semantic analysis: {}", e));
    let rs = spass_codegen::generate_rust(&typed)
        .unwrap_or_else(|e| panic!("Spass core library failed codegen: {}", e));

    // Keep the generated runtime prelude (math builtins like
    // `spass_floor` / `spass_abs`, vec helpers, multi-return ABI
    // structs) and only strip the argv dispatch `main` - any builtin
    // the core library ever calls stays available, and the extra code
    // is dead-code-allowed inside the private `gen` module.
    let main_start = rs
        .find("\nfn main()")
        .unwrap_or_else(|| panic!("dispatch main not found in generated bootstrap code"));
    let body = &rs[..main_start];
    if body.contains("fn main(") {
        panic!("bootstrap extraction accidentally captured the dispatch main");
    }
    // Inner attributes (`#![allow(...)]`) are illegal inside a macro
    // expansion - the `gen` module in lib.rs already carries the same
    // allows. Strip every leading `#![...]` line.
    let body = body
        .lines()
        .skip_while(|l| l.trim_start().starts_with("#![") || l.trim_start().is_empty()
            || l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("bootstrap_gen.rs");
    fs::write(&out_path, body)
        .unwrap_or_else(|e| panic!("cannot write {}: {}", out_path.display(), e));
}
