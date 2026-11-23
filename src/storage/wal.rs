// src/storage/wal.rs
// WAL record format: data structures, byte encoding/decoding and CRC32.
// Appending to the WAL file, LSN assignment and reading the file are not here yet.
//
// Record layout (all integers are little-endian):
//   header (17 bytes): lsn u64 | type u8 | len u32 | checksum u32
//   body:  FullPageWrite = page_id u32 + page_data [u8; 4096]
//          CatalogWrite  = catalog JSON bytes (variable length)
//          Commit        = empty
// `len` is the size of the whole record. `checksum` is the CRC32 of the whole
// record except the checksum field itself.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

/// Header size: lsn(8) + type(1) + len(4) + checksum(4)
pub const WAL_HEADER_SIZE: usize = 17;

const PAGE_SIZE: usize = 4096;
/// Size of the page_id field (u32) at the start of a FullPageWrite body
const PAGE_ID_SIZE: usize = 4;
/// Total size of a FullPageWrite record: header + page_id + page_data
const FULL_PAGE_WRITE_SIZE: usize = WAL_HEADER_SIZE + PAGE_ID_SIZE + PAGE_SIZE;
/// Total size of a Commit record: header only
const COMMIT_SIZE: usize = WAL_HEADER_SIZE;
/// Byte range of the checksum field inside the header
const CHECKSUM_START: usize = 13;
const CHECKSUM_END: usize = 17;

const TYPE_FULL_PAGE_WRITE: u8 = 1;
const TYPE_CATALOG_WRITE: u8 = 2;
const TYPE_COMMIT: u8 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum WalBody {
    FullPageWrite { page_id: u32, page_data: [u8; PAGE_SIZE] },
    CatalogWrite { catalog_json: Vec<u8> },
    Commit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WalRecord {
    pub lsn: u64,
    pub body: WalBody,
}

/// Why a record could not be decoded. One variant per stop condition in design.md.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WalError {
    /// Fewer bytes left than the header size
    TruncatedHeader,
    /// `len` is smaller than the header, or larger than the bytes left
    InvalidLength,
    /// `type` is not 1, 2 or 3
    UnknownType,
    /// `len` does not fit the type (FullPageWrite must be 4117, Commit must be 17)
    LengthMismatchForType,
    /// The stored checksum does not match the record
    ChecksumMismatch,
}

impl WalRecord {
    /// Encode the record, including `len` and the checksum.
    pub fn to_bytes(&self) -> Vec<u8> {
        let (record_type, body) = match &self.body {
            WalBody::FullPageWrite { page_id, page_data } => {
                let mut body = Vec::with_capacity(PAGE_ID_SIZE + PAGE_SIZE);
                body.extend_from_slice(&page_id.to_le_bytes());
                body.extend_from_slice(page_data);
                (TYPE_FULL_PAGE_WRITE, body)
            }
            WalBody::CatalogWrite { catalog_json } => (TYPE_CATALOG_WRITE, catalog_json.clone()),
            WalBody::Commit => (TYPE_COMMIT, Vec::new()),
        };

        let len = WAL_HEADER_SIZE + body.len();
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(&self.lsn.to_le_bytes());
        bytes.push(record_type);
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]); // checksum placeholder
        bytes.extend_from_slice(&body);

        let checksum = Self::checksum_of(&bytes);
        bytes[CHECKSUM_START..CHECKSUM_END].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decode one record from the start of `bytes`. More bytes (the following
    /// records) may follow. Returns the record and the number of bytes it used.
    /// The checks run in the order given in design.md, because the checksum range
    /// is only known once `len` is trusted.
    pub fn from_bytes(bytes: &[u8]) -> Result<(WalRecord, usize), WalError> {
        if bytes.len() < WAL_HEADER_SIZE {
            return Err(WalError::TruncatedHeader);
        }
        let lsn = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let record_type = bytes[8];
        let len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let stored_checksum = u32::from_le_bytes(bytes[CHECKSUM_START..CHECKSUM_END].try_into().unwrap());

        if len < WAL_HEADER_SIZE || len > bytes.len() {
            return Err(WalError::InvalidLength);
        }
        if !matches!(record_type, TYPE_FULL_PAGE_WRITE | TYPE_CATALOG_WRITE | TYPE_COMMIT) {
            return Err(WalError::UnknownType);
        }
        if (record_type == TYPE_FULL_PAGE_WRITE && len != FULL_PAGE_WRITE_SIZE)
            || (record_type == TYPE_COMMIT && len != COMMIT_SIZE)
        {
            return Err(WalError::LengthMismatchForType);
        }

        let record = &bytes[..len];
        if Self::checksum_of(record) != stored_checksum {
            return Err(WalError::ChecksumMismatch);
        }

        let body_bytes = &record[WAL_HEADER_SIZE..];
        let body = match record_type {
            TYPE_FULL_PAGE_WRITE => {
                let page_id = u32::from_le_bytes(body_bytes[..PAGE_ID_SIZE].try_into().unwrap());
                let mut page_data = [0u8; PAGE_SIZE];
                page_data.copy_from_slice(&body_bytes[PAGE_ID_SIZE..]);
                WalBody::FullPageWrite { page_id, page_data }
            }
            TYPE_CATALOG_WRITE => WalBody::CatalogWrite { catalog_json: body_bytes.to_vec() },
            _ => WalBody::Commit,
        };
        Ok((WalRecord { lsn, body }, len))
    }

    /// CRC32 of a whole encoded record except its checksum field
    fn checksum_of(record: &[u8]) -> u32 {
        crc32(&[&record[..CHECKSUM_START], &record[CHECKSUM_END..]])
    }
}

