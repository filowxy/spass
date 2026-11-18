//! Builds the deeppack artifact: a loadable shared library plus its ABI manifest.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use spass_semantic::TypedModule;
use spass_transform::plan::ModulePlan;

/// The library file name. The host looks it up by this name.
pub const LIBRARY_NAME: &str = "spass_module.dll";
/// The manifest that describes the exported symbols.
pub const MANIFEST_NAME: &str = "spass_module.abi.json";
/// The symbol that starts the module's sequences.
pub const START_SYMBOL: &str = "spass_module_start";
/// The symbol that waits for the module's sequences.
pub const WAIT_SYMBOL: &str = "spass_module_wait";
/// The symbol that calls an exported function by its mangled name.
pub const CALL_SYMBOL: &str = "spass_module_call";
/// The environment variable that carries the runner's host.
pub const RUNNER_HOST_ENV: &str = "SPASS_SOCKET_HOST";
/// The environment variable that carries the runner's port.
pub const RUNNER_PORT_ENV: &str = "SPASS_SOCKET_PORT";

/// Compile the module into a shared library and copy the unwinder next
/// to it.
///
/// The unwinder is the Rust toolchain's `libunwind.dll`: the library
/// links against it, and a host that cannot resolve it fails to load
/// the library at all.
pub fn build_library(
    typed: &TypedModule,
    plan: Option<&ModulePlan>,
    build_dir: &Path,
    verbose: bool,
) -> Result<PathBuf> {
    let rs_path = build_dir.join("module_cdylib.rs");
    let lib_path = build_dir.join(LIBRARY_NAME);

    let source = spass_codegen::generate_rust_with_plan(
        typed,
        spass_codegen::RustFlavor::Cdylib,
        plan,
    )
    .context("Rust codegen (cdylib)")?;
    fs::write(&rs_path, &source).context("writing the cdylib Rust output")?;
    if verbose {
        println!("emitted {}", rs_path.display());
    }

    compile_cdylib(&rs_path, &lib_path)?;
    copy_unwinder(build_dir)?;
    Ok(lib_path)
}

/// Copy the toolchain's unwinder next to the library.
pub fn copy_unwinder(build_dir: &Path) -> Result<Vec<String>> {
    let mut requires = Vec::new();
    if let Some(bin) = crate::rustup_toolchain_bin() {
        let unwind = bin.join("libunwind.dll");
        if unwind.exists() {
            let dest = build_dir.join("libunwind.dll");
            fs::copy(&unwind, &dest).context("copying libunwind.dll next to the library")?;
            requires.push("libunwind.dll".to_string());
        }
    }
    Ok(requires)
}

/// Compile the generated module into a shared library.
fn compile_cdylib(rs_path: &Path, lib_path: &Path) -> Result<()> {
    let mut cmd = Command::new("rustc");
    cmd.arg(rs_path)
        .arg("--crate-type")
        .arg("cdylib")
        .arg("-O")
        .arg("-C")
        .arg("codegen-units=1")
        .arg("-C")
        .arg("embed-bitcode=no")
        .arg("-C")
        .arg("linker=rust-lld")
        .arg("-o")
        .arg(lib_path);
    let mut readout = crate::progress::Readout::start("linking the shared library");
    // rustc's diagnostics must not land on the readout's line.
    let output = cmd
        .stderr(std::process::Stdio::piped())
        .output()
        .context("invoking rustc");
    readout.clear();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            readout.failed();
            return Err(e);
        }
    };
    if !output.stderr.is_empty() {
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    if !output.status.success() {
        readout.failed();
        anyhow::bail!("rustc failed to build the deeppack library");
    }
    readout.done();
    Ok(())
}

/// Write the ABI manifest.
pub fn write_manifest(typed: &TypedModule, build_dir: &Path) -> Result<()> {
    let requires = copy_unwinder(build_dir)?;
    let manifest = manifest_json(typed, &requires);
    let manifest_path = build_dir.join(MANIFEST_NAME);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .context("writing the ABI manifest")?;
    Ok(())
}

