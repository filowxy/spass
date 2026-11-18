//! Builds `.saple` packages from compiled modules.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use spass_semantic::TypedModule;

/// The format tag in the manifest. The version is part of the tag, so a
/// runner can tell the shapes it does not know from the ones it does.
pub const FORMAT: &str = "saple/2";

/// The name the native module has inside the package.
pub fn native_entry_name(tag: &str) -> String {
    spass_runner::native_entry_name(tag)
}

/// Package the module into a `.saple`.
///
/// `natives` are the machine code entries: the platform tag and the
/// bytes of each. The first one is the module for the platform this
/// build produced; the others travel with it - the unwinder the module
/// links against, for one, has to sit next to it when it is loaded.
pub fn package(
    out: &Path,
    module_name: &str,
    typed: &TypedModule,
    c_source: &str,
    natives: Vec<(String, Vec<u8>)>,
) -> Result<()> {
    let file = fs::File::create(out).context("creating .saple")?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut native_map = serde_json::Map::new();
    let mut platforms: Vec<String> = Vec::new();
    for (tag, bytes) in &natives {
        let entry = if tag == UNWINDING_TAG {
            format!("native/{}", unwinding_file_name())
        } else {
            native_entry_name(tag)
        };
        native_map.insert(tag.clone(), serde_json::Value::String(entry.clone()));
        // The unwinder is not a platform: it is not something a runner
        // loads, it is something the loader resolves next to the module.
        if tag != UNWINDING_TAG {
            platforms.push(tag.clone());
        }
        zip.start_file(&entry, opts)?;
        zip.write_all(bytes)?;
    }

    let mut manifest = manifest_base(module_name, typed);
    manifest["platforms"] = serde_json::json!(platforms);
    manifest["native"] = serde_json::Value::Object(native_map);
    zip.start_file("manifest.json", opts)?;
    zip.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes())?;

    // The C source travels with the package as a reference: a host that
    // has to rebuild the module for its own platform needs it, and the
    // package is the one artifact that reaches the host.
    zip.start_file("native/source.c", opts)?;
    zip.write_all(c_source.as_bytes())?;

    zip.start_file("module.sps", opts)?;
    zip.write_all(module_sps(typed, module_name).as_bytes())?;

    zip.finish().context("finalising .saple")?;
    Ok(())
}

/// The tag of the entry that carries the module's unwinder.
pub const UNWINDING_TAG: &str = "libunwind";

/// The file name of the module's unwinder.
///
/// The unwinder is not a platform: it is what the loader resolves next
/// to the module, and its name follows the platform the module was
/// built for.
fn unwinding_file_name() -> &'static str {
    if cfg!(windows) {
        "libunwind.dll"
    } else {
        "libunwind.so"
    }
}

/// The manifest fields that do not depend on the package's contents.
///
/// The loose `build/manifest.json` and the manifest inside the
/// `.saple` describe the same module; building them from one function
/// keeps the two from drifting apart. The package adds the platform
/// list and the native entry table on top.
pub fn manifest_base(module_name: &str, typed: &TypedModule) -> serde_json::Value {
    serde_json::json!({
        "format": FORMAT,
        "module": module_name,
        "version": "0.1.0",
        "compiler": env!("CARGO_PKG_VERSION"),
        "entry": default_entry(typed),
        "functions": crate::deeppack::functions_json(typed),
        "structs": crate::deeppack::structs_json(typed),
        "channels": channels_json(typed),
        "runner": crate::deeppack::runner_section(typed),
    })
}

/// The function a package runs when the caller does not name one.
///
/// A sequence is an entry point of its own: it runs when the module
/// starts, not through a call. Picking one as the default call would
/// execute the same body twice - once as a thread and once inline -
/// so sequences are not eligible.
fn default_entry(typed: &TypedModule) -> Option<String> {
    typed
        .functions
        .iter()
        .find(|f| {
            spass_codegen::is_public_export(f)
                && !typed.sequences.iter().any(|s| s.name == f.name)
        })
        .map(|f| f.name.clone())
}

/// The channel table: which channels the program reads and writes.
///
/// A host decides from this whether the module has to be pointed at a
/// runner before it is started.
fn channels_json(typed: &TypedModule) -> Vec<serde_json::Value> {
    typed
        .channels
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.channel,
                "mode": channel_mode(&c.mode),
                "direction": channel_dir(&c.dir),
            })
        })
        .collect()
}

