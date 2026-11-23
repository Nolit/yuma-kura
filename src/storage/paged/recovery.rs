// src/storage/paged/recovery.rs
// Recovery from the WAL at startup (design.md: "Recovery").
// It runs before catalog.json is loaded and pages.dat is opened, so no `PagedStorage`
// exists yet: everything here takes paths and bytes only.

use super::PAGE_SIZE;
use crate::storage::wal::{WalBody, WalReader, WalRecord};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::Path;

/// Result of verifying the whole WAL at the start of Recovery
struct WalInspection {
    /// Committed transactions in WAL order. Each one ends with its Commit record.
    committed: Vec<Vec<WalRecord>>,
    /// Byte offset just after the last Commit record (0 when there is none).
    /// If it is smaller than the WAL length, valid records without a Commit follow it.
    last_commit_end: usize,
}

/// Recovery from the WAL. It runs at startup before `catalog.json` is loaded and
/// `pages.dat` is opened, so it takes no `self` (the storage does not exist yet).
///
/// Called by `new` right after the directory is created. It verifies the whole WAL
/// (failing if a record is broken), writes the committed pages back to `pages.dat`
/// and the last committed catalog to `catalog.json`, and cuts the WAL off after the
/// last Commit record.
///
/// A missing or empty WAL means there is nothing to apply: `catalog.json`, `pages.dat`
/// and `wal.log` are not touched or created. Any other read error is returned.
pub(super) fn recover(base_path: &Path) -> Result<(), String> {
    let wal_path = base_path.join("wal.log");
    // Read with fs::read, not Wal::open: Wal::open would create a missing wal.log
    let wal_bytes = match std::fs::read(&wal_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Failed to read WAL {}: {}", wal_path.display(), e)),
    };
    if wal_bytes.is_empty() {
        return Ok(());
    }

    // Verify the whole WAL first. A broken record fails Recovery here, before
    // anything is written back or truncated.
    let inspection = inspect_wal(&wal_path, &wal_bytes)?;

    // Order: pages.dat (fsync) -> catalog.json (fsync) -> truncate the WAL.
    // The WAL is only changed last, so a failure before that leaves it intact
    // and Recovery can run again.
    restore_pages(base_path, &inspection)?;
    restore_catalog(base_path, &inspection)?;
    truncate_wal(&wal_path, wal_bytes.len(), &inspection)?;
    Ok(())
}