/// Build the ABI manifest.
pub fn manifest_json(typed: &TypedModule, requires: &[String]) -> serde_json::Value {
    serde_json::json!({
        "module": "spass_module",
        "version": "0.1.0",
        "compiler": env!("CARGO_PKG_VERSION"),
        "artifact": LIBRARY_NAME,
        "artifact_kind": "cdylib",
        "abi": "c",
        "requires": requires,
        "functions": functions_json(typed),
        "structs": structs_json(typed),
        "runner": runner_section(typed),
    })
}

/// The function table: how every exported function crosses the ABI.
///
/// The `results` field is how many slots a call fills: the `out`
/// parameters in declaration order, then the return value. A runner
/// allocates the buffer from it, so the two have to agree.
pub fn functions_json(typed: &TypedModule) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for f in spass_codegen::abi::export_abis(typed) {
        let results = result_slots(typed, &f.symbol);
        out.push(serde_json::json!({
            "name": f.name,
            "symbol": f.symbol,
            "results": results,
            "params": f.params.iter().map(|p| serde_json::json!({
                "name": p.name,
                "type": p.c_type,
                "is_out": p.is_out,
                "domain": p.domain.as_ref().map(domain_json),
            })).collect::<Vec<_>>(),
            "returns": f.returns.as_ref().map(|r| serde_json::json!({
                "type": r.c_type,
                "domain": r.domain.as_ref().map(domain_json),
            })),
        }));
    }
    out
}

/// The struct table: how every struct is laid out across the ABI.
pub fn structs_json(typed: &TypedModule) -> Vec<serde_json::Value> {
    spass_codegen::abi::struct_abis(typed)
        .into_iter()
        .map(|s| serde_json::json!({ "name": s.name, "fields": s.fields }))
        .collect()
}

/// How many result slots a call of one symbol fills.
///
/// A function with `out` parameters returns nothing the caller can use:
/// the generated dispatcher drops the return value in that case, and
/// the two have to agree.
fn result_slots(typed: &TypedModule, symbol: &str) -> usize {
    let Some(func) = typed.functions.iter().find(|f| {
        spass_codegen::mangle_for_call(f) == symbol
            || spass_codegen::mangle_function_name_variant(
                f,
                spass_transform::if_present::Variant::Present,
            ) == symbol
            || spass_codegen::mangle_function_name_variant(
                f,
                spass_transform::if_present::Variant::Absent,
            ) == symbol
    }) else {
        return 0;
    };
    let outs = func.params.iter().filter(|p| p.is_out).count();
    if outs > 0 {
        return outs;
    }
    if let Some(mr) = &func.multi_returns {
        return mr.len();
    }
    if let Some(spass_parser::ast::Type::Struct(name, _)) = &func.return_type {
        return typed
            .structs
            .iter()
            .find(|s| &s.name == name)
            .map(|s| s.fields.len())
            .unwrap_or(1);
    }
    if func.return_type.is_some() {
        1
    } else {
        0
    }
}

/// The section that tells a host how to reach the runner.
///
/// The module talks to the runner through the spasio protocol, and the
/// runner's address is a *runtime* fact: the environment the module
/// finds in its own process is where the runner has to be. The
/// manifest therefore names the environment variables and the defaults
/// they fall back to, so a host can put the module where the runner is
/// before it loads it.
pub fn runner_section(typed: &TypedModule) -> serde_json::Value {
    serde_json::json!({
        "start_symbol": START_SYMBOL,
        "wait_symbol": WAIT_SYMBOL,
        "call_symbol": CALL_SYMBOL,
        "has_sequences": !typed.sequences.is_empty(),
        "sequences": typed.sequences.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        "host_env": RUNNER_HOST_ENV,
        "port_env": RUNNER_PORT_ENV,
        "default_host": "127.0.0.1",
        "default_port": 9100,
    })
}

fn domain_json(d: &spass_codegen::abi::DomainAbi) -> serde_json::Value {
    serde_json::json!({
        "min": d.min,
        "max": d.max,
        "step": d.step,
        "k": d.k,
        "state_count": d.state_count,
        "bits": d.bits,
        "type": d.c_type,
    })
}
