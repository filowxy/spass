//! Reads the user-level optimiser configuration, separate from project config.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use spass_optimize::reduce::Switches;

/// The name of the environment variable that carries the level.
pub const ENV_LEVEL: &str = "SPASS_OPTIMIZE";

/// The configuration Spass reads for itself.
#[derive(Debug, Clone)]
pub struct BodyConfig {
    /// The switches that decide which reductions run.
    pub switches: Switches,
    /// How deep a constant expression may be before the evaluator gives
    /// up.
    pub max_fold_depth: usize,
    /// Whether the optimiser's report is printed.
    pub diagnostics: bool,
}

impl Default for BodyConfig {
    fn default() -> Self {
        BodyConfig {
            switches: Switches::default(),
            max_fold_depth: 8,
            diagnostics: false,
        }
    }
}

impl BodyConfig {
    /// Whether any reduction runs.
    pub fn is_enabled(&self) -> bool {
        self.switches.readonly_locals
            || self.switches.inline_single_use
            || self.switches.fold_constants
            || self.switches.const_fold_arith
            || self.switches.dead_code_elim
    }

    /// The switches of a level.
    ///
    /// * 0 - everything off.
    /// * 1 - conservative: only the locals that are never reassigned stop
    /// being mutable. Nothing about the arithmetic changes.
    /// * 2 - standard: the locals are reduced, the constants are folded.
    /// A folded float is the exact rational value, which is not always
    /// the number the runtime would have produced.
    /// * 3 - aggressive: everything, including dropping the functions
    /// nothing references. A host that looks a symbol up by name stops
    /// finding it.
    pub fn switches_for_level(level: u8) -> Switches {
        match level {
            0 => Switches::default(),
            1 => Switches { readonly_locals: true, ..Switches::default() },
            2 => Switches {
                readonly_locals: true,
                inline_single_use: true,
                fold_constants: true,
                const_fold_arith: true,
                dead_code_elim: false,
            },
            _ => Switches {
                readonly_locals: true,
                inline_single_use: true,
                fold_constants: true,
                const_fold_arith: true,
                dead_code_elim: true,
            },
        }
    }

    /// Read Spass's own configuration file.
    ///
    /// A missing file is not an error: the default is a valid
    /// configuration.
    pub fn load() -> Result<BodyConfig> {
        let path = config_path();
        if !path.exists() {
            return Ok(BodyConfig::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        BodyConfig::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Read a configuration from its text.
    pub fn from_str(text: &str) -> Result<BodyConfig> {
        let raw = crate::config::parse_toml(text)?;
        let mut cfg = BodyConfig::default();

        if let Some(value) = raw.int("optimize", "level") {
            match value {
                Some(i) if (0..=3).contains(&i) => {
                    cfg.switches = BodyConfig::switches_for_level(i as u8);
                }
                Some(_) => bail!("optimize.level must be 0, 1, 2 or 3"),
                None => bail!("optimize.level must be a number"),
            }
        }
        if let Some(value) = raw.int("optimize", "max_fold_depth") {
            match value {
                Some(i) if i >= 0 => cfg.max_fold_depth = i as usize,
                Some(_) => bail!("optimize.max_fold_depth must not be negative"),
                None => bail!("optimize.max_fold_depth must be a number"),
            }
        }
        if let Some(value) = raw.bool("optimize", "diagnostics") {
            match value {
                Some(b) => cfg.diagnostics = b,
                None => bail!("optimize.diagnostics must be true or false"),
            }
        }
        // The individual switches win over the level.
        if let Some(value) = raw.bool("optimize", "readonly_locals") {
            match value {
                Some(b) => cfg.switches.readonly_locals = b,
                None => bail!("optimize.readonly_locals must be true or false"),
            }
        }
        if let Some(value) = raw.bool("optimize", "inline_single_use") {
            match value {
                Some(b) => cfg.switches.inline_single_use = b,
                None => bail!("optimize.inline_single_use must be true or false"),
            }
        }
        if let Some(value) = raw.bool("optimize", "fold_constants") {
            match value {
                Some(b) => cfg.switches.fold_constants = b,
                None => bail!("optimize.fold_constants must be true or false"),
            }
        }
        if let Some(value) = raw.bool("optimize", "const_fold_arith") {
            match value {
                Some(b) => cfg.switches.const_fold_arith = b,
                None => bail!("optimize.const_fold_arith must be true or false"),
            }
        }
        if let Some(value) = raw.bool("optimize", "dead_code_elim") {
            match value {
                Some(b) => cfg.switches.dead_code_elim = b,
                None => bail!("optimize.dead_code_elim must be true or false"),
            }
        }
        Ok(cfg)
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, Copy, Default)]
pub struct OptimizeRequest {
    /// `--optimize`: turn the optimiser on.
    pub enable: bool,
    /// `--no-optimize`: turn it off.
    pub disable: bool,
    /// `--optimize-level N`.
    pub level: Option<u8>,
    /// `--diagnostics`: print what the optimiser did.
    pub diagnostics: bool,
}

/// Resolve the configuration: command line, then environment, then the
/// file, then the default.
pub fn resolve(request: OptimizeRequest) -> Result<BodyConfig> {
    let mut cfg = BodyConfig::load()?;

    // The environment sits between the command line and the file.
    if let Ok(raw) = std::env::var(ENV_LEVEL) {
        let raw = raw.trim();
        if !raw.is_empty() {
            cfg.switches = switches_from_text(raw)?;
        }
    }

    if let Some(level) = request.level {
        if request.disable {
            bail!("--no-optimize and --optimize-level contradict each other");
        }
        cfg.switches = BodyConfig::switches_for_level(level);
    } else if request.disable {
        cfg.switches = Switches::default();
    } else if request.enable && !cfg.is_enabled() {
        // `--optimize` with nothing configured means the standard level.
        cfg.switches = BodyConfig::switches_for_level(2);
    }
    if request.diagnostics {
        cfg.diagnostics = true;
    }
    Ok(cfg)
}

/// Read a level or a list of switch names.
///
/// `2` is the standard level; `readonly_locals,fold_constants` names the
/// switches one by one.
fn switches_from_text(raw: &str) -> Result<Switches> {
    if let Ok(level) = raw.parse::<u8>() {
        if level > 3 {
            bail!("{ENV_LEVEL} must be 0, 1, 2, 3 or a list of switch names");
        }
        return Ok(BodyConfig::switches_for_level(level));
    }
    let mut switches = Switches::default();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part {
            "readonly_locals" => switches.readonly_locals = true,
            "inline_single_use" => switches.inline_single_use = true,
            "fold_constants" => switches.fold_constants = true,
            "const_fold_arith" => switches.const_fold_arith = true,
            "dead_code_elim" => switches.dead_code_elim = true,
            other => bail!("unknown optimiser switch `{other}` in {ENV_LEVEL}"),
        }
    }
    Ok(switches)
}

/// The path of Spass's own configuration file.
pub fn config_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join("AppData").join("Local")
            });
        base.join("Spass").join("config.toml")
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".config")
            });
        base.join("spass").join("config.toml")
    }
}

