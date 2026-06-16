//! Userspace Rust bindings for the `/dev/memlog` kernel char device.
//!
//! `/dev/memlog` is a per-uid circular ring that captures
//! "about-to-be-compacted" LLM context state so it survives the writing
//! process's death. Userspace writes opaque CBOR blobs (≤ [`RECORD_MAX`]);
//! the kernel prefixes each with a [`RecordHeader`] (sequence number and
//! timestamp are kernel-issued) and stores `<header><blob>` tuples in the
//! ring. Readers consume those tuples back out.
//!
//! This crate is the Phase-2 `libmemlog` promised in the project README. It
//! is dependency-free (std only) and mirrors `include/uapi/linux/memlog.h`
//! exactly. It compiles and its unit tests pass without the device present;
//! anything that touches `/dev/memlog` returns an [`io::Result`] so callers
//! degrade gracefully when not booted into `linux-wintermute`.
//!
//! New in v0.2: [`persistence`] (atomic per-session snapshot writer) and
//! [`lock`] (single-instance file-lock guard) modules used by `memlog-witness`.

pub mod lock;
pub mod mode_contract;
pub mod persistence;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

/// Canonical device path created by the kernel module / udev rule.
pub const DEVICE_PATH: &str = "/dev/memlog";
/// Unreserved char-major the v0.1 driver registers under.
pub const MAJOR: u32 = 244;
/// Maximum CBOR blob size accepted per record (64 KiB).
pub const RECORD_MAX: usize = 64 * 1024;
/// On-disk schema version this binding understands.
pub const SCHEMA_VERSION: u32 = 1;
/// Record-header magic: ASCII `'MLOG'` (`0x4D4C4F47`), little-endian on the wire.
pub const RECORD_MAGIC: u32 = 0x4D4C_4F47;
/// Serialized size of [`RecordHeader`] on the wire, in bytes.
pub const HEADER_LEN: usize = 56;

/// Per-record header the kernel prepends to every stored blob.
///
/// Field order and widths mirror `struct memlog_record_header` in
/// `include/uapi/linux/memlog.h`. All multi-byte fields are little-endian on
/// the wire (the device runs on the writing host, so native == LE on x86_64).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordHeader {
    /// [`RECORD_MAGIC`]; rejects mis-framed reads.
    pub magic: u32,
    /// [`SCHEMA_VERSION`] at write time.
    pub schema_version: u32,
    /// CBOR blob length following this header.
    pub length: u32,
    pub reserved: u32,
    /// `ktime_get_real_ns` at write.
    pub ts_ns: u64,
    /// Kernel-issued monotonic sequence number.
    pub seq: u64,
    pub uid: u32,
    pub pid: u32,
    /// `agent_session_id` if available; all-zero otherwise.
    pub session_id: [u8; 16],
}

impl RecordHeader {
    /// Parse a header from the front of `buf`. Returns `None` if `buf` is too
    /// short or the magic does not match [`RECORD_MAGIC`].
    pub fn from_bytes(buf: &[u8]) -> Option<RecordHeader> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
        let magic = u32_at(0);
        if magic != RECORD_MAGIC {
            return None;
        }
        let mut session_id = [0u8; 16];
        session_id.copy_from_slice(&buf[40..56]);
        Some(RecordHeader {
            magic,
            schema_version: u32_at(4),
            length: u32_at(8),
            reserved: u32_at(12),
            ts_ns: u64_at(16),
            seq: u64_at(24),
            uid: u32_at(32),
            pid: u32_at(36),
            session_id,
        })
    }

    /// Serialize this header to its [`HEADER_LEN`]-byte little-endian wire form.
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4..8].copy_from_slice(&self.schema_version.to_le_bytes());
        b[8..12].copy_from_slice(&self.length.to_le_bytes());
        b[12..16].copy_from_slice(&self.reserved.to_le_bytes());
        b[16..24].copy_from_slice(&self.ts_ns.to_le_bytes());
        b[24..32].copy_from_slice(&self.seq.to_le_bytes());
        b[32..36].copy_from_slice(&self.uid.to_le_bytes());
        b[36..40].copy_from_slice(&self.pid.to_le_bytes());
        b[40..56].copy_from_slice(&self.session_id);
        b
    }
}

