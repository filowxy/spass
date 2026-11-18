// Downloads and installs the Rust toolchain with checksum verification.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::sha256;

/// The official bootstrapper. It can install any host toolchain, including
/// the `gnullvm` one Spass needs.
const RUSTUP_URL: &str =
    "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe";
const RUSTUP_SHA_URL: &str =
    "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe.sha256";

/// The toolchain Spass actually builds with.
///
/// A plain `rustup-init` defaults to the MSVC host, which needs Visual
/// Studio's `link.exe`; the plain GNU host needs MinGW gcc. `gnullvm` plus
/// the `llvm-tools` component and the bundled `rust-lld` linker needs
/// neither.
pub const SPASS_TOOLCHAIN: &str = "stable-x86_64-pc-windows-gnullvm";

/// PS prelude: TLS 1.2 is requested explicitly because older PowerShell
/// defaults to SSL3/TLS1.0, which static.rust-lang.org rejects.
const PS_TLS: &str = "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12";

/// Run a PowerShell snippet, returning stdout or a readable error.
fn ps(script: &str) -> Result<String, String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("could not run PowerShell: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            format!("PowerShell exited with {}", out.status)
        } else {
            err.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Download with byte-progress reporting.
///
/// `Invoke-WebRequest` gives no incremental progress, so the transfer is
/// driven through an `HttpWebRequest` stream that reports after each chunk.
/// A 12 MB download with a dead progress bar reads as a hang, which is why
/// this is worth the extra code. The sentinel prefix keeps progress lines
/// distinguishable from anything PowerShell itself might emit.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; {tls}; \
         [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; \
         $req = [Net.HttpWebRequest]::Create('{url}'); \
         $req.Timeout = 120000; \
         $res = $req.GetResponse(); \
         $total = $res.ContentLength; \
         $in = $res.GetResponseStream(); \
         $out = [IO.File]::Create('{dest}'); \
         $buf = New-Object byte[] 65536; \
         $done = 0; \
         while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {{ \
           $out.Write($buf, 0, $n); $done += $n; \
           Write-Output ('__SPASS_PROGRESS__ ' + $done + ' ' + $total); \
         }} \
         $out.Close(); $in.Close(); $res.Close();",
        tls = PS_TLS,
        url = url,
        dest = dest.display()
    );

    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run PowerShell: {}", e))?;

    // Drain stdout on this thread so the pipe never fills and deadlocks.
    use std::io::{BufRead, BufReader};
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if let Some(rest) = line.strip_prefix("__SPASS_PROGRESS__ ") {
            let mut it = rest.split_whitespace();
            let done = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let total = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            crate::ui::reporter().progress(done, total);
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not wait for PowerShell: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(if err.trim().is_empty() {
            format!("download failed (exit {})", out.status)
        } else {
            err.trim().to_string()
        });
    }
    Ok(())
}

fn fetch_text(url: &str) -> Result<String, String> {
    // `Invoke-WebRequest` on PS 5.1 returns `.Content` as a byte array for
    // some content types, so decode explicitly to get the text.
    let script = format!(
        "$ErrorActionPreference='Stop'; {tls}; \
         $r = Invoke-WebRequest -Uri '{url}' -UseBasicParsing; \
         if ($r.Content -is [byte[]]) {{ [Text.Encoding]::UTF8.GetString($r.Content) }} \
         else {{ [string]$r.Content }}",
        tls = PS_TLS,
        url = url
    );
    ps(&script)
}

/// Extract the hex digest from the published `.sha256` body, which is a
/// bare 64-char hex digest followed by a newline. Anything else is rejected
/// rather than guessed at.
fn parse_sha256(body: &str) -> Option<String> {
    let token = body.split_whitespace().next()?;
    let t = token.trim().to_ascii_lowercase();
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(t)
    } else {
        None
    }
}

/// True when a working `cargo` is already on PATH.
pub fn rust_on_path() -> bool {
    Command::new("cargo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Result of the verification step, so the caller can decide what to do.
pub enum Verified {
    /// The download matched the published checksum.
    Ok { path: std::path::PathBuf, sha: String },
    /// Verification failed; any downloaded file was removed.
    Failed(String),
}

/// Download `rustup-init.exe` and verify it against the published SHA-256.
/// Nothing is executed here.
pub fn fetch_verified() -> Verified {
    let dir = std::env::temp_dir().join("spass-rustup");
    if let Err(e) = fs::create_dir_all(&dir) {
        return Verified::Failed(format!("could not create {}: {}", dir.display(), e));
    }
    let exe = dir.join("rustup-init.exe");

    if let Err(e) = download(RUSTUP_URL, &exe) {
        return Verified::Failed(e);
    }

    let body = match fetch_text(RUSTUP_SHA_URL) {
        Ok(b) => b,
        Err(e) => {
            let _ = fs::remove_file(&exe);
            return Verified::Failed(format!("could not fetch the published checksum: {}", e));
        }
    };
    let expected = match parse_sha256(&body) {
        Some(h) => h,
        None => {
            let _ = fs::remove_file(&exe);
            return Verified::Failed(
                "the checksum file was not a 64-character hex digest".to_string(),
            );
        }
    };
    let actual = match sha256::hash_file(&exe) {
        Ok(h) => h,
        Err(e) => {
            let _ = fs::remove_file(&exe);
            return Verified::Failed(format!("could not hash the download: {}", e));
        }
    };
    if actual != expected {
        let _ = fs::remove_file(&exe);
        return Verified::Failed(format!(
            "checksum mismatch\n       expected {}\n       actual   {}",
            expected, actual
        ));
    }
    Verified::Ok { path: exe, sha: actual }
}

/// The follow-up commands the user needs for a Spass-capable toolchain.
pub fn follow_up_commands() -> (String, String) {
    (
        format!(
            "rustup toolchain install {} --component llvm-tools",
            SPASS_TOOLCHAIN
        ),
        format!("rustup default {}", SPASS_TOOLCHAIN),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_digest() {
        assert_eq!(
            parse_sha256("6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e\n"),
            Some("6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e".to_string())
        );
    }

    #[test]
    fn parses_with_surrounding_whitespace_and_crlf() {
        assert_eq!(
            parse_sha256("\r\n  6F4BEF66261261FCB43131BE8720BAB817D403A09EDEC7455C371974B90BDB7E  \r\n"),
            Some("6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e".to_string())
        );
    }

    /// A digest with the filename appended (the `sha256sum` CLI format) is
    /// still accepted, since the first token is the digest.
    #[test]
    fn parses_sha256sum_cli_format() {
        assert_eq!(
            parse_sha256("6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e  rustup-init.exe\n"),
            Some("6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e".to_string())
        );
    }

    /// Malformed input must be rejected, not silently accepted - accepting
    /// garbage here would defeat the point of verifying the download.
    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_sha256(""), None);
        assert_eq!(parse_sha256("not a hash at all"), None);
        // too short
        assert_eq!(parse_sha256("6f4bef66"), None);
        // right length, non-hex characters
        assert_eq!(parse_sha256(&"z".repeat(64)), None);
    }

    #[test]
    fn spass_toolchain_is_gnullvm() {
        assert!(SPASS_TOOLCHAIN.contains("gnullvm"), "Spass builds with gnullvm");
    }
}
