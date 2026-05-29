//! `memlog-witness` — long-running consumer that drains `/dev/memlog` into
//! per-session snapshot files under `~/.claude/memlog/<session-id>/`.
//!
//! # Usage
//!
//! ```text
//! memlog-witness daemon [--out <dir>] [--device <path>] [--quota <bytes>]
//! memlog-witness status
//! memlog-witness drain --session <id>
//! ```

use libmemlog::lock::WitnessLock;
use libmemlog::parse_records;
use libmemlog::persistence::{write_session_meta, SessionWriter};
use libmemlog::DEVICE_PATH;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

// ── CLI parsing ───────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Cmd {
    Daemon {
        out: PathBuf,
        device: String,
        quota: u64,
    },
    Status {
        out: PathBuf,
    },
    Drain {
        out: PathBuf,
        session: String,
    },
}

fn default_out() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude").join("memlog")
}

fn parse_args() -> Result<Cmd, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Err("Usage: memlog-witness <daemon|status|drain> [options]".into());
    }
    match args[0].as_str() {
        "daemon" => {
            let mut out = default_out();
            let mut device = DEVICE_PATH.to_string();
            let mut quota: u64 = 100 * 1024 * 1024; // 100 MB
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--out" => {
                        i += 1;
                        out = PathBuf::from(args.get(i).ok_or("--out requires a value")?);
                    }
                    "--device" => {
                        i += 1;
                        device = args.get(i).ok_or("--device requires a value")?.clone();
                    }
                    "--quota" => {
                        i += 1;
                        quota = args
                            .get(i)
                            .ok_or("--quota requires a value")?
                            .parse::<u64>()
                            .map_err(|e| format!("--quota: {e}"))?;
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
                i += 1;
            }
            Ok(Cmd::Daemon { out, device, quota })
        }
        "status" => {
            let out = if args.len() > 2 && args[1] == "--out" {
                PathBuf::from(&args[2])
            } else {
                default_out()
            };
            Ok(Cmd::Status { out })
        }
        "drain" => {
            let mut out = default_out();
            let mut session = String::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--session" => {
                        i += 1;
                        session = args.get(i).ok_or("--session requires a value")?.clone();
                    }
                    "--out" => {
                        i += 1;
                        out = PathBuf::from(args.get(i).ok_or("--out requires a value")?);
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
                i += 1;
            }
            if session.is_empty() {
                return Err("drain requires --session <id>".into());
            }
            Ok(Cmd::Drain { out, session })
        }
        other => Err(format!("unknown command: {other}")),
    }
}

// ── session-id derivation ─────────────────────────────────────────────────────

/// Derive a string session-id from the header's 16-byte field.
/// If non-zero bytes exist, interpret as UTF-8 (null-trimmed) or hex.
/// Otherwise fall back to "comm:unknown:pid:<pid>", then try to read
/// `/proc/<pid>/agent_session` if the process is still alive.
fn derive_session_id(session_id_bytes: &[u8; 16], pid: u32) -> String {
    if !session_id_bytes.iter().all(|&b| b == 0) {
        let s = std::str::from_utf8(session_id_bytes)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        if !s.is_empty() {
            return s;
        }
        return hex_encode(session_id_bytes);
    }
    // Try /proc/<pid>/agent_session.
    let proc_path = format!("/proc/{pid}/agent_session");
    if let Ok(sid) = fs::read_to_string(&proc_path) {
        let sid = sid.trim().to_string();
        if !sid.is_empty() {
            return sid;
        }
    }
    format!("comm:unknown:pid:{pid}")
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ── daemon ────────────────────────────────────────────────────────────────────

fn run_daemon(out: &Path, device: &str, quota: u64) -> io::Result<()> {
    // Single-instance guard.
    fs::create_dir_all(out)?;
    match WitnessLock::try_acquire(out)? {
        None => {
            eprintln!("memlog-witness: already running (lock held by another process)");
            process::exit(0);
        }
        Some(_lock) => {
            // Lock is held; proceeds below.
            do_daemon_loop(out, device, quota)
        }
    }
}

fn do_daemon_loop(out: &Path, device: &str, quota: u64) -> io::Result<()> {
    eprintln!("memlog-witness: starting daemon, device={device}, out={out:?}");
    let mut writers: HashMap<String, SessionWriter> = HashMap::new();

    // Open the device/fixture file.
    let mut file = std::fs::File::open(device)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let records = parse_records(&buf);
    for rec in records {
        let sid = derive_session_id(&rec.header.session_id, rec.header.pid);
        let session_dir = out.join(&sid);
        let w = writers.entry(sid.clone()).or_insert_with(|| {
            let w = SessionWriter::open(&session_dir, quota).expect("open session dir");
            write_session_meta(&session_dir, &sid).ok();
            w
        });
        let json = format!(
            "{{\"seq\":{},\"ts_ns\":{},\"uid\":{},\"pid\":{},\"blob_len\":{}}}",
            rec.header.seq,
            rec.header.ts_ns,
            rec.header.uid,
            rec.header.pid,
            rec.blob.len(),
        );
        w.write_snap(json.as_bytes())?;
        eprintln!(
            "memlog-witness: wrote snap for session={sid} seq={}",
            rec.header.seq
        );
    }
    eprintln!("memlog-witness: device drained (EOF)");
    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────────

fn run_status(out: &Path) -> io::Result<()> {
    if !out.exists() {
        println!("no sessions (out_dir {out:?} does not exist)");
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(out)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("no sessions");
        return Ok(());
    }

    println!("{:<40} {:>6} {:>12}", "session-id", "snaps", "bytes");
    println!("{}", "-".repeat(62));
    for entry in entries {
        let sid = entry.file_name().to_string_lossy().to_string();
        let dir = entry.path();
        let (snap_count, total_bytes) = count_snaps(&dir);
        println!("{:<40} {:>6} {:>12}", sid, snap_count, total_bytes);
    }
    Ok(())
}

fn count_snaps(dir: &Path) -> (usize, u64) {
    let snaps: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("snap-") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    let total: u64 = snaps
        .iter()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    (snaps.len(), total)
}

// ── drain ─────────────────────────────────────────────────────────────────────

fn run_drain(out: &Path, session: &str) -> io::Result<()> {
    let session_dir = out.join(session);
    if !session_dir.exists() {
        eprintln!("memlog-witness: session {session} has no directory at {session_dir:?}");
        return Ok(());
    }
    // Flush by doing a sync on the directory itself.
    let d = fs::File::open(&session_dir)?;
    d.sync_all()?;
    eprintln!("memlog-witness: fsynced {session_dir:?}");
    Ok(())
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cmd = parse_args().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    let result = match &cmd {
        Cmd::Daemon { out, device, quota } => run_daemon(out, device, *quota),
        Cmd::Status { out } => run_status(out),
        Cmd::Drain { out, session } => run_drain(out, session),
    };

    if let Err(e) = result {
        eprintln!("memlog-witness: fatal: {e}");
        process::exit(1);
    }
}