/// Cut the WAL off after the last Commit record, dropping the valid records of a
/// transaction that never committed. They were not applied above.
///
/// They must not stay: records appended after the next start would follow them, and
/// the next Recovery would treat "previous Commit .. next Commit" as one transaction
/// and apply the uncommitted records too.
///
/// Nothing is done when the last Commit record already ends the file. With no Commit
/// record at all, the WAL is cut to length 0. A broken WAL never gets here: it fails
/// in `inspect_wal`. The LSN is not handled here: `Wal::open` resumes at the last
/// record's LSN + 1, so opening the truncated WAL does the right thing.
fn truncate_wal(wal_path: &Path, wal_len: usize, inspection: &WalInspection) -> Result<(), String> {
    if inspection.last_commit_end == wal_len {
        return Ok(());
    }

    let file = OpenOptions::new()
        .write(true)
        .open(wal_path)
        .map_err(|e| format!("Failed to open {}: {}", wal_path.display(), e))?;
    file.set_len(inspection.last_commit_end as u64)
        .map_err(|e| format!("Failed to truncate {}: {}", wal_path.display(), e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to fsync {}: {}", wal_path.display(), e))?;
    Ok(())
}

/// Write the last committed `CatalogWrite` back to `catalog.json`.
///
/// Only that one image is written, not one per transaction. It is written as it is
/// in the WAL (the same bytes `save_catalog` writes at commit); the JSON is not
/// parsed here, since Recovery does not read `catalog.json`. A broken `catalog.json`
/// is overwritten without being looked at.
///
/// With no committed `CatalogWrite`, `catalog.json` is not touched or created.
/// If this fails halfway the WAL is untouched, so Recovery can simply run again.
fn restore_catalog(base_path: &Path, inspection: &WalInspection) -> Result<(), String> {
    let last_catalog = inspection
        .committed
        .iter()
        .flatten()
        .filter_map(|record| match &record.body {
            WalBody::CatalogWrite { catalog_json } => Some(catalog_json),
            _ => None,
        })
        .last();
    let Some(catalog_json) = last_catalog else {
        return Ok(());
    };

    let catalog_path = base_path.join("catalog.json");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&catalog_path)
        .map_err(|e| format!("Failed to open {}: {}", catalog_path.display(), e))?;
    file.write_all(catalog_json)
        .map_err(|e| format!("Failed to restore catalog.json: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to fsync catalog.json: {}", e))?;
    Ok(())
}

/// Write the `FullPageWrite` images of the committed transactions back to `pages.dat`.
///
/// The images are written in WAL order (= LSN order) and unconditionally: the page's
/// own `last_applied_lsn` is not compared, because a torn page has an untrustworthy
/// header. The bytes are written as they are in the WAL, so `last_applied_lsn` of the
/// restored page equals the LSN of its `FullPageWrite`. A `page_id` beyond the end of
/// the file extends it (the gap is zero-filled, and later records of the same
/// transaction fill it). Replaying the same WAL again gives the same file.
///
/// With no committed `FullPageWrite`, `pages.dat` is neither opened nor created.
/// If this fails halfway the WAL is untouched, so Recovery can simply run again.
fn restore_pages(base_path: &Path, inspection: &WalInspection) -> Result<(), String> {
    let mut images = inspection
        .committed
        .iter()
        .flatten()
        .filter_map(|record| match &record.body {
            WalBody::FullPageWrite { page_id, page_data } => Some((*page_id, page_data)),
            _ => None,
        })
        .peekable();
    if images.peek().is_none() {
        return Ok(());
    }

    let page_file_path = base_path.join("pages.dat");
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&page_file_path)
        .map_err(|e| format!("Failed to open {}: {}", page_file_path.display(), e))?;

    for (page_id, page_data) in images {
        let offset = page_id as u64 * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to page {} in pages.dat: {}", page_id, e))?;
        file.write_all(page_data)
            .map_err(|e| format!("Failed to restore page {} to pages.dat: {}", page_id, e))?;
    }

    // sync_all, not sync_data: the file may have been extended
    file.sync_all()
        .map_err(|e| format!("Failed to fsync pages.dat: {}", e))?;
    Ok(())
}