/// Reads WAL records one by one from the start of a byte buffer (the whole WAL file).
/// Shared by `dump`, LSN resumption and Recovery.
///
/// - Yields `Ok(record)` for each valid record.
/// - At the first broken record it yields one `Err(WalError)` and then `None`.
///   Nothing after a broken record is read, because its `len` cannot be trusted.
/// - Reaching the end of the buffer exactly at a record boundary is a normal EOF (`None`).
pub struct WalReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> WalReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0, failed: false }
    }

    /// Byte offset just after the last record that was read successfully
    pub fn position(&self) -> usize {
        self.pos
    }
}

impl Iterator for WalReader<'_> {
    type Item = Result<WalRecord, WalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.bytes.len() {
            return None;
        }
        match WalRecord::from_bytes(&self.bytes[self.pos..]) {
            Ok((record, used)) => {
                self.pos += used;
                Some(Ok(record))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// The WAL file (`wal.log`): append-only, LSN assignment and fsync.
/// Writing `last_applied_lsn` into page images is the caller's job
/// (see `next_lsn`); this type does not know the page layout.
pub struct Wal {
    file: File,
    next_lsn: u64,
}

impl Wal {
    /// Open the WAL file, creating it if it does not exist.
    /// An existing file is read from the start; the LSN resumes at the last valid
    /// record's LSN + 1 (1 for a missing or empty file). Valid records after the last
    /// Commit are counted too (they are cut off by Recovery, not here).
    /// A corrupt WAL is an error and the file is left untouched.
    pub fn open(path: &Path) -> Result<Wal, String> {
        let existing = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("Failed to read WAL {}: {}", path.display(), e)),
        };

        let mut last_lsn = 0;
        let mut reader = WalReader::new(&existing);
        while let Some(item) = reader.next() {
            let record = item.map_err(|e| {
                format!("WAL {} is corrupted at byte {}: {:?}", path.display(), reader.position(), e)
            })?;
            last_lsn = record.lsn;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Failed to open WAL {}: {}", path.display(), e))?;

        Ok(Wal { file, next_lsn: last_lsn + 1 })
    }

    /// The LSN the next appended record will get
    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }

    /// Append one record with the next LSN and return that LSN.
    /// This does not fsync; call `sync` after the Commit record.
    /// On a write error the LSN is not consumed.
    pub fn append(&mut self, body: WalBody) -> Result<u64, String> {
        let lsn = self.next_lsn;
        let bytes = WalRecord { lsn, body }.to_bytes();
        self.file
            .write_all(&bytes)
            .map_err(|e| format!("Failed to append to WAL: {}", e))?;
        self.next_lsn += 1;
        Ok(lsn)
    }

    /// fsync the WAL file (data and metadata such as the file length)
    pub fn sync(&mut self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|e| format!("Failed to fsync WAL: {}", e))
    }
}

