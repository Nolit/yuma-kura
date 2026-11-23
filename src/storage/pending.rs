// src/storage/pending.rs
// Uncommitted changes held by PagedStorage until commit_changes.
// Storage-internal type: must not appear in the Storage trait signature.

/// One uncommitted change, stored as a full image (FPW style).
#[derive(Debug, Clone, PartialEq)]
pub enum PendingChange {
    /// Full image of a page after the change
    PageWrite {
        page_id: u32,
        page_data: [u8; 4096],
    },
    /// Full catalog (JSON bytes) after the change
    CatalogWrite {
        catalog_json: Vec<u8>,
    },
}
