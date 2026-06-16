//! Cross-source contract: driver, README, and packaged udev rule must agree on device mode.

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub struct ModeSource {
    pub name: &'static str,
    pub path: PathBuf,
    pub mode: u32,
}

/// Parse the `.mode = 0XYZ` line near the cdev/device-create block in driver/memlog.c.
pub fn parse_driver_mode(src: &str) -> Result<u32, String> {
    // Look for `.mode = 0NNN` pattern (octal literal)
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(".mode") && trimmed.contains('=') {
            let val = trimmed.split('=').nth(1)
                .ok_or("no value after =")?
                .trim()
                .trim_end_matches(',')
                .trim();
            return u32::from_str_radix(val.trim_start_matches('0'), 8)
                .map_err(|e| format!("parse error on {:?}: {}", val, e));
        }
    }
    Err("no .mode = line found".into())
}

/// Parse MODE="NNNN" from a udev rule line containing KERNEL=="memlog".
pub fn parse_udev_rule_mode(src: &str) -> Result<u32, String> {
    for line in src.lines() {
        if line.contains("memlog") && line.contains("MODE=") {
            for part in line.split(',') {
                let p = part.trim();
                if let Some(val) = p.strip_prefix("MODE=\"").and_then(|s| s.strip_suffix('"')) {
                    return u32::from_str_radix(val.trim_start_matches('0'), 8)
                        .map_err(|e| format!("parse error on {:?}: {}", val, e));
                }
            }
        }
    }
    Err("no MODE= found in memlog udev rule line".into())
}

/// Resolve the packaged udev rule path (env override → in-tree → installed).
pub fn resolve_udev_rule_path(repo_root: &Path) -> Option<(PathBuf, &'static str)> {
    if let Ok(env_path) = std::env::var("MEMLOG_UDEV_RULE_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some((p, "env:MEMLOG_UDEV_RULE_PATH"));
        }
    }
    let in_tree = repo_root.join("../wintermute-kernel/pkg/linux-wintermute-memlog.rules");
    if in_tree.exists() {
        return Some((in_tree.canonicalize().unwrap_or(in_tree), "in-tree"));
    }
    let installed = Path::new("/usr/lib/udev/rules.d/70-linux-wintermute-memlog.rules");
    if installed.exists() {
        return Some((installed.to_path_buf(), "installed"));
    }
    None
}