/// Print the records of a WAL file, one line each (used by `dump-wal`).
///
/// Line format: `offset=<byte> lsn=<n> type=<name> len=<n> checksum=0x<hex> OK <details>`
/// `OK` means the stored checksum matches the record. The first broken record is
/// printed as `NG <reason>` and the dump stops there (nothing after it can be trusted).
/// Returns `Err` only when the file cannot be read or the output cannot be written;
/// a corrupt WAL is reported in the output, not as an error. A missing file is an
/// error and is not created (unlike `Wal::open`).
pub fn dump(path: &Path, out: &mut impl Write) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Failed to read WAL {}: {}", path.display(), e))?;
    let io_err = |e: std::io::Error| format!("Failed to write dump output: {}", e);

    let mut reader = WalReader::new(&bytes);
    let mut count = 0;
    loop {
        let offset = reader.position();
        match reader.next() {
            None => {
                writeln!(out, "End of WAL: {} records, {} bytes", count, bytes.len()).map_err(io_err)?;
                return Ok(());
            }
            Some(Ok(record)) => {
                let stored_checksum = u32::from_le_bytes(
                    bytes[offset + CHECKSUM_START..offset + CHECKSUM_END].try_into().unwrap(),
                );
                let (type_name, details) = match &record.body {
                    WalBody::FullPageWrite { page_id, .. } => ("FullPageWrite", format!("page_id={}", page_id)),
                    WalBody::CatalogWrite { catalog_json } => {
                        ("CatalogWrite", format!("catalog_bytes={}", catalog_json.len()))
                    }
                    WalBody::Commit => ("Commit", String::new()),
                };
                writeln!(
                    out,
                    "offset={} lsn={} type={} len={} checksum=0x{:08X} OK {}",
                    offset,
                    record.lsn,
                    type_name,
                    reader.position() - offset,
                    stored_checksum,
                    details
                )
                .map_err(io_err)?;
                count += 1;
            }
            Some(Err(error)) => {
                writeln!(
                    out,
                    "offset={} NG {:?} ({} bytes left, not read)",
                    offset,
                    error,
                    bytes.len() - offset
                )
                .map_err(io_err)?;
                writeln!(out, "Stopped at byte {}: {} records read", offset, count).map_err(io_err)?;
                return Ok(());
            }
        }
    }
}

/// CRC32 lookup table for the reflected polynomial 0xEDB88320 (IEEE 802.3)
const CRC32_TABLE: [u32; 256] = make_crc32_table();

