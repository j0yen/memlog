//! Atomic per-session snapshot writer.
//!
//! Each compaction event is persisted as `snap-NNNNN.json` inside
//! `<out_dir>/<session_id>/`. Writes are atomic: the blob is first written to
//! `snap.tmp`, fsynced, then renamed into place.  A partial `snap.tmp` left
//! by a crash is silently ignored on the next sequence-number probe.
//!
//! Quota enforcement: when total bytes in the session directory exceed
//! `quota_bytes`, the oldest `snap-*.json` files are removed until the usage
//! drops below quota, and a `_quota-trimmed` marker is created.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Manages writes for one session directory.
pub struct SessionWriter {
    dir: PathBuf,
    next_seq: u64,
    quota_bytes: u64,
}

impl SessionWriter {
    /// Open (creating if necessary) the session directory at `dir`.
    /// Scans for the highest existing `snap-NNNNN.json` to resume the sequence.
    pub fn open<P: AsRef<Path>>(dir: P, quota_bytes: u64) -> io::Result<SessionWriter> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let next_seq = probe_next_seq(&dir);
        Ok(SessionWriter {
            dir,
            next_seq,
            quota_bytes,
        })
    }

    /// Write `json_bytes` as the next snapshot.  Uses a tmp→rename dance so
    /// the result is always either the previous state or the full new file.
    pub fn write_snap(&mut self, json_bytes: &[u8]) -> io::Result<()> {
        let tmp_path = self.dir.join("snap.tmp");
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            f.write_all(json_bytes)?;
            f.sync_data()?;
        }
        let final_path = self.snap_path(self.next_seq);
        fs::rename(&tmp_path, &final_path)?;
        self.next_seq += 1;

        // optional sleep injection for kill-9 tests
        #[cfg(test)]
        if let Ok(ms) = std::env::var("MEMLOG_WITNESS_DELAY_MS") {
            if let Ok(n) = ms.parse::<u64>() {
                std::thread::sleep(std::time::Duration::from_millis(n));
            }
        }

        self.enforce_quota()
    }

    /// Write a sentinel for a ring-overrun event.
    pub fn write_overrun(&mut self, seq_at_overrun: u64) -> io::Result<()> {
        let path = self
            .dir
            .join(format!("_overrun-{seq_at_overrun:05}.json"));
        let payload =
            format!("{{\"overrun\":true,\"ring_seq_at_overrun\":{seq_at_overrun}}}\n");
        atomic_write(&self.dir.join("overrun.tmp"), &path, payload.as_bytes())
    }

    /// Return the path to `snap-NNNNN.json` for `seq`.
    pub fn snap_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("snap-{seq:05}.json"))
    }

    /// Number of successfully written snapshots (max seq written so far).
    pub fn snap_count(&self) -> u64 {
        self.next_seq
    }

    /// Current directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // ── internal ──────────────────────────────────────────────────────────────

    fn enforce_quota(&self) -> io::Result<()> {
        if self.quota_bytes == 0 {
            return Ok(());
        }
        // Collect snap files sorted oldest-first.
        let mut snaps = sorted_snaps(&self.dir)?;
        let mut total = total_bytes(&snaps)?;
        if total <= self.quota_bytes {
            return Ok(());
        }
        // Create marker.
        let marker = self.dir.join("_quota-trimmed");
        if !marker.exists() {
            File::create(&marker)?;
        }
        // Remove oldest until within quota.
        while total > self.quota_bytes && !snaps.is_empty() {
            let oldest = snaps.remove(0);
            let size = oldest.metadata().map(|m| m.len()).unwrap_or(0);
            fs::remove_file(&oldest)?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Find the next snap sequence number by scanning for `snap-NNNNN.json`.
fn probe_next_seq(dir: &Path) -> u64 {
    let max = sorted_snaps(dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("snap-"))
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
        })
        .max();
    max.map(|n| n + 1).unwrap_or(0)
}