/// One record read back from the ring: its header plus the CBOR blob bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub header: RecordHeader,
    pub blob: Vec<u8>,
}

/// Returns `Err` if `len` exceeds [`RECORD_MAX`]. Pulled out as a free function
/// so the size contract is testable without a live device.
pub fn check_blob_len(len: usize) -> io::Result<()> {
    if len > RECORD_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("blob is {len} bytes; max is {RECORD_MAX}"),
        ));
    }
    Ok(())
}

/// An open handle to `/dev/memlog`.
pub struct Memlog {
    file: File,
}

impl Memlog {
    /// Open the default [`DEVICE_PATH`] for read+write.
    pub fn open() -> io::Result<Memlog> {
        Self::open_path(DEVICE_PATH)
    }

    /// Open an arbitrary path (the device, or a fixture file for testing).
    pub fn open_path<P: AsRef<Path>>(path: P) -> io::Result<Memlog> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Memlog { file })
    }

    /// Write one opaque CBOR blob. The kernel issues the header (sequence,
    /// timestamp, uid/pid); userspace supplies only the blob bytes.
    pub fn write_record(&mut self, blob: &[u8]) -> io::Result<()> {
        check_blob_len(blob.len())?;
        self.file.write_all(blob)
    }

    /// Read the ring contents and parse them into `<header><blob>` records.
    /// Stops at end-of-stream or the first mis-framed header.
    pub fn read_records(&mut self) -> io::Result<Vec<Record>> {
        let mut buf = Vec::new();
        self.file.read_to_end(&mut buf)?;
        Ok(parse_records(&buf))
    }
}

/// Parse a buffer of concatenated `<header><blob>` tuples into [`Record`]s.
/// Returns the records decoded before the first truncated or mis-framed entry.
pub fn parse_records(mut buf: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    while let Some(header) = RecordHeader::from_bytes(buf) {
        let blob_len = header.length as usize;
        let end = HEADER_LEN + blob_len;
        if buf.len() < end {
            break; // truncated tail
        }
        out.push(Record {
            header,
            blob: buf[HEADER_LEN..end].to_vec(),
        });
        buf = &buf[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_wire_size_matches_uapi() {
        // memlog_record_header is 56 packed bytes; our wire form must agree.
        assert_eq!(HEADER_LEN, 56);
        assert_eq!(std::mem::size_of::<RecordHeader>(), 56);
    }

    #[test]
    fn header_roundtrips() {
        let h = RecordHeader {
            magic: RECORD_MAGIC,
            schema_version: SCHEMA_VERSION,
            length: 42,
            reserved: 0,
            ts_ns: 1_700_000_000_123_456_789,
            seq: 9001,
            uid: 1000,
            pid: 506601,
            session_id: *b"abcdef0123456789",
        };
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(RecordHeader::from_bytes(&bytes), Some(h));
    }

    #[test]
    fn from_bytes_rejects_bad_magic_and_short_buf() {
        assert_eq!(RecordHeader::from_bytes(&[0u8; HEADER_LEN]), None);
        assert_eq!(RecordHeader::from_bytes(&[0u8; 10]), None);
    }

    #[test]
    fn parse_records_decodes_tuples_and_stops_on_garbage() {
        let mk = |seq: u64, blob: &[u8]| {
            let h = RecordHeader {
                magic: RECORD_MAGIC,
                schema_version: SCHEMA_VERSION,
                length: blob.len() as u32,
                reserved: 0,
                ts_ns: 0,
                seq,
                uid: 1000,
                pid: 1,
                session_id: [0u8; 16],
            };
            let mut v = h.to_bytes().to_vec();
            v.extend_from_slice(blob);
            v
        };
        let mut stream = mk(1, b"hello");
        stream.extend_from_slice(&mk(2, b"world"));
        stream.extend_from_slice(b"\xff\xff garbage"); // mis-framed tail

        let recs = parse_records(&stream);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].blob, b"hello");
        assert_eq!(recs[1].header.seq, 2);
        assert_eq!(recs[1].blob, b"world");
    }

    #[test]
    fn check_blob_len_enforces_max() {
        assert!(check_blob_len(0).is_ok());
        assert!(check_blob_len(RECORD_MAX).is_ok());
        assert!(check_blob_len(RECORD_MAX + 1).is_err());
    }
}