const fn make_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// CRC32 over the concatenation of `parts` (without copying them together)
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc = CRC32_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_with(byte: u8) -> [u8; PAGE_SIZE] {
        let mut page = [0u8; PAGE_SIZE];
        page[PAGE_SIZE - 1] = byte;
        page
    }

    fn commit(lsn: u64) -> WalRecord {
        WalRecord { lsn, body: WalBody::Commit }
    }

    fn full_page_write(lsn: u64) -> WalRecord {
        WalRecord { lsn, body: WalBody::FullPageWrite { page_id: 7, page_data: page_with(0xAB) } }
    }

    fn catalog_write(lsn: u64) -> WalRecord {
        WalRecord { lsn, body: WalBody::CatalogWrite { catalog_json: br#"{"tables":{}}"#.to_vec() } }
    }

    /// A header-only byte string with the given fields, zero-padded up to `total` bytes
    fn crafted(lsn: u64, record_type: u8, len: u32, total: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&lsn.to_le_bytes());
        bytes.push(record_type);
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.resize(total, 0);
        bytes
    }

    #[test]
    fn crc32_matches_the_standard_test_vector() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        // splitting the input into parts does not change the result
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn record_sizes_match_the_spec() {
        assert_eq!(commit(1).to_bytes().len(), 17);
        assert_eq!(full_page_write(1).to_bytes().len(), 4117);
        assert_eq!(catalog_write(1).to_bytes().len(), WAL_HEADER_SIZE + br#"{"tables":{}}"#.len());
    }

    #[test]
    fn every_record_type_roundtrips() {
        for record in [commit(3), full_page_write(4), catalog_write(5)] {
            let bytes = record.to_bytes();
            let (decoded, used) = WalRecord::from_bytes(&bytes).unwrap();
            assert_eq!(decoded, record);
            assert_eq!(used, bytes.len());
        }
    }

    #[test]
    fn header_fields_are_laid_out_as_specified() {
        let bytes = full_page_write(0x0102_0304_0506_0708).to_bytes();
        assert_eq!(&bytes[0..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(bytes[8], 1);
        assert_eq!(&bytes[9..13], &4117u32.to_le_bytes());
        assert_eq!(&bytes[17..21], &7u32.to_le_bytes()); // page_id
        assert_eq!(catalog_write(1).to_bytes()[8], 2);
        assert_eq!(commit(1).to_bytes()[8], 3);
    }

    #[test]
    fn truncated_header_is_detected() {
        let bytes = commit(1).to_bytes();
        assert_eq!(WalRecord::from_bytes(&bytes[..WAL_HEADER_SIZE - 1]), Err(WalError::TruncatedHeader));
        assert_eq!(WalRecord::from_bytes(&[]), Err(WalError::TruncatedHeader));
    }

    #[test]
    fn invalid_length_is_detected() {
        // record cut off in the middle: len is larger than the bytes left
        let bytes = full_page_write(1).to_bytes();
        assert_eq!(WalRecord::from_bytes(&bytes[..bytes.len() - 1]), Err(WalError::InvalidLength));
        // len smaller than the header
        assert_eq!(WalRecord::from_bytes(&crafted(1, 3, 5, 17)), Err(WalError::InvalidLength));
    }

    #[test]
    fn unknown_type_is_detected() {
        assert_eq!(WalRecord::from_bytes(&crafted(1, 0, 17, 17)), Err(WalError::UnknownType));
        assert_eq!(WalRecord::from_bytes(&crafted(1, 9, 17, 17)), Err(WalError::UnknownType));
    }

    #[test]
    fn length_that_does_not_fit_the_type_is_detected() {
        // FullPageWrite must be 4117 bytes, Commit must be 17 bytes
        assert_eq!(WalRecord::from_bytes(&crafted(1, 1, 100, 100)), Err(WalError::LengthMismatchForType));
        assert_eq!(WalRecord::from_bytes(&crafted(1, 3, 20, 20)), Err(WalError::LengthMismatchForType));
    }

    #[test]
    fn checksum_mismatch_is_detected() {
        // flip one bit in the page data
        let mut bytes = full_page_write(1).to_bytes();
        bytes[100] ^= 0x01;
        assert_eq!(WalRecord::from_bytes(&bytes), Err(WalError::ChecksumMismatch));
        // flip one bit in the lsn of a Commit
        let mut bytes = commit(1).to_bytes();
        bytes[0] ^= 0x01;
        assert_eq!(WalRecord::from_bytes(&bytes), Err(WalError::ChecksumMismatch));
        // a wrong stored checksum
        let mut bytes = catalog_write(1).to_bytes();
        bytes[13] ^= 0xFF;
        assert_eq!(WalRecord::from_bytes(&bytes), Err(WalError::ChecksumMismatch));
    }

    /// A fresh path for a WAL file in a unique temp directory
    fn wal_path(test_name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("mydb_wal_{}_{}", test_name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("wal.log")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn page_body(page_id: u32) -> WalBody {
        WalBody::FullPageWrite { page_id, page_data: page_with(page_id as u8) }
    }

    #[test]
    fn open_creates_the_file_and_starts_at_lsn_1() {
        let path = wal_path("open_new");
        assert!(!path.exists());
        let wal = Wal::open(&path).unwrap();
        assert!(path.exists());
        assert_eq!(wal.next_lsn(), 1);
        assert_eq!(std::fs::read(&path).unwrap().len(), 0);
        cleanup(&path);
    }

    #[test]
    fn append_assigns_consecutive_lsns_and_writes_readable_records() {
        let path = wal_path("append");
        let mut wal = Wal::open(&path).unwrap();
        assert_eq!(wal.append(page_body(1)).unwrap(), 1);
        assert_eq!(wal.append(WalBody::CatalogWrite { catalog_json: b"{}".to_vec() }).unwrap(), 2);
        assert_eq!(wal.append(WalBody::Commit).unwrap(), 3);
        assert_eq!(wal.next_lsn(), 4);
        wal.sync().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let records: Vec<_> = WalReader::new(&bytes).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            records,
            vec![
                WalRecord { lsn: 1, body: page_body(1) },
                WalRecord { lsn: 2, body: WalBody::CatalogWrite { catalog_json: b"{}".to_vec() } },
                WalRecord { lsn: 3, body: WalBody::Commit },
            ]
        );
        cleanup(&path);
    }

    #[test]
    fn reopening_resumes_the_lsn_and_appends_after_the_existing_records() {
        let path = wal_path("reopen");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(page_body(1)).unwrap();
            wal.append(WalBody::Commit).unwrap();
        }
        let mut wal = Wal::open(&path).unwrap();
        assert_eq!(wal.next_lsn(), 3);
        assert_eq!(wal.append(WalBody::Commit).unwrap(), 3);

        let bytes = std::fs::read(&path).unwrap();
        let lsns: Vec<u64> = WalReader::new(&bytes).map(|r| r.unwrap().lsn).collect();
        assert_eq!(lsns, vec![1, 2, 3]);
        cleanup(&path);
    }

    #[test]
    fn records_after_the_last_commit_are_counted_when_resuming() {
        let path = wal_path("uncommitted_tail");
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append(page_body(1)).unwrap();
            wal.append(WalBody::Commit).unwrap();
            wal.append(page_body(2)).unwrap(); // no Commit after this one
        }
        let wal = Wal::open(&path).unwrap();
        assert_eq!(wal.next_lsn(), 4);
        cleanup(&path);
    }

    #[test]
    fn opening_a_corrupt_wal_fails_and_leaves_the_file_unchanged() {
        let path = wal_path("corrupt");
        let mut bytes = concat(&[&commit(1), &full_page_write(2)]);
        let flip_at = commit(1).to_bytes().len() + 100; // inside the second record
        bytes[flip_at] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        let result = Wal::open(&path);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("corrupted"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        cleanup(&path);
    }

    #[test]
    fn opening_a_corrupt_wal_does_not_create_anything_new() {
        // a partial tail: the file must stay exactly as it was
        let path = wal_path("partial_tail");
        let mut bytes = commit(1).to_bytes();
        bytes.extend_from_slice(&commit(2).to_bytes()[..5]);
        std::fs::write(&path, &bytes).unwrap();

        assert!(Wal::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        cleanup(&path);
    }

    fn concat(records: &[&WalRecord]) -> Vec<u8> {
        records.iter().flat_map(|r| r.to_bytes()).collect()
    }

    #[test]
    fn reader_returns_nothing_for_empty_bytes() {
        let mut reader = WalReader::new(&[]);
        assert!(reader.next().is_none());
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn reader_returns_all_records_in_order_and_ends_normally() {
        let records = [catalog_write(1), full_page_write(2), commit(3)];
        let bytes = concat(&[&records[0], &records[1], &records[2]]);

        let mut reader = WalReader::new(&bytes);
        let items: Vec<_> = reader.by_ref().collect();
        let expected: Vec<_> = records.iter().cloned().map(Ok).collect();
        assert_eq!(items, expected);
        assert_eq!(reader.position(), bytes.len());
    }

    #[test]
    fn reader_reports_a_partial_tail_once_and_then_stops() {
        let (a, b) = (catalog_write(1), commit(2));
        let good = concat(&[&a, &b]);

        // tail cut inside the header
        let mut bytes = good.clone();
        bytes.extend_from_slice(&commit(3).to_bytes()[..10]);
        let mut reader = WalReader::new(&bytes);
        let items: Vec<_> = reader.by_ref().collect();
        assert_eq!(items, vec![Ok(a.clone()), Ok(b.clone()), Err(WalError::TruncatedHeader)]);
        assert_eq!(reader.position(), good.len());
        assert!(reader.next().is_none());

        // tail cut inside the body
        let mut bytes = good.clone();
        let partial = full_page_write(3).to_bytes();
        bytes.extend_from_slice(&partial[..partial.len() - 1]);
        let mut reader = WalReader::new(&bytes);
        let items: Vec<_> = reader.by_ref().collect();
        assert_eq!(items, vec![Ok(a), Ok(b), Err(WalError::InvalidLength)]);
        assert_eq!(reader.position(), good.len());
    }

    #[test]
    fn reader_stops_at_a_corrupt_record_in_the_middle() {
        let (a, b, c) = (commit(1), full_page_write(2), commit(3));
        let mut bytes = concat(&[&a, &b, &c]);
        bytes[a.to_bytes().len() + 100] ^= 0x01; // inside b's page data

        let mut reader = WalReader::new(&bytes);
        let items: Vec<_> = reader.by_ref().collect();
        // c is valid but is not returned: nothing after a broken record is read
        assert_eq!(items, vec![Ok(a.clone()), Err(WalError::ChecksumMismatch)]);
        assert_eq!(reader.position(), a.to_bytes().len());
    }

    fn dump_text(path: &Path) -> String {
        let mut out = Vec::new();
        dump(path, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn dump_shows_lsn_type_and_checksum_with_ok_for_every_record() {
        let path = wal_path("dump_ok");
        let records = [catalog_write(1), full_page_write(2), commit(3)];
        std::fs::write(&path, concat(&[&records[0], &records[1], &records[2]])).unwrap();

        let text = dump_text(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("lsn=1 type=CatalogWrite"));
        assert!(lines[1].contains("lsn=2 type=FullPageWrite len=4117"));
        assert!(lines[1].contains("page_id=7"));
        assert!(lines[2].contains("lsn=3 type=Commit len=17"));
        for line in &lines[..3] {
            assert!(line.contains(" OK"), "{}", line);
            assert!(!line.contains("NG"), "{}", line);
        }
        // the printed checksum is the one stored in the record
        let stored = u32::from_le_bytes(records[2].to_bytes()[13..17].try_into().unwrap());
        assert!(lines[2].contains(&format!("checksum=0x{:08X}", stored)));
        assert!(lines[3].contains("3 records"));
        cleanup(&path);
    }

    #[test]
    fn dump_marks_a_checksum_mismatch_as_ng_and_stops() {
        let path = wal_path("dump_ng");
        let mut bytes = concat(&[&commit(1), &full_page_write(2), &commit(3)]);
        bytes[commit(1).to_bytes().len() + 100] ^= 0x01; // inside the second record
        std::fs::write(&path, &bytes).unwrap();

        let text = dump_text(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("lsn=1") && lines[0].contains(" OK"));
        assert!(lines[1].contains("NG ChecksumMismatch"));
        assert!(lines[2].contains("Stopped at byte 17: 1 records read"));
        assert_eq!(lines.len(), 3); // the third record is not shown
        cleanup(&path);
    }

    #[test]
    fn dump_reports_a_partial_tail_as_ng() {
        let path = wal_path("dump_partial");
        let mut bytes = commit(1).to_bytes();
        bytes.extend_from_slice(&commit(2).to_bytes()[..5]);
        std::fs::write(&path, &bytes).unwrap();

        let text = dump_text(&path);
        assert!(text.contains("NG TruncatedHeader (5 bytes left"));
        cleanup(&path);
    }

    #[test]
    fn dump_of_an_empty_wal_reports_zero_records() {
        let path = wal_path("dump_empty");
        std::fs::write(&path, []).unwrap();
        assert_eq!(dump_text(&path), "End of WAL: 0 records, 0 bytes\n");
        cleanup(&path);
    }

    #[test]
    fn dump_of_a_missing_file_fails_without_creating_it() {
        let path = wal_path("dump_missing");
        let mut out = Vec::new();
        assert!(dump(&path, &mut out).is_err());
        assert!(!path.exists());
        cleanup(&path);
    }

    #[test]
    fn decoding_returns_only_the_first_of_several_records() {
        let first = full_page_write(1);
        let second = commit(2);
        let mut bytes = first.to_bytes();
        let first_len = bytes.len();
        bytes.extend_from_slice(&second.to_bytes());

        let (decoded, used) = WalRecord::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, first);
        assert_eq!(used, first_len);
        let (decoded, used) = WalRecord::from_bytes(&bytes[first_len..]).unwrap();
        assert_eq!(decoded, second);
        assert_eq!(used, 17);
    }
}
