# Repository Layout

```
compiler/crates/spass-lexer          tokenizer
compiler/crates/spass-parser         AST + parser (include expansion happens here)
compiler/crates/spass-semantic       symbol table, type checking, TypedModule
compiler/crates/spass-infer          range/value-domain inference, rational model
compiler/crates/spass-transform      if_present/@dispatch rewrites, emission plan
compiler/crates/spass-optimize       optional compile-time reductions (off by default)
compiler/crates/spass-codegen        Rust + C emitters, runtime prelude
compiler/crates/spass-runner         .saple reader, native-module loader, module ABI
compiler/crates/spass-cli            the spass binary
compiler/crates/spass-runtime        the C runtime source, inlined into every module
compiler/crates/spass-bootstrap-core self-hosting numeric core
compiler/crates/spass-installer      MSI packaging (not .saple)
compiler/crates/spass-playground     playground runtime appended to generated Rust
compiler/crates/spass-bench          criterion benchmarks (dev-dependency only)
compiler/scripts/                    measurement and verification scripts
examples/<feature>/                  one project per language feature
installer/                           WiX source and MSI build script
```

## Self-hosting constraint

`spass-bootstrap-core/build.rs` runs the entire parse → semantic → codegen
pipeline inside a build script, on a thread with a 512 MB stack. A codegen
change can therefore break the compiler's own build, and the failure appears
as a build-script panic in an unrelated place.

## Runtime sections

The code generator emits only the runtime a program actually uses. It decides
which of `vec_mat`, `spasio`, `tasks`, `sequences`, and `playground` a module
needs, and both emitters gate on that decision. The playground section alone
is roughly 52 KB of Rust (39 KB of C).

A runtime helper emitted only when its section is on is not available to a
program outside that section. Adding a call to a section-specific helper from
a sectionless module produces generated code that does not compile.

## The ponytail marker

Comments use `ponytail` to mean a deliberate, deferred improvement — a known
gap with a stated reason. A `ponytail` comment is not an undiscovered bug.