/// The configuration Spass reads, for the `--config-dump` readout.
pub fn describe() -> String {
    let path = config_path();
    format!("{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_everything_off() {
        let cfg = BodyConfig::default();
        assert!(!cfg.is_enabled());
        assert!(!cfg.switches.readonly_locals);
        assert!(!cfg.switches.dead_code_elim);
    }

    #[test]
    fn the_levels_pick_the_switches() {
        assert!(!BodyConfig::switches_for_level(0).readonly_locals);
        assert!(BodyConfig::switches_for_level(1).readonly_locals);
        assert!(!BodyConfig::switches_for_level(1).fold_constants);
        assert!(BodyConfig::switches_for_level(2).fold_constants);
        assert!(!BodyConfig::switches_for_level(2).dead_code_elim);
        assert!(BodyConfig::switches_for_level(3).dead_code_elim);
    }

    #[test]
    fn the_file_is_read() {
        let text = r#"
[optimize]
level = 2
max_fold_depth = 12
diagnostics = true
"#;
        let cfg = BodyConfig::from_str(text).expect("parse");
        assert!(cfg.switches.fold_constants);
        assert_eq!(cfg.max_fold_depth, 12);
        assert!(cfg.diagnostics);
    }

    /// An individual switch wins over the level.
    #[test]
    fn a_switch_wins_over_the_level() {
        let text = r#"
[optimize]
level = 3
dead_code_elim = false
"#;
        let cfg = BodyConfig::from_str(text).expect("parse");
        assert!(cfg.switches.fold_constants);
        assert!(!cfg.switches.dead_code_elim);
    }

    #[test]
    fn a_bad_value_is_an_error() {
        assert!(BodyConfig::from_str("[optimize]\nlevel = 9").is_err());
        assert!(BodyConfig::from_str("[optimize]\nlevel = \"two\"").is_err());
        assert!(BodyConfig::from_str("[optimize]\nfold_constants = 1").is_err());
    }

    #[test]
    fn the_environment_takes_a_level_or_a_list() {
        let s = switches_from_text("2").expect("level");
        assert!(s.fold_constants);
        let s = switches_from_text("readonly_locals,fold_constants").expect("list");
        assert!(s.readonly_locals);
        assert!(s.fold_constants);
        assert!(!s.dead_code_elim);
        assert!(switches_from_text("nonsense").is_err());
    }
}