/// Atomic write: tmp→fsync→rename.
fn atomic_write(tmp: &Path, dst: &Path, data: &[u8]) -> io::Result<()> {
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)?;
        f.write_all(data)?;
        f.sync_data()?;
    }
    fs::rename(tmp, dst)
}

/// Return `snap-*.json` entries in the directory sorted by filename.
fn sorted_snaps(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut snaps: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("snap-") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    snaps.sort();
    Ok(snaps)
}

/// Sum of sizes of the given paths.
fn total_bytes(paths: &[PathBuf]) -> io::Result<u64> {
    let mut total = 0u64;
    for p in paths {
        total += p.metadata().map(|m| m.len()).unwrap_or(0);
    }
    Ok(total)
}

// ── write_session_meta helpers ────────────────────────────────────────────────

/// Write `intent_tag` and `opened_at` for a newly-opened session (idempotent).
pub fn write_session_meta(dir: &Path, session_id: &str) -> io::Result<()> {
    let tag_path = dir.join("intent_tag");
    if !tag_path.exists() {
        fs::write(&tag_path, session_id)?;
    }
    let ts_path = dir.join("opened_at");
    if !ts_path.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        fs::write(&ts_path, format!("{ts}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn probe_seq_empty_dir() {
        let d = tmp();
        assert_eq!(probe_next_seq(d.path()), 0);
    }

    #[test]
    fn write_snap_creates_file() {
        let d = tmp();
        let mut w = SessionWriter::open(d.path(), 0).unwrap();
        w.write_snap(b"{}").unwrap();
        assert!(d.path().join("snap-00000.json").exists());
        assert_eq!(w.next_seq, 1);
    }

    #[test]
    fn write_snap_sequential() {
        let d = tmp();
        let mut w = SessionWriter::open(d.path(), 0).unwrap();
        for i in 0..5u64 {
            w.write_snap(format!("{{\"i\":{i}}}").as_bytes()).unwrap();
        }
        assert!(d.path().join("snap-00004.json").exists());
        assert_eq!(w.snap_count(), 5);
    }

    #[test]
    fn probe_seq_resumes_after_existing() {
        let d = tmp();
        fs::write(d.path().join("snap-00003.json"), b"x").unwrap();
        fs::write(d.path().join("snap-00001.json"), b"x").unwrap();
        assert_eq!(probe_next_seq(d.path()), 4);
    }

    #[test]
    fn snap_content_matches() {
        let d = tmp();
        let mut w = SessionWriter::open(d.path(), 0).unwrap();
        w.write_snap(b"{\"hello\":\"world\"}").unwrap();
        let content = fs::read(d.path().join("snap-00000.json")).unwrap();
        assert_eq!(content, b"{\"hello\":\"world\"}");
    }

    #[test]
    fn quota_trims_oldest() {
        let d = tmp();
        // ~200-byte blobs, quota 500 → should trim after 3 written
        let blob = b"{\"x\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}";
        let mut w = SessionWriter::open(d.path(), 500).unwrap();
        for _ in 0..5 {
            w.write_snap(blob).unwrap();
        }
        // oldest snaps should be trimmed, marker should exist
        assert!(d.path().join("_quota-trimmed").exists());
        // at least some snaps removed
        let remaining = sorted_snaps(d.path()).unwrap().len();
        assert!(remaining < 5, "expected trimming, got {remaining} snaps");
    }

    #[test]
    fn overrun_sentinel_created() {
        let d = tmp();
        let mut w = SessionWriter::open(d.path(), 0).unwrap();
        w.write_overrun(42).unwrap();
        assert!(d.path().join("_overrun-00042.json").exists());
    }

    #[test]
    fn write_session_meta_idempotent() {
        let d = tmp();
        write_session_meta(d.path(), "test-session").unwrap();
        let tag1 = fs::read_to_string(d.path().join("intent_tag")).unwrap();
        write_session_meta(d.path(), "test-session").unwrap();
        let tag2 = fs::read_to_string(d.path().join("intent_tag")).unwrap();
        assert_eq!(tag1, tag2);
    }
}