/// The deterministic schema file (`module.sps`).
///
/// The file is line-oriented and free of Rust debug formatting and
/// absolute paths: a runner or an external client parses it, so it has
/// to be stable across machines and compiler runs. Each function block
/// carries the source-level type and the ABI type side by side -
/// the source type is what the program says, the ABI type is what
/// crosses the boundary.
pub fn module_sps(typed: &TypedModule, module_name: &str) -> String {
    let mut out = String::new();
    out.push_str("schema: spass_module_v1\n");
    out.push_str(&format!("module: {}\n", module_name));
    out.push_str(&format!("compiler: {}\n", env!("CARGO_PKG_VERSION")));

    for export in spass_codegen::abi::export_abis(typed) {
        out.push_str(&format!("\nfunction {}\n", export.name));
        out.push_str(&format!("  symbol {}\n", export.symbol));
        // An `if_present` function exports two symbols under one
        // source name; the variant tells the caller which branch it
        // is looking at.
        if export.symbol.ends_with("_present") {
            out.push_str("  variant present\n");
        } else if export.symbol.ends_with("_absent") {
            out.push_str("  variant absent\n");
        }
        for p in &export.params {
            out.push_str(&format!(
                "  param {}{} {}{}\n",
                p.name,
                if p.is_out { " out" } else { "" },
                p.c_type,
                domain_suffix(p.domain.as_ref()),
            ));
        }
        match &export.returns {
            Some(r) => out.push_str(&format!(
                "  result {}{}\n",
                r.c_type,
                domain_suffix(r.domain.as_ref()),
            )),
            None => out.push_str("  result void\n"),
        }
        out.push_str("end\n");
    }

    for s in &typed.structs {
        out.push_str(&format!("\nstruct {}\n", s.name));
        for (name, ty) in &s.fields {
            out.push_str(&format!("  field {} {}\n", name, ty.display_name()));
        }
        out.push_str("end\n");
    }

    if !typed.channels.is_empty() {
        out.push_str("\n# channels (mode, direction, name)\n");
        for c in &typed.channels {
            out.push_str(&format!(
                "channel: {} {} {}\n",
                channel_mode(&c.mode),
                channel_dir(&c.dir),
                c.channel,
            ));
        }
    }
    out
}

/// The domain annotation of one ABI value, or nothing when the value
/// has no value-domain.
fn domain_suffix(domain: Option<&spass_codegen::abi::DomainAbi>) -> String {
    match domain {
        None => String::new(),
        Some(d) => format!(
            " domain=({}, {}, {}) k={} states={} bits={} abi={}",
            d.min, d.max, d.step, d.k, d.state_count, d.bits, d.c_type,
        ),
    }
}

fn channel_mode(mode: &spass_parser::ast::SpasioMode) -> &'static str {
    match mode {
        spass_parser::ast::SpasioMode::File => "file",
        spass_parser::ast::SpasioMode::Socket => "socket",
        spass_parser::ast::SpasioMode::Memory => "memory",
    }
}

fn channel_dir(dir: &spass_semantic::ChannelDir) -> &'static str {
    match dir {
        spass_semantic::ChannelDir::Input => "input",
        spass_semantic::ChannelDir::Output => "output",
    }
}

/// The mangled symbol of the function the caller asked for.
///
/// The caller may name the function the way the source spells it
/// (`height`) or the way the ABI spells it (`_spass_height_2_f_f`); the
/// manifest's function table has both.
pub fn resolve_symbol(manifest: &serde_json::Value, func: Option<&str>) -> Result<String> {
    let functions = manifest
        .get("functions")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow::anyhow!("the package manifest has no function table"))?;
    match func {
        None => {
            let entry = manifest
                .get("entry")
                .and_then(|e| e.as_str())
                .ok_or_else(|| anyhow::anyhow!("the package names no entry function"))?;
            resolve_one(functions, entry)
        }
        Some(name) => resolve_one(functions, name),
    }
}

fn resolve_one(functions: &[serde_json::Value], name: &str) -> Result<String> {
    for f in functions {
        let fname = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let symbol = f.get("symbol").and_then(|s| s.as_str()).unwrap_or("");
        if fname == name || symbol == name {
            return Ok(symbol.to_string());
        }
    }
    anyhow::bail!("the package exports no function called `{}`", name)
}

/// How many result slots the call of a function fills.
///
/// The `out` parameters come first, then the return value. A function
/// with `out` parameters returns nothing the caller can use, so its
/// slots are the parameters alone - the same rule the generated
/// dispatcher follows.
pub fn result_slots(manifest: &serde_json::Value, symbol: &str) -> usize {
    manifest
        .get("functions")
        .and_then(|f| f.as_array())
        .and_then(|functions| {
            functions.iter().find(|f| {
                f.get("symbol").and_then(|s| s.as_str()) == Some(symbol)
            })
        })
        .and_then(|f| f.get("results"))
        .and_then(|r| r.as_u64())
        .unwrap_or(0) as usize
}
