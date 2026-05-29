//! Integration tests for memlog-witness persistence logic.
//!
//! Uses the pre-recorded binary fixture at `tests/fixture/memlog-replay.bin`
//! (three records: two for "session-a", one for "session-b").  Verifies that
//! `parse_records` + `SessionWriter` produce the expected directory layout.

use libmemlog::persistence::{write_session_meta, SessionWriter};
use libmemlog::parse_records;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Return the fixture bytes.
fn fixture_bytes() -> Vec<u8> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixture/memlog-replay.bin");
    fs::read(&p).expect("fixture file missing")
}

/// Derive a session-id string from a record's session_id bytes (same logic
/// as memlog-witness: hex if non-zero, else "comm:unknown:pid:<n>").
fn session_id_str(session_id: &[u8; 16], pid: u32) -> String {
    if session_id.iter().all(|&b| b == 0) {
        format!("comm:unknown:pid:{pid}")
    } else {
        // Use the non-null prefix as ASCII, trim nulls.
        let s = std::str::from_utf8(session_id)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();
        if s.is_empty() {
            hex::encode(session_id)
        } else {
            s
        }
    }
}

#[test]
fn fixture_parses_to_three_records() {
    let data = fixture_bytes();
    let records = parse_records(&data);
    assert_eq!(records.len(), 3, "expected 3 records in fixture");
}

#[test]
fn fixture_replay_writes_per_session_snaps() {
    let data = fixture_bytes();
    let records = parse_records(&data);

    let out_dir = tempfile::tempdir().expect("tempdir");
    let mut writers: HashMap<String, SessionWriter> = HashMap::new();

    for rec in &records {
        let sid = session_id_str(&rec.header.session_id, rec.header.pid);
        let session_dir = out_dir.path().join(&sid);
        let w = writers
            .entry(sid.clone())
            .or_insert_with(|| SessionWriter::open(&session_dir, 0).unwrap());
        // Serialize record as JSON envelope.
        let json = format!(
            "{{\"seq\":{},\"ts_ns\":{},\"uid\":{},\"pid\":{},\"blob_hex\":\"{}\"}}",
            rec.header.seq,
            rec.header.ts_ns,
            rec.header.uid,
            rec.header.pid,
            hex::encode(&rec.blob),
        );
        w.write_snap(json.as_bytes()).unwrap();
    }

    // session-a had 2 records
    let sa_dir = out_dir.path().join("session-a");
    assert!(sa_dir.exists(), "session-a dir missing");
    assert!(sa_dir.join("snap-00000.json").exists(), "snap-00000 missing");
    assert!(sa_dir.join("snap-00001.json").exists(), "snap-00001 missing");
    assert!(
        !sa_dir.join("snap-00002.json").exists(),
        "unexpected snap-00002"
    );

    // session-b had 1 record
    let sb_dir = out_dir.path().join("session-b");
    assert!(sb_dir.exists(), "session-b dir missing");
    assert!(sb_dir.join("snap-00000.json").exists(), "session-b snap missing");
}

#[test]
fn fixture_replay_snap_content_is_valid_json() {
    let data = fixture_bytes();
    let records = parse_records(&data);
    let out_dir = tempfile::tempdir().expect("tempdir");
    let mut writers: HashMap<String, SessionWriter> = HashMap::new();

    for rec in &records {
        let sid = session_id_str(&rec.header.session_id, rec.header.pid);
        let session_dir = out_dir.path().join(&sid);
        let w = writers
            .entry(sid.clone())
            .or_insert_with(|| SessionWriter::open(&session_dir, 0).unwrap());
        let json = format!("{{\"seq\":{}}}", rec.header.seq);
        w.write_snap(json.as_bytes()).unwrap();
    }

    // Check that snap files contain valid UTF-8 starting with '{'
    let sa_snap = fs::read(out_dir.path().join("session-a").join("snap-00000.json")).unwrap();
    assert!(sa_snap.starts_with(b"{"), "not JSON");
}

#[test]
fn session_meta_written_for_each_session() {
    let data = fixture_bytes();
    let records = parse_records(&data);
    let out_dir = tempfile::tempdir().expect("tempdir");
    let mut sessions_seen: Vec<String> = Vec::new();

    for rec in &records {
        let sid = session_id_str(&rec.header.session_id, rec.header.pid);
        let session_dir = out_dir.path().join(&sid);
        fs::create_dir_all(&session_dir).unwrap();
        write_session_meta(&session_dir, &sid).unwrap();
        if !sessions_seen.contains(&sid) {
            sessions_seen.push(sid.clone());
        }
    }

    for sid in &sessions_seen {
        let d = out_dir.path().join(sid);
        assert!(d.join("intent_tag").exists(), "intent_tag missing for {sid}");
        assert!(d.join("opened_at").exists(), "opened_at missing for {sid}");
    }
}