/// Read the whole WAL and split it into committed transactions. Writes nothing.
///
/// A broken record (see the stop conditions in design.md) is an error, whether it is
/// in the middle or at the end of the WAL. Nothing after it is read.
/// Transactions are split at Commit records (no transaction id is used). Valid
/// records after the last Commit belong to no transaction: they are not returned,
/// and `last_commit_end` tells where they start.
fn inspect_wal(wal_path: &Path, wal_bytes: &[u8]) -> Result<WalInspection, String> {
    let mut committed = Vec::new();
    let mut current = Vec::new();
    let mut last_commit_end = 0;

    let mut reader = WalReader::new(wal_bytes);
    while let Some(item) = reader.next() {
        let record = item.map_err(|e| {
            format!("WAL {} is corrupted at byte {}: {:?}", wal_path.display(), reader.position(), e)
        })?;
        let is_commit = matches!(record.body, WalBody::Commit);
        current.push(record);
        if is_commit {
            committed.push(std::mem::take(&mut current));
            last_commit_end = reader.position();
        }
    }
    // `current` is now the records after the last Commit (no Commit reached): dropped.

    Ok(WalInspection { committed, last_commit_end })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::wal::Wal;
    use std::path::PathBuf;

    // ---- Recovery: the frame (missing / empty WAL, read errors) ----

    /// A fresh empty directory for a Recovery test (no storage is opened in it)
    fn recovery_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("mydb_recover_{}_{}", test_name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn recover_without_a_wal_succeeds_and_creates_nothing() {
        let dir = recovery_dir("no_wal");
        recover(&dir).unwrap();
        assert!(file_names(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_with_an_empty_wal_succeeds_and_creates_nothing() {
        let dir = recovery_dir("empty_wal");
        std::fs::write(dir.join("wal.log"), []).unwrap();
        recover(&dir).unwrap();
        // no pages.dat / catalog.json, and the WAL is still empty
        assert_eq!(file_names(&dir), vec!["wal.log".to_string()]);
        assert_eq!(std::fs::metadata(dir.join("wal.log")).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_with_an_empty_wal_leaves_existing_files_unchanged() {
        let dir = recovery_dir("empty_wal_existing");
        let catalog = b"not even valid json".to_vec();
        let pages = vec![0xABu8; PAGE_SIZE * 2];
        std::fs::write(dir.join("wal.log"), []).unwrap();
        std::fs::write(dir.join("catalog.json"), &catalog).unwrap();
        std::fs::write(dir.join("pages.dat"), &pages).unwrap();

        recover(&dir).unwrap();

        // Recovery does not read catalog.json (a broken one is not its concern here)
        assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), catalog);
        assert_eq!(std::fs::read(dir.join("pages.dat")).unwrap(), pages);
        assert_eq!(std::fs::metadata(dir.join("wal.log")).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_fails_when_the_wal_cannot_be_read() {
        let dir = recovery_dir("unreadable_wal");
        // a directory named wal.log: reading it fails with an error other than NotFound
        std::fs::create_dir(dir.join("wal.log")).unwrap();
        let error = recover(&dir).unwrap_err();
        assert!(error.contains("Failed to read WAL"), "unexpected error: {}", error);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Recovery: verifying the whole WAL ----

    fn page_record(lsn: u64, page_id: u32) -> WalRecord {
        WalRecord {
            lsn,
            body: WalBody::FullPageWrite { page_id, page_data: [page_id as u8; PAGE_SIZE] },
        }
    }

    fn catalog_record(lsn: u64) -> WalRecord {
        WalRecord { lsn, body: WalBody::CatalogWrite { catalog_json: b"{}".to_vec() } }
    }

    fn commit_record(lsn: u64) -> WalRecord {
        WalRecord { lsn, body: WalBody::Commit }
    }

    fn encode(records: &[&WalRecord]) -> Vec<u8> {
        records.iter().flat_map(|r| r.to_bytes()).collect()
    }

    fn inspect(wal_bytes: &[u8]) -> Result<WalInspection, String> {
        inspect_wal(Path::new("wal.log"), wal_bytes)
    }

    /// Run Recovery on a directory holding this WAL plus a catalog.json and pages.dat,
    /// and check that it fails without changing any of the three files.
    fn assert_recover_fails_and_changes_nothing(test_name: &str, wal_bytes: &[u8]) -> String {
        let dir = recovery_dir(test_name);
        let catalog = b"{\"tables\":{},\"next_page_id\":0}".to_vec();
        let pages = vec![0x5Au8; PAGE_SIZE];
        std::fs::write(dir.join("wal.log"), wal_bytes).unwrap();
        std::fs::write(dir.join("catalog.json"), &catalog).unwrap();
        std::fs::write(dir.join("pages.dat"), &pages).unwrap();

        let error = recover(&dir).unwrap_err();

        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), wal_bytes);
        assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), catalog);
        assert_eq!(std::fs::read(dir.join("pages.dat")).unwrap(), pages);
        assert_eq!(file_names(&dir), vec!["catalog.json", "pages.dat", "wal.log"]);
        let _ = std::fs::remove_dir_all(dir);
        error
    }

    #[test]
    fn inspect_splits_committed_transactions_at_commit_records() {
        let records = [
            page_record(1, 0), catalog_record(2), commit_record(3),
            page_record(4, 1), commit_record(5),
        ];
        let bytes = encode(&records.iter().collect::<Vec<_>>());

        let inspection = inspect(&bytes).unwrap();

        assert_eq!(inspection.committed.len(), 2);
        assert_eq!(inspection.committed[0], records[0..3].to_vec());
        assert_eq!(inspection.committed[1], records[3..5].to_vec());
        assert_eq!(inspection.last_commit_end, bytes.len());
    }

    #[test]
    fn inspect_leaves_valid_records_after_the_last_commit_out_of_the_transactions() {
        let committed = [page_record(1, 0), commit_record(2)];
        let tail = [page_record(3, 1), catalog_record(4)];
        let committed_bytes = encode(&committed.iter().collect::<Vec<_>>());
        let mut bytes = committed_bytes.clone();
        bytes.extend(encode(&tail.iter().collect::<Vec<_>>()));

        let inspection = inspect(&bytes).unwrap();

        assert_eq!(inspection.committed, vec![committed.to_vec()]);
        assert_eq!(inspection.last_commit_end, committed_bytes.len());
        assert!(inspection.last_commit_end < bytes.len());
    }

    #[test]
    fn inspect_without_any_commit_returns_no_transactions() {
        let bytes = encode(&[&page_record(1, 0), &catalog_record(2)]);

        let inspection = inspect(&bytes).unwrap();

        assert!(inspection.committed.is_empty());
        assert_eq!(inspection.last_commit_end, 0);
    }

    #[test]
    fn recover_fails_on_a_record_cut_off_in_the_middle_and_changes_nothing() {
        let mut bytes = encode(&[&page_record(1, 0), &commit_record(2)]);
        let valid_len = bytes.len();
        bytes.extend_from_slice(&page_record(3, 1).to_bytes()[..100]);

        let error = assert_recover_fails_and_changes_nothing("cut_record", &bytes);
        assert!(error.contains(&format!("corrupted at byte {}", valid_len)), "unexpected error: {}", error);
    }

    #[test]
    fn recover_fails_on_a_header_cut_off_at_the_end_and_changes_nothing() {
        let mut bytes = encode(&[&page_record(1, 0), &commit_record(2)]);
        let valid_len = bytes.len();
        bytes.extend_from_slice(&commit_record(3).to_bytes()[..5]);

        let error = assert_recover_fails_and_changes_nothing("cut_header", &bytes);
        assert!(error.contains(&format!("corrupted at byte {}", valid_len)), "unexpected error: {}", error);
    }

    #[test]
    fn recover_fails_on_a_checksum_mismatch_in_the_middle_and_changes_nothing() {
        let first = encode(&[&page_record(1, 0), &commit_record(2)]);
        let mut broken = page_record(3, 1).to_bytes();
        broken[200] ^= 0xFF; // flip a byte in the page data: the checksum no longer matches
        let mut bytes = first.clone();
        bytes.extend(broken);
        bytes.extend(commit_record(4).to_bytes()); // a valid record after the broken one

        let error = assert_recover_fails_and_changes_nothing("bad_checksum", &bytes);
        assert!(error.contains(&format!("corrupted at byte {}", first.len())), "unexpected error: {}", error);
    }

    // ---- Recovery: writing the committed pages back to pages.dat ----

    /// A FullPageWrite whose page is filled with `fill`
    fn filled_page_record(lsn: u64, page_id: u32, fill: u8) -> WalRecord {
        WalRecord {
            lsn,
            body: WalBody::FullPageWrite { page_id, page_data: [fill; PAGE_SIZE] },
        }
    }

    /// Put `records` into wal.log (and `existing_pages` into pages.dat, if given) of a
    /// fresh directory, run Recovery and return the directory.
    fn recover_records(
        test_name: &str,
        records: &[WalRecord],
        existing_pages: Option<Vec<u8>>,
    ) -> PathBuf {
        let dir = recovery_dir(test_name);
        std::fs::write(dir.join("wal.log"), encode(&records.iter().collect::<Vec<_>>())).unwrap();
        if let Some(pages) = existing_pages {
            std::fs::write(dir.join("pages.dat"), pages).unwrap();
        }
        recover(&dir).unwrap();
        dir
    }

    fn page_in(file: &[u8], page_id: usize) -> &[u8] {
        &file[page_id * PAGE_SIZE..(page_id + 1) * PAGE_SIZE]
    }

    #[test]
    fn recover_extends_a_short_pages_dat() {
        let records = [
            filled_page_record(1, 0, 0xA0),
            filled_page_record(2, 3, 0xA3), // page 3 is beyond the end of the file
            commit_record(3),
        ];
        let dir = recover_records("restore_extend", &records, Some(vec![0x11; PAGE_SIZE]));

        let file = std::fs::read(dir.join("pages.dat")).unwrap();
        assert_eq!(file.len(), 4 * PAGE_SIZE);
        assert!(page_in(&file, 0).iter().all(|&b| b == 0xA0));
        assert!(page_in(&file, 1).iter().all(|&b| b == 0));
        assert!(page_in(&file, 2).iter().all(|&b| b == 0));
        assert!(page_in(&file, 3).iter().all(|&b| b == 0xA3));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_keeps_the_last_image_of_a_page_and_handles_descending_page_ids() {
        // page 2 appears in two transactions: the later image wins.
        // In the third transaction the larger page_id comes first (file extended, then a gap is filled).
        let records = [
            filled_page_record(1, 2, 0x01), commit_record(2),
            filled_page_record(3, 2, 0x02), commit_record(4),
            filled_page_record(5, 5, 0x55), filled_page_record(6, 4, 0x44), commit_record(7),
        ];
        let dir = recover_records("restore_order", &records, None);

        let file = std::fs::read(dir.join("pages.dat")).unwrap();
        assert_eq!(file.len(), 6 * PAGE_SIZE);
        assert!(page_in(&file, 0).iter().all(|&b| b == 0));
        assert!(page_in(&file, 1).iter().all(|&b| b == 0));
        assert!(page_in(&file, 2).iter().all(|&b| b == 0x02));
        assert!(page_in(&file, 3).iter().all(|&b| b == 0));
        assert!(page_in(&file, 4).iter().all(|&b| b == 0x44));
        assert!(page_in(&file, 5).iter().all(|&b| b == 0x55));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_does_not_apply_pages_of_an_uncommitted_transaction() {
        let records = [
            filled_page_record(1, 0, 0xAA), commit_record(2),
            filled_page_record(3, 0, 0xBB), filled_page_record(4, 1, 0xCC), // no Commit
        ];
        let dir = recover_records("restore_uncommitted", &records, None);

        let file = std::fs::read(dir.join("pages.dat")).unwrap();
        assert_eq!(file.len(), PAGE_SIZE); // page 1 was not written
        assert!(page_in(&file, 0).iter().all(|&b| b == 0xAA));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_without_a_committed_full_page_write_does_not_open_pages_dat() {
        // only a CatalogWrite is committed
        let dir = recover_records("restore_catalog_only", &[catalog_record(1), commit_record(2)], None);
        assert!(!dir.join("pages.dat").exists());
        let _ = std::fs::remove_dir_all(dir);

        // a FullPageWrite without a Commit: not created, and an existing file is left as it is
        let uncommitted = [filled_page_record(1, 0, 0xAA)];
        let dir = recover_records("restore_no_commit", &uncommitted, None);
        assert!(!dir.join("pages.dat").exists());
        let _ = std::fs::remove_dir_all(dir);

        let existing = vec![0x77u8; 2 * PAGE_SIZE];
        let dir = recover_records("restore_no_commit_existing", &uncommitted, Some(existing.clone()));
        assert_eq!(std::fs::read(dir.join("pages.dat")).unwrap(), existing);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovering_twice_gives_the_same_pages_dat() {
        let records = [
            filled_page_record(1, 1, 0x11), commit_record(2),
            filled_page_record(3, 0, 0x22), filled_page_record(4, 2, 0x33), commit_record(5),
        ];
        let dir = recover_records("restore_twice", &records, None);
        let after_first = std::fs::read(dir.join("pages.dat")).unwrap();

        recover(&dir).unwrap();

        assert_eq!(std::fs::read(dir.join("pages.dat")).unwrap(), after_first);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Recovery: writing the last committed catalog back to catalog.json ----

    fn catalog_record_with(lsn: u64, json: &str) -> WalRecord {
        WalRecord { lsn, body: WalBody::CatalogWrite { catalog_json: json.as_bytes().to_vec() } }
    }

    #[test]
    fn recover_writes_only_the_last_committed_catalog() {
        let records = [
            catalog_record_with(1, "A"), commit_record(2),
            catalog_record_with(3, "B"), commit_record(4),
            catalog_record_with(5, "C"), // no Commit: not applied
        ];
        let dir = recover_records("catalog_last", &records, None);

        assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), b"B");
        assert!(!dir.join("pages.dat").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_does_not_touch_catalog_json_without_a_committed_catalog_write() {
        let broken = b"not json".to_vec();
        let cases: [(&str, Vec<WalRecord>); 2] = [
            ("catalog_none_pages_only", vec![filled_page_record(1, 0, 0xAA), commit_record(2)]),
            ("catalog_none_uncommitted", vec![catalog_record_with(1, "C")]),
        ];
        for (name, records) in cases {
            // no catalog.json: it is not created
            let dir = recover_records(name, &records, None);
            assert!(!dir.join("catalog.json").exists(), "{} created catalog.json", name);
            let _ = std::fs::remove_dir_all(dir);

            // a broken catalog.json: Recovery does not read it and leaves it as it is
            let dir = recovery_dir(&format!("{}_existing", name));
            std::fs::write(dir.join("wal.log"), encode(&records.iter().collect::<Vec<_>>())).unwrap();
            std::fs::write(dir.join("catalog.json"), &broken).unwrap();
            recover(&dir).unwrap();
            assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), broken, "{}", name);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn recovering_twice_gives_the_same_catalog_json() {
        let records = [catalog_record_with(1, "A"), commit_record(2), catalog_record_with(3, "B"), commit_record(4)];
        let dir = recover_records("catalog_twice", &records, None);
        let after_first = std::fs::read(dir.join("catalog.json")).unwrap();

        recover(&dir).unwrap();

        assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), after_first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_fails_when_catalog_json_cannot_be_opened_and_leaves_the_wal_intact() {
        let dir = recovery_dir("catalog_unopenable");
        let wal = encode(&[&catalog_record_with(1, "A"), &commit_record(2)]);
        std::fs::write(dir.join("wal.log"), &wal).unwrap();
        std::fs::create_dir(dir.join("catalog.json")).unwrap(); // a directory, not a file

        let error = recover(&dir).unwrap_err();

        assert!(error.contains("Failed to open"), "unexpected error: {}", error);
        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), wal);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Recovery: cutting off the WAL after the last Commit record ----

    /// Run Recovery on a directory holding only this WAL bytes and return the directory
    fn recover_wal_bytes(test_name: &str, wal_bytes: &[u8]) -> PathBuf {
        let dir = recovery_dir(test_name);
        std::fs::write(dir.join("wal.log"), wal_bytes).unwrap();
        recover(&dir).unwrap();
        dir
    }

    #[test]
    fn recover_cuts_off_records_after_the_last_commit() {
        let committed = encode(&[&filled_page_record(1, 0, 0xAA), &commit_record(2)]);
        let mut bytes = committed.clone();
        bytes.extend(encode(&[&filled_page_record(3, 1, 0xCC), &catalog_record_with(4, "C")])); // no Commit

        let dir = recover_wal_bytes("truncate_tail", &bytes);

        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), committed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_cuts_the_wal_to_zero_when_there_is_no_commit() {
        let bytes = encode(&[&filled_page_record(1, 0, 0xAA), &catalog_record_with(2, "C")]);

        let dir = recover_wal_bytes("truncate_all", &bytes);

        assert_eq!(std::fs::metadata(dir.join("wal.log")).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_leaves_a_wal_that_ends_with_a_commit_unchanged() {
        let bytes = encode(&[
            &filled_page_record(1, 0, 0xAA), &commit_record(2),
            &catalog_record_with(3, "B"), &commit_record(4),
        ]);

        let dir = recover_wal_bytes("truncate_nothing", &bytes);

        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lsn_resumes_after_the_last_record_that_is_left() {
        // records 3 and 4 (no Commit) are cut off: the last record left is 2, so the next LSN is 3
        let bytes = encode(&[
            &filled_page_record(1, 0, 0xAA), &commit_record(2),
            &filled_page_record(3, 1, 0xCC), &catalog_record_with(4, "C"),
        ]);
        let dir = recover_wal_bytes("lsn_resume", &bytes);
        assert_eq!(Wal::open(&dir.join("wal.log")).unwrap().next_lsn(), 3);
        let _ = std::fs::remove_dir_all(dir);

        // everything is cut off: the next LSN is 1
        let dir = recover_wal_bytes("lsn_resume_empty", &encode(&[&filled_page_record(1, 0, 0xAA)]));
        assert_eq!(Wal::open(&dir.join("wal.log")).unwrap().next_lsn(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_uncommitted_tail_is_not_applied_after_a_new_transaction_is_committed() {
        let records = [
            filled_page_record(1, 0, 0xAA), commit_record(2),
            filled_page_record(3, 1, 0xCC), // never committed
        ];
        let dir = recover_records("truncate_then_append", &records, None);

        // the next start appends a new transaction to the truncated WAL
        let wal_path = dir.join("wal.log");
        let mut wal = Wal::open(&wal_path).unwrap();
        assert_eq!(wal.next_lsn(), 3);
        wal.append(WalBody::FullPageWrite { page_id: 0, page_data: [0xBB; PAGE_SIZE] }).unwrap();
        wal.append(WalBody::Commit).unwrap();
        wal.sync().unwrap();
        drop(wal);

        recover(&dir).unwrap();

        // the new transaction is applied; the old uncommitted page 1 is not
        let file = std::fs::read(dir.join("pages.dat")).unwrap();
        assert_eq!(file.len(), PAGE_SIZE);
        assert!(page_in(&file, 0).iter().all(|&b| b == 0xBB));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovering_twice_gives_the_same_wal() {
        let mut bytes = encode(&[&filled_page_record(1, 0, 0xAA), &commit_record(2)]);
        bytes.extend(encode(&[&filled_page_record(3, 1, 0xCC)]));
        let dir = recover_wal_bytes("truncate_twice", &bytes);
        let after_first = std::fs::read(dir.join("wal.log")).unwrap();

        recover(&dir).unwrap();

        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), after_first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_wal_is_not_cut_when_writing_back_fails() {
        let dir = recovery_dir("truncate_after_failure");
        let mut bytes = encode(&[&filled_page_record(1, 0, 0xAA), &commit_record(2)]);
        bytes.extend(encode(&[&filled_page_record(3, 1, 0xCC)])); // uncommitted tail
        std::fs::write(dir.join("wal.log"), &bytes).unwrap();
        std::fs::create_dir(dir.join("pages.dat")).unwrap(); // pages.dat cannot be opened

        assert!(recover(&dir).is_err());

        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_fails_when_pages_dat_cannot_be_opened() {
        let dir = recovery_dir("restore_unopenable");
        let records = [filled_page_record(1, 0, 0xAA), commit_record(2)];
        std::fs::write(dir.join("wal.log"), encode(&records.iter().collect::<Vec<_>>())).unwrap();
        std::fs::create_dir(dir.join("pages.dat")).unwrap(); // a directory, not a file

        let error = recover(&dir).unwrap_err();

        assert!(error.contains("Failed to open"), "unexpected error: {}", error);
        let _ = std::fs::remove_dir_all(dir);
    }
}
