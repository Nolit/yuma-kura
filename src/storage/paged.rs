// src/storage/paged.rs
// 4KB fixed-page storage implementation
// Page data is strictly 4KB, metadata is separated into catalog.json

use crate::ast::{ColumnDef, Assignment, Predicate, CmpOp};
use crate::engine::value::Value;
use super::{Storage, Row, FatalError};
use super::pending::PendingChange;
use super::wal::{Wal, WalBody};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::PathBuf;
use serde::{Serialize, Deserialize};

mod recovery;

/// Page size: 4KB (4096 bytes)
const PAGE_SIZE: usize = 4096;
/// Maximum size of a single data item: 1KB
const MAX_DATA_SIZE: usize = 1024;
/// Page header size
const PAGE_HEADER_SIZE: usize = 28;

/// Page-aligned memory structure
#[repr(align(4096))]
#[derive(Clone)]
pub struct PageBytes([u8; PAGE_SIZE]);

impl PageBytes {
    pub fn new() -> Self {
        Self([0; PAGE_SIZE])
    }
    
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.0
    }
    
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// Page header (first 28 bytes of a page)
#[repr(C, packed)]
struct PageHeader {
    magic: u32,           // Magic number: 0x50414745 ("PAGE")
    page_id: u32,         // Page ID (global)
    free_space_offset: u16, // Start offset of free space (from data section)
    row_count: u16,       // Number of rows stored in this page
    next_page: u32,       // Next page ID (for overflow pages, 0=none)
    flags: u16,           // Flags (for future use)
    _reserved: u16,       // Reserved (padding before last_applied_lsn)
    last_applied_lsn: u64, // LSN of the last WAL record applied to this page (0 = no WAL applied)
}

impl PageHeader {
    const MAGIC: u32 = 0x50414745; // "PAGE"
    
    fn new(page_id: u32) -> Self {
        Self {
            magic: Self::MAGIC,
            page_id,
            free_space_offset: PAGE_HEADER_SIZE as u16,
            row_count: 0,
            next_page: 0,
            flags: 0,
            _reserved: 0,
            last_applied_lsn: 0,
        }
    }
    
    fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < PAGE_HEADER_SIZE {
            return Err("Invalid page header size".to_string());
        }
        
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != Self::MAGIC {
            return Err("Invalid page magic number".to_string());
        }
        
        Ok(Self {
            magic,
            page_id: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            free_space_offset: u16::from_le_bytes([bytes[8], bytes[9]]),
            row_count: u16::from_le_bytes([bytes[10], bytes[11]]),
            next_page: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            flags: u16::from_le_bytes([bytes[16], bytes[17]]),
            _reserved: u16::from_le_bytes([bytes[18], bytes[19]]),
            last_applied_lsn: u64::from_le_bytes([
                bytes[20], bytes[21], bytes[22], bytes[23],
                bytes[24], bytes[25], bytes[26], bytes[27],
            ]),
        })
    }
    
    fn to_bytes(&self) -> [u8; PAGE_HEADER_SIZE] {
        let mut bytes = [0u8; PAGE_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.page_id.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.free_space_offset.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.row_count.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.next_page.to_le_bytes());
        bytes[16..18].copy_from_slice(&self.flags.to_le_bytes());
        bytes[18..20].copy_from_slice(&self._reserved.to_le_bytes());
        bytes[20..28].copy_from_slice(&self.last_applied_lsn.to_le_bytes());
        bytes
    }
    
    fn free_space(&self) -> usize {
        (PAGE_SIZE - self.free_space_offset as usize).max(0)
    }
}

/// Row header (supports variable-length data)
#[repr(C, packed)]
struct RowHeader {
    row_id: u32,          // Row ID
    data_size: u16,       // Size of data part (bytes)
    deleted: u8,          // Deletion flag (0: valid, 1: deleted)
    overflow: u8,         // Overflow flag (0: none, 1: next page, 2: external storage)
    next_page: u32,       // Overflow page ID (when overflow=1)
    toast_id: u64,        // TOAST ID (when overflow=2, for future use)
}

impl RowHeader {
    const SIZE: usize = 20; // row_id(4) + data_size(2) + deleted(1) + overflow(1) + next_page(4) + toast_id(8)
    
    fn new(row_id: u32, data_size: u16) -> Self {
        Self {
            row_id,
            data_size,
            deleted: 0,
            overflow: 0,
            next_page: 0,
            toast_id: 0,
        }
    }
    
    fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < Self::SIZE {
            return Err("Invalid row header size".to_string());
        }
        
        Ok(Self {
            row_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            data_size: u16::from_le_bytes([bytes[4], bytes[5]]),
            deleted: bytes[6],
            overflow: bytes[7],
            next_page: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            toast_id: u64::from_le_bytes([
                bytes[12], bytes[13], bytes[14], bytes[15],
                bytes[16], bytes[17], bytes[18], bytes[19],
            ]),
        })
    }
    
    fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.row_id.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.data_size.to_le_bytes());
        bytes[6] = self.deleted;
        bytes[7] = self.overflow;
        bytes[8..12].copy_from_slice(&self.next_page.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.toast_id.to_le_bytes());
        bytes
    }
}

/// Table metadata (stored in catalog.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TableMetadata {
    schema: Vec<ColumnDef>,
    next_row_id: usize,
    first_page_id: u32,   // First page ID of this table
    last_page_id: u32,     // Last page ID of this table
}

/// Catalog information (stored in catalog.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Catalog {
    tables: HashMap<String, TableMetadata>,
    next_page_id: u32,    // Next available page ID (global)
}

/// 4KB fixed-page storage
/// Stores data of all tables in a single file
pub struct PagedStorage {
    base_path: PathBuf,
    catalog: Catalog,
    page_file: Option<File>, // Page file (shared by all tables)
    /// Pages modified during the current SQL (page_id -> page image).
    /// read_page consults this map before the page file.
    pending_pages: HashMap<u32, Box<PageBytes>>,
    /// Copy of the catalog modified during the current SQL.
    /// `catalog` is replaced by it only after the WAL commit is durable.
    working_catalog: Option<Catalog>,
    /// The write-ahead log (`wal.log` in the same directory)
    wal: Wal,
}

impl PagedStorage {
    pub fn new(file_path: String) -> Result<Self, String> {
        let base_path = PathBuf::from(file_path);
        
        // Create directory
        std::fs::create_dir_all(&base_path)
            .map_err(|e| format!("Failed to create directory: {}", e))?;

        // Startup order: create directory -> Recovery -> load catalog.json -> open
        // pages.dat -> open the WAL. Recovery comes first because it may rewrite
        // catalog.json and pages.dat, and it cuts the WAL, which must be done before the
        // WAL is opened for appending. A Recovery failure fails the startup.
        recovery::recover(&base_path)?;
        
        // Load catalog (after Recovery has written back the committed one)
        let catalog_path = base_path.join("catalog.json");
        let catalog = if catalog_path.exists() {
            let content = std::fs::read_to_string(&catalog_path)
                .map_err(|e| format!("Failed to read catalog.json: {}", e))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse catalog.json: {}", e))?
        } else {
            Catalog {
                tables: HashMap::new(),
                next_page_id: 0,
            }
        };
        
        // Open page file (shared by all tables)
        let page_file_path = base_path.join("pages.dat");
        let page_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&page_file_path)
            .map_err(|e| format!("Failed to open page file: {}", e))?;

        // Open the WAL (last in the startup order; Recovery will run before the catalog load)
        let wal = Wal::open(&base_path.join("wal.log"))?;
        
        Ok(Self {
            base_path,
            catalog,
            page_file: Some(page_file),
            pending_pages: HashMap::new(),
            working_catalog: None,
            wal,
        })
    }

    /// Start of a data-changing SQL. One transaction is one SQL, so the previous SQL must
    /// have been committed or aborted: otherwise its staged pages would be mixed with a
    /// working catalog that no longer matches them.
    fn begin_sql(&mut self) -> Result<(), String> {
        if self.working_catalog.is_some() || !self.pending_pages.is_empty() {
            return Err("previous statement was neither committed nor aborted".to_string());
        }
        self.begin_working_catalog();
        Ok(())
    }

    /// Start a working copy of the committed catalog (called at the start of an SQL)
    fn begin_working_catalog(&mut self) {
        self.working_catalog = Some(self.catalog.clone());
    }

    /// The catalog to read during an SQL: the working copy if present, else the committed one
    fn active_catalog(&self) -> &Catalog {
        self.working_catalog.as_ref().unwrap_or(&self.catalog)
    }

    /// The catalog to modify during an SQL. Never touches the committed catalog.
    fn active_catalog_mut(&mut self) -> Result<&mut Catalog, String> {
        self.working_catalog.as_mut()
            .ok_or_else(|| "No working catalog: SQL not started".to_string())
    }
    
    /// Serialize a catalog to the catalog.json format (shared by catalog.json and CatalogWrite)
    fn serialize_catalog(catalog: &Catalog) -> Result<Vec<u8>, String> {
        serde_json::to_string_pretty(catalog)
            .map(String::into_bytes)
            .map_err(|e| format!("Failed to serialize catalog: {}", e))
    }

    /// Save catalog
    fn save_catalog(&self) -> Result<(), String> {
        let catalog_path = self.base_path.join("catalog.json");
        let content = Self::serialize_catalog(&self.catalog)?;
        
        std::fs::write(&catalog_path, content)
            .map_err(|e| format!("Failed to write catalog.json: {}", e))?;
        
        Ok(())
    }

    /// Build the PendingChange list for the current SQL from pending_pages and
    /// working_catalog. Read-only: neither of them is modified.
    /// - one PageWrite per staged page, in page_id order
    /// - one CatalogWrite (after the PageWrites) if the working catalog differs from the committed one
    fn build_pending_changes(&self) -> Result<Vec<PendingChange>, String> {
        let mut page_ids: Vec<u32> = self.pending_pages.keys().copied().collect();
        page_ids.sort_unstable();

        let mut changes = Vec::with_capacity(page_ids.len() + 1);
        for page_id in page_ids {
            let page = &self.pending_pages[&page_id];
            let mut page_data = [0u8; PAGE_SIZE];
            page_data.copy_from_slice(page.as_slice());
            changes.push(PendingChange::PageWrite { page_id, page_data });
        }

        if let Some(working) = &self.working_catalog {
            // Compare as serde_json::Value so that HashMap iteration order does not matter
            let committed_value = serde_json::to_value(&self.catalog)
                .map_err(|e| format!("Failed to serialize catalog: {}", e))?;
            let working_value = serde_json::to_value(working)
                .map_err(|e| format!("Failed to serialize catalog: {}", e))?;
            if committed_value != working_value {
                changes.push(PendingChange::CatalogWrite {
                    catalog_json: Self::serialize_catalog(working)?,
                });
            }
        }

        Ok(changes)
    }

    /// Write `lsn` into the `last_applied_lsn` field of a page image's header.
    /// Only `paged.rs` knows the page layout; the WAL does not.
    fn stamp_last_applied_lsn(page: &mut [u8; PAGE_SIZE], lsn: u64) -> Result<(), String> {
        let mut header = PageHeader::from_bytes(&page[0..PAGE_HEADER_SIZE])?;
        header.last_applied_lsn = lsn;
        page[0..PAGE_HEADER_SIZE].copy_from_slice(&header.to_bytes());
        Ok(())
    }

    /// Append `changes` to the WAL as one transaction, then a Commit record, then fsync.
    /// - PageWrite: the page image is copied, stamped with the LSN it is about to get
    ///   and appended as FullPageWrite (the `PendingChange` itself is not modified).
    /// - CatalogWrite: appended as CatalogWrite.
    /// Returns the LSN-stamped page images, in the same order, so that pages.dat can
    /// be written with exactly the bytes that are in the WAL.
    /// An empty `changes` writes nothing (no Commit-only transaction).
    fn append_changes_to_wal(&mut self, changes: &[PendingChange]) -> Result<Vec<(u32, Box<PageBytes>)>, String> {
        if changes.is_empty() {
            return Ok(Vec::new());
        }

        let mut stamped_pages = Vec::new();
        for change in changes {
            match change {
                PendingChange::PageWrite { page_id, page_data } => {
                    let mut image = *page_data;
                    let lsn = self.wal.next_lsn();
                    Self::stamp_last_applied_lsn(&mut image, lsn)?;
                    let appended = self.wal.append(WalBody::FullPageWrite { page_id: *page_id, page_data: image })?;
                    if appended != lsn {
                        return Err(format!("WAL assigned LSN {} but the page was stamped with {}", appended, lsn));
                    }
                    let mut page = Box::new(PageBytes::new());
                    page.as_mut_slice().copy_from_slice(&image);
                    stamped_pages.push((*page_id, page));
                }
                PendingChange::CatalogWrite { catalog_json } => {
                    self.wal.append(WalBody::CatalogWrite { catalog_json: catalog_json.clone() })?;
                }
            }
        }

        self.wal.append(WalBody::Commit)?;
        self.wal.sync()?;
        Ok(stamped_pages)
    }

    /// Make the current SQL's changes durable. Every error in here is fatal.
    /// 1. build the PendingChange list
    /// 2. append it + Commit to the WAL and fsync (the commit is decided here)
    /// 3. replace the committed catalog with the working catalog
    /// 4. write the LSN-stamped pages (the same bytes as in the WAL) to pages.dat
    /// 5. write catalog.json if the catalog changed
    /// 6. drop the staged pages
    /// On failure nothing is rolled back or discarded: the caller must stop the
    /// process and let Recovery repair the files at the next start.
    fn commit_changes_inner(&mut self) -> Result<(), String> {
        let changes = self.build_pending_changes()?;
        let stamped_pages = self.append_changes_to_wal(&changes)?;

        if let Some(working) = self.working_catalog.take() {
            self.catalog = working;
        }

        for (page_id, page) in &stamped_pages {
            self.write_page_to_file(*page_id, page)?;
        }
        if changes.iter().any(|c| matches!(c, PendingChange::CatalogWrite { .. })) {
            self.save_catalog()?;
        }

        self.pending_pages.clear();
        Ok(())
    }
    
    /// Read a page: pending_pages first, then the page file
    fn read_page(&mut self, page_id: u32) -> Result<Box<PageBytes>, String> {
        if let Some(page) = self.pending_pages.get(&page_id) {
            return Ok(page.clone());
        }
        self.read_page_from_file(page_id)
    }

    /// Read a page directly from the page file (ignores pending_pages)
    fn read_page_from_file(&mut self, page_id: u32) -> Result<Box<PageBytes>, String> {
        let file = self.page_file.as_mut()
            .ok_or("Page file not open".to_string())?;
        
        let offset = page_id as u64 * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to page {}: {}", page_id, e))?;
        
        let mut page = Box::new(PageBytes::new());
        file.read_exact(page.as_mut_slice())
            .map_err(|e| format!("Failed to read page {}: {}", page_id, e))?;
        
        Ok(page)
    }
    
    /// Stage a modified page in pending_pages. Nothing is written to the page file here.
    fn write_page(&mut self, page_id: u32, page: &PageBytes) -> Result<(), String> {
        self.pending_pages.insert(page_id, Box::new(page.clone()));
        Ok(())
    }

    /// Write a page directly to the page file
    fn write_page_to_file(&mut self, page_id: u32, page: &PageBytes) -> Result<(), String> {
        let file = self.page_file.as_mut()
            .ok_or("Page file not open".to_string())?;
        
        let offset = page_id as u64 * PAGE_SIZE as u64;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to page {}: {}", page_id, e))?;
        
        file.write_all(page.as_slice())
            .map_err(|e| format!("Failed to write page {}: {}", page_id, e))?;
        
        file.flush()
            .map_err(|e| format!("Failed to flush page {}: {}", page_id, e))?;
        
        Ok(())
    }
    
    /// Create a new page
    fn create_new_page(&mut self) -> Result<u32, String> {
        let catalog = self.active_catalog_mut()?;
        let page_id = catalog.next_page_id;
        catalog.next_page_id += 1;
        
        let mut page = Box::new(PageBytes::new());
        let header = PageHeader::new(page_id);
        page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&header.to_bytes());
        
        self.write_page(page_id, &page)?;
        
        Ok(page_id)
    }
    
    /// Serialize a row (supports variable-length data)
    fn serialize_row(row: &Row, schema: &[ColumnDef]) -> Result<Vec<u8>, String> {
        let mut buffer = Vec::new();
        
        for (i, value) in row.iter().enumerate() {
            if i >= schema.len() {
                return Err("Row length exceeds schema".to_string());
            }
            
            match (&schema[i].data_type, value) {
                (crate::ast::ColumnType::Int, Value::Int(n)) => {
                    buffer.extend_from_slice(&n.to_le_bytes());
                }
                (crate::ast::ColumnType::Text, Value::Text(s)) => {
                    let text_bytes = s.as_bytes();
                    if text_bytes.len() > MAX_DATA_SIZE {
                        return Err(format!("Text value too large: {} bytes (max: {} bytes)", 
                            text_bytes.len(), MAX_DATA_SIZE));
                    }
                    
                    // Variable-length: length (2 bytes) + data
                    let len = text_bytes.len() as u16;
                    buffer.extend_from_slice(&len.to_le_bytes());
                    buffer.extend_from_slice(text_bytes);
                }
                (crate::ast::ColumnType::Bool, Value::Bool(b)) => {
                    buffer.push(if *b { 1 } else { 0 });
                }
                (crate::ast::ColumnType::Text, Value::Null) | 
                (crate::ast::ColumnType::Int, Value::Null) |
                (crate::ast::ColumnType::Bool, Value::Null) => {
                    // Handle NULL values
                    match schema[i].data_type {
                        crate::ast::ColumnType::Int => {
                            buffer.extend_from_slice(&0i64.to_le_bytes());
                        }
                        crate::ast::ColumnType::Text => {
                            // NULL string has length 0
                            buffer.extend_from_slice(&0u16.to_le_bytes());
                        }
                        crate::ast::ColumnType::Bool => {
                            buffer.push(0);
                        }
                    }
                }
                _ => {
                    return Err(format!("Type mismatch for column {}: expected {:?}, got {:?}", 
                        schema[i].name, schema[i].data_type, value));
                }
            }
        }
        
        Ok(buffer)
    }
    
    /// Deserialize a row
    fn deserialize_row(data: &[u8], schema: &[ColumnDef]) -> Result<Row, String> {
        let mut row = Vec::new();
        let mut offset = 0;
        
        for col_def in schema {
            match col_def.data_type {
                crate::ast::ColumnType::Int => {
                    if offset + 8 > data.len() {
                        return Err("Insufficient data for Int".to_string());
                    }
                    let value = i64::from_le_bytes([
                        data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
                        data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7],
                    ]);
                    row.push(Value::Int(value));
                    offset += 8;
                }
                crate::ast::ColumnType::Text => {
                    if offset + 2 > data.len() {
                        return Err("Insufficient data for Text length".to_string());
                    }
                    let len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
                    offset += 2;
                    
                    if offset + len > data.len() {
                        return Err("Insufficient data for Text".to_string());
                    }
                    
                    let text = String::from_utf8_lossy(&data[offset..offset + len]).to_string();
                    row.push(Value::Text(text));
                    offset += len;
                }
                crate::ast::ColumnType::Bool => {
                    if offset + 1 > data.len() {
                        return Err("Insufficient data for Bool".to_string());
                    }
                    let value = data[offset] != 0;
                    row.push(Value::Bool(value));
                    offset += 1;
                }
            }
        }
        
        Ok(row)
    }
    
    /// Check if a page has free space
    fn find_page_with_space(&mut self, table_name: &str, required_space: usize) -> Result<Option<u32>, String> {
        let table_meta = self.active_catalog().tables.get(table_name)
            .ok_or("table not found")?;
        
        // Check from the last page
        let current_page_id = table_meta.last_page_id;
        
        let page = self.read_page(current_page_id)?;
        let header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
        
        if header.free_space() >= required_space {
            return Ok(Some(current_page_id));
        }
        
        // TODO: Need a mechanism to manage page chains
        
        Ok(None)
    }
    
    /// Add a row to a page
    fn add_row_to_page(&mut self, page_id: u32, row_id: u32, row_data: &[u8]) -> Result<(), String> {
        let mut page = self.read_page(page_id)?;
        let mut header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
        
        let row_size = RowHeader::SIZE + row_data.len();
        
        if row_size > header.free_space() {
            return Err(format!("Not enough space in page {}: required {}, available {}", 
                page_id, row_size, header.free_space()));
        }
        
        // Add row
        let data_offset = header.free_space_offset as usize;
        let row_header = RowHeader::new(row_id, row_data.len() as u16);
        
        page.as_mut_slice()[data_offset..data_offset + RowHeader::SIZE]
            .copy_from_slice(&row_header.to_bytes());
        page.as_mut_slice()[data_offset + RowHeader::SIZE..data_offset + RowHeader::SIZE + row_data.len()]
            .copy_from_slice(row_data);
        
        // Update page header
        header.free_space_offset = (data_offset + RowHeader::SIZE + row_data.len()) as u16;
        header.row_count += 1;
        page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&header.to_bytes());
        
        self.write_page(page_id, &page)?;
        
        Ok(())
    }
    
    /// Read all rows from a page
    fn read_rows_from_page(&mut self, page_id: u32, schema: &[ColumnDef]) -> Result<Vec<(u32, Row)>, String> {
        let page = self.read_page(page_id)?;
        let header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
        
        let mut rows = Vec::new();
        let mut data_offset = PAGE_HEADER_SIZE;
        
        for _ in 0..header.row_count {
            if data_offset + RowHeader::SIZE > PAGE_SIZE {
                break;
            }
            
            let row_header = RowHeader::from_bytes(&page.as_slice()[data_offset..data_offset + RowHeader::SIZE])?;
            
            if row_header.deleted == 0 {
                let row_data_start = data_offset + RowHeader::SIZE;
                let row_data_end = row_data_start + row_header.data_size as usize;
                
                if row_data_end > PAGE_SIZE {
                    break;
                }
                
                let row_data = &page.as_slice()[row_data_start..row_data_end];
                
                // Handle overflow pages (future implementation)
                let overflow = row_header.overflow;
                let row_id = row_header.row_id;
                if overflow == 1 {
                    // TODO: Read data from next page
                    eprintln!("Warning: Overflow page not yet implemented for row {}", row_id);
                } else if overflow == 2 {
                    // TODO: Read data from TOAST
                    eprintln!("Warning: TOAST not yet implemented for row {}", row_id);
                }
                
                let row_id = row_header.row_id;
                match Self::deserialize_row(row_data, schema) {
                    Ok(row) => rows.push((row_id, row)),
                    Err(e) => {
                        eprintln!("Warning: Failed to deserialize row {}: {}", row_id, e);
                    }
                }
            }
            
            data_offset = data_offset + RowHeader::SIZE + row_header.data_size as usize;
        }
        
        Ok(rows)
    }
    
    /// Update or delete rows in a page
    fn update_rows_in_page(&mut self, table_name: &str, page_id: u32, schema: &[ColumnDef],
                          assignments: Option<&[Assignment]>, filter: Option<&Predicate>) -> Result<usize, String> {
        let mut page = self.read_page(page_id)?;
        let mut header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
        
        let mut updated_count = 0;
        let mut data_offset = PAGE_HEADER_SIZE;
        
        for _ in 0..header.row_count {
            if data_offset + RowHeader::SIZE > PAGE_SIZE {
                break;
            }
            
            let row_header_bytes = &page.as_slice()[data_offset..data_offset + RowHeader::SIZE];
            let mut row_header = RowHeader::from_bytes(row_header_bytes)?;
            
            if row_header.deleted == 0 {
                let row_data_start = data_offset + RowHeader::SIZE;
                let row_data_end = row_data_start + row_header.data_size as usize;
                
                if row_data_end > PAGE_SIZE {
                    break;
                }
                
                let row_data = &page.as_slice()[row_data_start..row_data_end];
                let mut row = Self::deserialize_row(row_data, schema)?;
                
                // Check filter condition
                let matches = if let Some(predicate) = filter {
                    evaluate_predicate(predicate, &row, schema)
                } else {
                    true
                };
                
                if matches {
                    if let Some(assignments) = assignments {
                        // Update
                        for assignment in assignments.iter() {
                            if let Some(col_idx) = schema.iter().position(|c| c.name == assignment.column) {
                                let parsed_value = parse_value(&assignment.value, &schema[col_idx].data_type)?;
                                row[col_idx] = parsed_value;
                            } else {
                                return Err(format!("Column '{}' not found", assignment.column));
                            }
                        }
                        
                        // Reserialize row
                        let new_row_data = Self::serialize_row(&row, schema)?;
                        let new_row_size = RowHeader::SIZE + new_row_data.len();
                        
                        if new_row_data.len() <= row_header.data_size as usize {
                            // Row size stayed the same or decreased: update in place.
                            // `data_size` is the space allocated to this row and is left
                            // unchanged. The next row's offset is computed from it, and
                            // rows are not compacted, so shrinking it would shift every
                            // following row. The unused tail is zero-filled; the
                            // deserializer ignores bytes after the last column.
                            let slot = &mut page.as_mut_slice()[row_data_start..row_data_end];
                            slot[..new_row_data.len()].copy_from_slice(&new_row_data);
                            slot[new_row_data.len()..].fill(0);
                            updated_count += 1;
                        } else {
                            // Row size increased: mark old location as deleted and add new row
                            // Mark old row as deleted
                            row_header.deleted = 1;
                            page.as_mut_slice()[data_offset..data_offset + RowHeader::SIZE]
                                .copy_from_slice(&row_header.to_bytes());
                            
                            // Check if we can fit the new row in this page
                            let available_space = header.free_space();
                            if available_space >= new_row_size {
                                // Add new row at the end of the page
                                let new_data_offset = header.free_space_offset as usize;
                                let new_row_header = RowHeader::new(row_header.row_id, new_row_data.len() as u16);
                                
                                page.as_mut_slice()[new_data_offset..new_data_offset + RowHeader::SIZE]
                                    .copy_from_slice(&new_row_header.to_bytes());
                                page.as_mut_slice()[new_data_offset + RowHeader::SIZE..new_data_offset + RowHeader::SIZE + new_row_data.len()]
                                    .copy_from_slice(&new_row_data);
                                
                                // Update page header
                                header.free_space_offset = (new_data_offset + RowHeader::SIZE + new_row_data.len()) as u16;
                                header.row_count += 1; // New row added (old one is marked deleted but still counted)
                                page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&header.to_bytes());
                                
                                updated_count += 1;
                            } else {
                                // Not enough space in this page - create a new page
                                let new_page_id = self.create_new_page()?;
                                
                                // Add new row to the new page
                                let new_row_header = RowHeader::new(row_header.row_id, new_row_data.len() as u16);
                                let mut new_page = self.read_page(new_page_id)?;
                                let mut new_page_header = PageHeader::from_bytes(&new_page.as_slice()[0..PAGE_HEADER_SIZE])?;
                                
                                let new_data_offset = new_page_header.free_space_offset as usize;
                                new_page.as_mut_slice()[new_data_offset..new_data_offset + RowHeader::SIZE]
                                    .copy_from_slice(&new_row_header.to_bytes());
                                new_page.as_mut_slice()[new_data_offset + RowHeader::SIZE..new_data_offset + RowHeader::SIZE + new_row_data.len()]
                                    .copy_from_slice(&new_row_data);
                                
                                new_page_header.free_space_offset = (new_data_offset + RowHeader::SIZE + new_row_data.len()) as u16;
                                new_page_header.row_count += 1;
                                new_page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&new_page_header.to_bytes());
                                
                                self.write_page(new_page_id, &new_page)?;
                                
                                // Append the new page to the end of the chain
                                let tail_page_id = self.active_catalog().tables.get(table_name)
                                    .ok_or("table not found")?
                                    .last_page_id;
                                if tail_page_id == page_id {
                                    // Current page is the tail: update the in-memory copy
                                    header.next_page = new_page_id;
                                    page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&header.to_bytes());
                                } else {
                                    let mut tail_page = self.read_page(tail_page_id)?;
                                    let mut tail_header = PageHeader::from_bytes(&tail_page.as_slice()[0..PAGE_HEADER_SIZE])?;
                                    tail_header.next_page = new_page_id;
                                    tail_page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&tail_header.to_bytes());
                                    self.write_page(tail_page_id, &tail_page)?;
                                }

                                // Update table metadata to track new page
                                if let Some(table_meta) = self.active_catalog_mut()?.tables.get_mut(table_name) {
                                    table_meta.last_page_id = new_page_id;
                                }
                                
                                updated_count += 1;
                            }
                        }
                    } else {
                        // Delete (set deletion flag)
                        row_header.deleted = 1;
                        page.as_mut_slice()[data_offset..data_offset + RowHeader::SIZE]
                            .copy_from_slice(&row_header.to_bytes());
                        updated_count += 1;
                    }
                }
            }
            
            data_offset = data_offset + RowHeader::SIZE + row_header.data_size as usize;
        }
        
        if updated_count > 0 {
            self.write_page(page_id, &page)?;
        }
        
        Ok(updated_count)
    }
}

// The bodies of the four data-changing operations. The `Storage` impl below wraps
// each of them with `begin_sql`.
impl PagedStorage {
    fn create_table_inner(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String> {
        let table_name = name.to_lowercase();
        
        if self.active_catalog().tables.contains_key(&table_name) {
            return Err("table exists".to_string());
        }
        
        // Create a new page
        let first_page_id = self.create_new_page()?;
        
        // Register table metadata
        self.active_catalog_mut()?.tables.insert(table_name.clone(), TableMetadata {
            schema: schema.clone(),
            next_row_id: 1,
            first_page_id,
            last_page_id: first_page_id,
        });
        
        Ok(())
    }
    
    fn insert_row_inner(&mut self, table: &str, row: Row) -> Result<(), String> {
        let table_name = table.to_lowercase();
        
        let (schema, row_id) = {
            let table_meta = self.active_catalog().tables.get(&table_name)
                .ok_or("table not found")?;
            
            if row.len() != table_meta.schema.len() {
                return Err("column count mismatch".to_string());
            }
            
            (table_meta.schema.clone(), table_meta.next_row_id as u32)
        };
        
        // Serialize row
        let row_data = Self::serialize_row(&row, &schema)?;
        let row_size = RowHeader::SIZE + row_data.len();
        
        // Error if exceeds 4KB (will split to next page in the future)
        if row_size > PAGE_SIZE - PAGE_HEADER_SIZE {
            return Err(format!("Row too large: {} bytes (max: {} bytes). Overflow pages not yet implemented.", 
                row_size, PAGE_SIZE - PAGE_HEADER_SIZE));
        }
        
        // Find a page with free space
        let target_page_id = match self.find_page_with_space(&table_name, row_size)? {
            Some(page_id) => page_id,
            None => {
                // Create a new page
                let prev_last_page_id = self.active_catalog().tables.get(&table_name)
                    .ok_or("table not found")?
                    .last_page_id;
                let new_page_id = self.create_new_page()?;
                
                // Link the previous last page to the new page
                let mut prev_page = self.read_page(prev_last_page_id)?;
                let mut prev_header = PageHeader::from_bytes(&prev_page.as_slice()[0..PAGE_HEADER_SIZE])?;
                prev_header.next_page = new_page_id;
                prev_page.as_mut_slice()[0..PAGE_HEADER_SIZE].copy_from_slice(&prev_header.to_bytes());
                self.write_page(prev_last_page_id, &prev_page)?;
                
                // Update metadata
                if let Some(table_meta) = self.active_catalog_mut()?.tables.get_mut(&table_name) {
                    table_meta.last_page_id = new_page_id;
                }
                
                new_page_id
            }
        };
        
        // Add row to page
        self.add_row_to_page(target_page_id, row_id, &row_data)?;
        
        // Update metadata
        if let Some(table_meta) = self.active_catalog_mut()?.tables.get_mut(&table_name) {
            table_meta.next_row_id += 1;
        }
        
        Ok(())
    }
    
    fn update_rows_inner(&mut self, table: &str, assignments: &[Assignment], filter: Option<&Predicate>) -> Result<usize, String> {
        let table_name = table.to_lowercase();
        let (schema, first_page_id, last_page_id) = {
            let table_meta = self.active_catalog().tables.get(&table_name)
                .ok_or("table not found")?;
            (table_meta.schema.clone(), table_meta.first_page_id, table_meta.last_page_id)
        };
        
        // Traverse page chain via next_page pointer.
        // Stop at the tail as of the start of this update: pages appended during
        // the update already contain updated rows and must not be processed again.
        let mut total_updated = 0;
        let mut current_page_id = first_page_id;
        loop {
            let updated = self.update_rows_in_page(&table_name, current_page_id, &schema, Some(assignments), filter)?;
            total_updated += updated;

            if current_page_id == last_page_id {
                break;
            }
            let page = self.read_page(current_page_id)?;
            let header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
            if header.next_page == 0 {
                break;
            }
            current_page_id = header.next_page;
        }
        
        Ok(total_updated)
    }
    
    fn delete_rows_inner(&mut self, table: &str, filter: Option<&Predicate>) -> Result<usize, String> {
        let table_name = table.to_lowercase();
        let (schema, first_page_id) = {
            let table_meta = self.active_catalog().tables.get(&table_name)
                .ok_or("table not found")?;
            (table_meta.schema.clone(), table_meta.first_page_id)
        };
        
        // Traverse page chain via next_page pointer
        let mut total_deleted = 0;
        let mut current_page_id = first_page_id;
        loop {
            let deleted = self.update_rows_in_page(&table_name, current_page_id, &schema, None, filter)?;
            total_deleted += deleted;

            let page = self.read_page(current_page_id)?;
            let header = PageHeader::from_bytes(&page.as_slice()[0..PAGE_HEADER_SIZE])?;
            if header.next_page == 0 {
                break;
            }
            current_page_id = header.next_page;
        }
        
        Ok(total_deleted)
    }
}

impl Storage for PagedStorage {
    /// See `commit_changes_inner`. Every error is fatal.
    fn commit_changes(&mut self) -> Result<(), FatalError> {
        self.commit_changes_inner().map_err(FatalError)
    }

    /// Throw away the staged pages and the working catalog. The committed catalog
    /// and the files are not touched, and nothing was written to the WAL yet.
    fn abort_changes(&mut self) {
        self.pending_pages.clear();
        self.working_catalog = None;
    }

    // The four data-changing operations only stage their changes (pending_pages and
    // the working catalog). `commit_changes` makes them durable; `abort_changes`
    // discards them. Both are called by the executor.
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String> {
        self.begin_sql()?;
        self.create_table_inner(name, schema)
    }

    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String> {
        self.begin_sql()?;
        self.insert_row_inner(table, row)
    }

    fn update_rows(&mut self, table: &str, assignments: &[Assignment], filter: Option<&Predicate>) -> Result<usize, String> {
        self.begin_sql()?;
        self.update_rows_inner(table, assignments, filter)
    }

    fn delete_rows(&mut self, table: &str, filter: Option<&Predicate>) -> Result<usize, String> {
        self.begin_sql()?;
        self.delete_rows_inner(table, filter)
    }

    fn schema(&self, table: &str) -> Result<&[ColumnDef], String> {
        let table_name = table.to_lowercase();
        self.catalog.tables
            .get(&table_name)
            .map(|t| t.schema.as_slice())
            .ok_or("table not found".to_string())
    }

    fn rows<'a>(&'a self, _table: &str) -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String> {
        // Need to return references, but difficult to implement with paged storage
        Err("Paged storage rows() method requires different implementation - use get_all_rows() instead".to_string())
    }

    fn get_all_rows(&self, table: &str) -> Result<Vec<Row>, String> {
        // Due to Storage trait constraints, must implement with &self
        // Internally needs mutable reference, so as a temporary solution
        // In actual implementation, should use interior mutability (RefCell, etc.) or
        // reopen file each time
        let table_name = table.to_lowercase();
        let table_meta = self.catalog.tables.get(&table_name)
            .ok_or("table not found")?;
        
        // Simple implementation: reopen file each time
        let page_file_path = self.base_path.join("pages.dat");
        let mut page_file = File::open(&page_file_path)
            .map_err(|e| format!("Failed to open page file: {}", e))?;
        
        let mut all_rows = Vec::new();
        let mut current_page_id = table_meta.first_page_id;
        
        loop {
            let offset = current_page_id as u64 * PAGE_SIZE as u64;
            page_file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("Failed to seek to page {}: {}", current_page_id, e))?;
            
            let mut page_data = vec![0u8; PAGE_SIZE];
            page_file.read_exact(&mut page_data)
                .map_err(|e| format!("Failed to read page {}: {}", current_page_id, e))?;
            
            let header = PageHeader::from_bytes(&page_data[0..PAGE_HEADER_SIZE])?;
            let mut data_offset = PAGE_HEADER_SIZE;
            
            for _ in 0..header.row_count {
                if data_offset + RowHeader::SIZE > PAGE_SIZE {
                    break;
                }
                
                let row_header = RowHeader::from_bytes(&page_data[data_offset..data_offset + RowHeader::SIZE])?;
                
                if row_header.deleted == 0 {
                    let row_data_start = data_offset + RowHeader::SIZE;
                    let row_data_end = row_data_start + row_header.data_size as usize;
                    
                    if row_data_end > PAGE_SIZE {
                        break;
                    }
                    
                    let row_data = &page_data[row_data_start..row_data_end];
                    let row_id = row_header.row_id;
                    match Self::deserialize_row(row_data, &table_meta.schema) {
                        Ok(row) => all_rows.push(row),
                        Err(e) => {
                            eprintln!("Warning: Failed to deserialize row {}: {}", row_id, e);
                        }
                    }
                }
                
                data_offset = data_offset + RowHeader::SIZE + row_header.data_size as usize;
            }
            
            if header.next_page == 0 {
                break;
            }
            current_page_id = header.next_page;
        }
        
        Ok(all_rows)
    }
}

// Helper functions
fn parse_value(raw: &str, ty: &crate::ast::ColumnType) -> Result<Value, String> {
    match ty {
        crate::ast::ColumnType::Int => raw.parse::<i64>().map(Value::Int)
            .map_err(|_| format!("expected INT, got {raw:?}")),
        crate::ast::ColumnType::Text => Ok(Value::Text(raw.to_string())),
        crate::ast::ColumnType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("expected BOOL, got {raw:?}")),
        },
    }
}

fn evaluate_predicate(predicate: &Predicate, row: &[Value], schema: &[ColumnDef]) -> bool {
    match predicate {
        Predicate::Cmp { col, op, val } => {
            if let Some(col_idx) = schema.iter().position(|c| &c.name == col) {
                if let Some(row_val) = row.get(col_idx) {
                    return evaluate_comparison(row_val, op, val);
                }
            }
            false
        }
        Predicate::And(left, right) => {
            evaluate_predicate(left, row, schema) && evaluate_predicate(right, row, schema)
        }
        Predicate::Or(left, right) => {
            evaluate_predicate(left, row, schema) || evaluate_predicate(right, row, schema)
        }
    }
}

fn evaluate_comparison(left: &Value, op: &CmpOp, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(l), Value::Int(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Text(l), Value::Text(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Bool(l), Value::Bool(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::wal::WalRecord;
    use crate::ast::ColumnType;
    use std::path::Path;

    /// Creates a fresh storage in a unique temp directory with table `t(id INT, name TEXT)`
    /// holding three rows whose names are 10 bytes long. All rows fit in one page.
    fn setup_keeping_wal(test_name: &str) -> (PagedStorage, String) {
        let dir = std::env::temp_dir()
            .join(format!("mydb_paged_{}_{}", test_name, std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = PagedStorage::new(dir.clone()).unwrap();
        s.create_table("t", vec![
            ColumnDef { name: "id".into(), data_type: ColumnType::Int },
            ColumnDef { name: "name".into(), data_type: ColumnType::Text },
        ]).unwrap();
        s.commit_changes().unwrap();
        for (id, name) in [(1, "aaaaaaaaaa"), (2, "bbbbbbbbbb"), (3, "cccccccccc")] {
            s.insert_row("t", vec![Value::Int(id), Value::Text(name.into())]).unwrap();
            s.commit_changes().unwrap();
        }
        (s, dir)
    }

    /// Empty the WAL file and restart LSN numbering at 1, so that a test only sees
    /// the WAL records it produces itself.
    fn clear_wal(s: &mut PagedStorage, dir: &str) {
        let path = std::path::Path::new(dir).join("wal.log");
        std::fs::write(&path, []).unwrap();
        s.wal = Wal::open(&path).unwrap();
    }

    /// Like `setup_keeping_wal`, but with an empty WAL
    fn setup(test_name: &str) -> (PagedStorage, String) {
        let (mut s, dir) = setup_keeping_wal(test_name);
        clear_wal(&mut s, &dir);
        (s, dir)
    }

    fn id_eq(id: i64) -> Predicate {
        Predicate::Cmp { col: "id".into(), op: CmpOp::Eq, val: Value::Int(id) }
    }

    fn set_name(value: &str) -> Vec<Assignment> {
        vec![Assignment { column: "name".into(), value: value.into() }]
    }

    fn row(id: i64, name: &str) -> Row {
        vec![Value::Int(id), Value::Text(name.into())]
    }

    #[test]
    fn write_page_stages_in_memory_until_committed() {
        let (mut s, dir) = setup("stage_write");
        let on_disk = s.read_page_from_file(0).unwrap();
        let mut staged = on_disk.clone();
        staged.as_mut_slice()[PAGE_SIZE - 1] = 0xCD;

        s.write_page(0, &staged).unwrap();
        // visible through read_page, but the page file is untouched
        assert_eq!(s.read_page(0).unwrap().as_slice(), staged.as_slice());
        assert_eq!(s.read_page_from_file(0).unwrap().as_slice(), on_disk.as_slice());

        // commit writes the page (stamped with its LSN) to the page file
        s.commit_changes().unwrap();
        let committed = s.read_page_from_file(0).unwrap();
        assert_eq!(committed.as_slice()[PAGE_SIZE - 1], 0xCD);
        let header = PageHeader::from_bytes(&committed.as_slice()[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!({ header.last_applied_lsn }, 1);
        assert!(s.pending_pages.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_pages_are_staged_until_commit_and_dropped_by_abort() {
        let (mut s, dir) = setup("pending_empty");
        assert!(s.pending_pages.is_empty());

        s.update_rows("t", &set_name("x"), Some(&id_eq(1))).unwrap();
        assert!(!s.pending_pages.is_empty()); // staged, not written yet
        s.commit_changes().unwrap();
        assert!(s.pending_pages.is_empty());
        s.delete_rows("t", Some(&id_eq(2))).unwrap();
        s.commit_changes().unwrap();
        assert!(s.pending_pages.is_empty());

        // a failing statement: abort_changes discards whatever was staged
        s.pending_pages.insert(0, Box::new(PageBytes::new()));
        s.abort_changes();
        assert!(s.pending_pages.is_empty());
        // the page file was not damaged by the discarded image
        assert_eq!(s.get_all_rows("t").unwrap(), vec![row(1, "x"), row(3, "cccccccccc")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn read_wal_records(dir: &str) -> Vec<super::super::wal::WalRecord> {
        let bytes = std::fs::read(std::path::Path::new(dir).join("wal.log")).unwrap();
        super::super::wal::WalReader::new(&bytes)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn append_changes_to_wal_writes_pages_catalog_and_commit_in_order() {
        let (mut s, dir) = setup("wal_append_order");
        s.begin_working_catalog();
        let mut page = s.read_page(0).unwrap();
        page.as_mut_slice()[PAGE_SIZE - 1] = 0xEE;
        s.write_page(0, &page).unwrap();
        s.active_catalog_mut().unwrap().next_page_id += 1;
        let changes = s.build_pending_changes().unwrap();
        assert_eq!(changes.len(), 2);

        let stamped = s.append_changes_to_wal(&changes).unwrap();

        let records = read_wal_records(&dir);
        assert_eq!(records.len(), 3);
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(matches!(records[0].body, WalBody::FullPageWrite { page_id: 0, .. }));
        assert!(matches!(records[1].body, WalBody::CatalogWrite { .. }));
        assert!(matches!(records[2].body, WalBody::Commit));
        assert_eq!(s.wal.next_lsn(), 4);
        assert_eq!(stamped.len(), 1);
        assert_eq!(stamped[0].0, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wal_page_image_is_stamped_with_its_own_lsn_and_matches_the_returned_image() {
        let (mut s, dir) = setup("wal_append_stamp");
        s.begin_working_catalog();
        let mut page = s.read_page(0).unwrap();
        page.as_mut_slice()[PAGE_SIZE - 1] = 0xEE;
        s.write_page(0, &page).unwrap();
        let changes = s.build_pending_changes().unwrap();
        // the PendingChange keeps the unstamped image
        let unstamped = match &changes[0] {
            PendingChange::PageWrite { page_data, .. } => *page_data,
            other => panic!("expected PageWrite, got {:?}", other),
        };
        // it still carries the LSN from the last commit of setup, not the new one
        let lsn_on_disk = PageHeader::from_bytes(&page.as_slice()[..PAGE_HEADER_SIZE]).unwrap().last_applied_lsn;
        assert_ne!(lsn_on_disk, 1);
        assert_eq!({ PageHeader::from_bytes(&unstamped[..PAGE_HEADER_SIZE]).unwrap().last_applied_lsn }, lsn_on_disk);

        let stamped = s.append_changes_to_wal(&changes).unwrap();

        let records = read_wal_records(&dir);
        let (lsn, wal_page) = match &records[0] {
            r => match &r.body {
                WalBody::FullPageWrite { page_data, .. } => (r.lsn, *page_data),
                other => panic!("expected FullPageWrite, got {:?}", other),
            },
        };
        let wal_header = PageHeader::from_bytes(&wal_page[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!({ wal_header.last_applied_lsn }, lsn);
        // the image returned for pages.dat is byte-for-byte the one in the WAL
        assert_eq!(stamped[0].1.as_slice(), &wal_page[..]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn append_changes_to_wal_does_nothing_for_empty_changes() {
        let (mut s, dir) = setup("wal_append_empty");
        s.begin_working_catalog();
        let changes = s.build_pending_changes().unwrap();
        assert!(changes.is_empty());

        assert!(s.append_changes_to_wal(&changes).unwrap().is_empty());
        assert!(read_wal_records(&dir).is_empty());
        assert_eq!(s.wal.next_lsn(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Stage a change to page 0 and to the catalog (next_row_id of table `t`)
    fn stage_page_and_catalog_change(s: &mut PagedStorage) -> u32 {
        s.begin_working_catalog();
        let mut page = s.read_page(0).unwrap();
        page.as_mut_slice()[PAGE_SIZE - 1] = 0xEE;
        s.write_page(0, &page).unwrap();
        let meta = s.active_catalog_mut().unwrap().tables.get_mut("t").unwrap();
        meta.next_row_id += 1;
        meta.next_row_id as u32
    }

    #[test]
    fn commit_changes_makes_wal_pages_and_catalog_consistent() {
        let (mut s, dir) = setup("commit_ok");
        let new_next_row_id = stage_page_and_catalog_change(&mut s);

        s.commit_changes().unwrap();

        // WAL: FullPageWrite(page 0), CatalogWrite, Commit
        let records = read_wal_records(&dir);
        assert_eq!(records.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![1, 2, 3]);
        let wal_page = match &records[0].body {
            WalBody::FullPageWrite { page_id: 0, page_data } => *page_data,
            other => panic!("expected FullPageWrite of page 0, got {:?}", other),
        };
        assert!(matches!(records[1].body, WalBody::CatalogWrite { .. }));
        assert!(matches!(records[2].body, WalBody::Commit));

        // pages.dat holds exactly the bytes of the WAL, including last_applied_lsn
        let on_disk = s.read_page_from_file(0).unwrap();
        assert_eq!(on_disk.as_slice(), &wal_page[..]);
        let header = PageHeader::from_bytes(&on_disk.as_slice()[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!({ header.last_applied_lsn }, records[0].lsn);

        // in-memory state: committed catalog replaced, temporary state dropped
        assert_eq!(s.catalog.tables["t"].next_row_id as u32, new_next_row_id);
        assert!(s.working_catalog.is_none());
        assert!(s.pending_pages.is_empty());

        // catalog.json has the new catalog
        drop(s);
        let reopened = PagedStorage::new(dir.clone()).unwrap();
        assert_eq!(reopened.catalog.tables["t"].next_row_id as u32, new_next_row_id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn commit_changes_without_changes_writes_nothing() {
        let (mut s, dir) = setup("commit_empty");
        let catalog_json_before = std::fs::read(std::path::Path::new(&dir).join("catalog.json")).unwrap();
        s.begin_working_catalog();

        s.commit_changes().unwrap();

        assert!(read_wal_records(&dir).is_empty());
        assert_eq!(s.wal.next_lsn(), 1);
        assert_eq!(
            std::fs::read(std::path::Path::new(&dir).join("catalog.json")).unwrap(),
            catalog_json_before
        );
        assert!(s.working_catalog.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failure_to_write_real_files_is_a_fatal_error_after_the_wal_commit() {
        let (mut s, dir) = setup("commit_fatal");
        stage_page_and_catalog_change(&mut s);
        s.page_file = None; // simulate that pages.dat cannot be written

        let result = s.commit_changes();

        // reported as the fatal error type, not as an ordinary SQL error (String)
        let error: FatalError = result.unwrap_err();
        assert!(error.to_string().contains("Page file not open"));
        // the commit itself is already durable in the WAL
        let records = read_wal_records(&dir);
        assert_eq!(records.len(), 3);
        assert!(matches!(records[2].body, WalBody::Commit));
        // nothing is discarded on failure
        assert_eq!(s.pending_pages.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn abort_changes_discards_only_the_in_memory_state() {
        let (mut s, dir) = setup("abort_changes");
        let committed_next_row_id = s.catalog.tables["t"].next_row_id;
        let page_on_disk = s.read_page_from_file(0).unwrap();
        let wal_before = std::fs::read(std::path::Path::new(&dir).join("wal.log")).unwrap();
        stage_page_and_catalog_change(&mut s);

        s.abort_changes();

        assert!(s.pending_pages.is_empty());
        assert!(s.working_catalog.is_none());
        assert_eq!(s.catalog.tables["t"].next_row_id, committed_next_row_id);
        assert_eq!(s.read_page_from_file(0).unwrap().as_slice(), page_on_disk.as_slice());
        assert_eq!(std::fs::read(std::path::Path::new(&dir).join("wal.log")).unwrap(), wal_before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stamp_last_applied_lsn_only_changes_the_lsn_field() {
        let mut page = [7u8; PAGE_SIZE];
        page[0..PAGE_HEADER_SIZE].copy_from_slice(&PageHeader::new(9).to_bytes());
        let before = page;

        PagedStorage::stamp_last_applied_lsn(&mut page, 42).unwrap();

        assert_eq!(&page[20..28], &42u64.to_le_bytes());
        for i in (0..PAGE_SIZE).filter(|i| !(20..28).contains(i)) {
            assert_eq!(page[i], before[i], "byte {} changed", i);
        }

        // a page without a valid header is rejected
        let mut broken = [0u8; PAGE_SIZE];
        assert!(PagedStorage::stamp_last_applied_lsn(&mut broken, 1).is_err());
    }

    #[test]
    fn new_creates_an_empty_wal_file() {
        let dir = std::env::temp_dir()
            .join(format!("mydb_paged_wal_new_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);

        let s = PagedStorage::new(dir.clone()).unwrap();
        let wal_path = std::path::Path::new(&dir).join("wal.log");
        assert!(wal_path.exists());
        assert_eq!(std::fs::read(&wal_path).unwrap().len(), 0);
        assert_eq!(s.wal.next_lsn(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_fails_when_the_wal_is_corrupt() {
        let dir = std::env::temp_dir()
            .join(format!("mydb_paged_wal_corrupt_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 5 bytes cannot even hold a WAL header
        std::fs::write(std::path::Path::new(&dir).join("wal.log"), [1u8, 2, 3, 4, 5]).unwrap();

        assert!(PagedStorage::new(dir.clone()).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_changes_have_one_page_write_per_page_in_page_id_order() {
        let (mut s, dir) = setup("pending_changes_pages");
        s.begin_working_catalog();
        let mut page = PageBytes::new();
        page.as_mut_slice()[100] = 1;
        s.write_page(5, &page).unwrap();
        s.write_page(2, &page).unwrap();
        page.as_mut_slice()[100] = 2;
        s.write_page(2, &page).unwrap(); // same page again: only the last image counts

        let changes = s.build_pending_changes().unwrap();
        assert_eq!(changes.len(), 2);
        match (&changes[0], &changes[1]) {
            (
                PendingChange::PageWrite { page_id: a, page_data: a_data },
                PendingChange::PageWrite { page_id: b, .. },
            ) => {
                assert_eq!((*a, *b), (2, 5));
                assert_eq!(a_data[100], 2);
            }
            other => panic!("expected two PageWrites, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_changes_include_catalog_write_when_catalog_changed() {
        let (mut s, dir) = setup("pending_changes_catalog");
        let committed_next = s.catalog.next_page_id;

        // an unchanged working catalog (e.g. DELETE) produces no CatalogWrite
        s.begin_working_catalog();
        assert!(s.build_pending_changes().unwrap().is_empty());

        // a changed working catalog produces one CatalogWrite after the PageWrites
        s.active_catalog_mut().unwrap().next_page_id += 3;
        s.write_page(0, &PageBytes::new()).unwrap();
        let changes = s.build_pending_changes().unwrap();
        assert_eq!(changes.len(), 2);
        assert!(matches!(changes[0], PendingChange::PageWrite { page_id: 0, .. }));
        match &changes[1] {
            PendingChange::CatalogWrite { catalog_json } => {
                let restored: Catalog = serde_json::from_slice(catalog_json).unwrap();
                assert_eq!(restored.next_page_id, committed_next + 3);
            }
            other => panic!("expected CatalogWrite, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn building_pending_changes_does_not_modify_state() {
        let (mut s, dir) = setup("pending_changes_readonly");
        s.begin_working_catalog();
        s.active_catalog_mut().unwrap().next_page_id += 1;
        s.write_page(3, &PageBytes::new()).unwrap();

        let _ = s.build_pending_changes().unwrap();
        assert_eq!(s.pending_pages.len(), 1);
        assert!(s.pending_pages.contains_key(&3));
        assert!(s.working_catalog.is_some());
        assert_eq!(s.active_catalog().next_page_id, s.catalog.next_page_id + 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_sql_leaves_committed_catalog_unchanged() {
        let (mut s, dir) = setup("failed_catalog");
        let next_page_id = s.catalog.next_page_id;

        // an SQL that allocates a page and then fails
        s.begin_working_catalog();
        let new_page_id = s.create_new_page().unwrap();
        assert_eq!(new_page_id, next_page_id);
        s.abort_changes();

        assert_eq!(s.catalog.next_page_id, next_page_id);
        assert!(s.working_catalog.is_none());
        assert!(s.pending_pages.is_empty());

        // a following statement still works and gets the same page id
        s.insert_row("t", row(4, "dddddddddd")).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(s.get_all_rows("t").unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn successful_sql_replaces_committed_catalog_and_persists_it() {
        let (mut s, dir) = setup("commit_catalog");
        let before = s.catalog.next_page_id;
        // four rows of ~1KB each do not fit into the first page, so a second page is created
        let big = "x".repeat(1000);
        for i in 10..14 {
            s.insert_row("t", row(i, &big)).unwrap();
            s.commit_changes().unwrap();
        }
        assert!(s.catalog.next_page_id > before);
        assert!(s.working_catalog.is_none());
        let expected = s.get_all_rows("t").unwrap();
        assert_eq!(expected.len(), 7);
        drop(s);

        let reopened = PagedStorage::new(dir.clone()).unwrap();
        assert_eq!(reopened.get_all_rows("t").unwrap(), expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Split the WAL records into transactions; each one ends with its Commit record
    fn wal_transactions(dir: &str) -> Vec<Vec<super::super::wal::WalRecord>> {
        let mut transactions = Vec::new();
        let mut current = Vec::new();
        for record in read_wal_records(dir) {
            let is_commit = matches!(record.body, WalBody::Commit);
            current.push(record);
            if is_commit {
                transactions.push(std::mem::take(&mut current));
            }
        }
        assert!(current.is_empty(), "the WAL ends with records that have no Commit");
        transactions
    }

    fn kinds(transaction: &[super::super::wal::WalRecord]) -> Vec<&'static str> {
        transaction.iter().map(|r| match &r.body {
            WalBody::FullPageWrite { .. } => "page",
            WalBody::CatalogWrite { .. } => "catalog",
            WalBody::Commit => "commit",
        }).collect()
    }

    #[test]
    fn every_statement_is_recorded_in_the_wal_as_one_transaction() {
        let (mut s, dir) = setup_keeping_wal("wal_per_statement");
        // setup ran CREATE TABLE and three INSERTs, each committed on its own
        let tx = wal_transactions(&dir);
        assert_eq!(tx.len(), 4);
        for t in &tx {
            assert_eq!(kinds(t), vec!["page", "catalog", "commit"]);
        }
        let lsns: Vec<u64> = read_wal_records(&dir).iter().map(|r| r.lsn).collect();
        assert_eq!(lsns, (1..=12).collect::<Vec<u64>>());

        // UPDATE that needs no new page, and DELETE: pages only, no CatalogWrite
        s.update_rows("t", &set_name("x"), Some(&id_eq(1))).unwrap();
        s.commit_changes().unwrap();
        s.delete_rows("t", Some(&id_eq(2))).unwrap();
        s.commit_changes().unwrap();
        let tx = wal_transactions(&dir);
        assert_eq!(tx.len(), 6);
        assert_eq!(kinds(&tx[4]), vec!["page", "commit"]);
        assert_eq!(kinds(&tx[5]), vec!["page", "commit"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_multi_page_insert_is_one_transaction_with_several_page_writes() {
        let (mut s, dir) = setup("wal_multi_page");
        let big = "x".repeat(1000);
        for i in 10..14 {
            s.insert_row("t", row(i, &big)).unwrap();
            s.commit_changes().unwrap();
        }

        let tx = wal_transactions(&dir);
        assert_eq!(tx.len(), 4);
        // only the INSERT that did not fit created a page: the old tail page (next_page
        // link) and the new page, in page_id order, then CatalogWrite, then one Commit
        let multi: Vec<_> = tx.iter()
            .filter(|t| kinds(t) == vec!["page", "page", "catalog", "commit"])
            .collect();
        assert_eq!(multi.len(), 1);
        let page_ids: Vec<u32> = multi[0].iter().filter_map(|r| match &r.body {
            WalBody::FullPageWrite { page_id, .. } => Some(*page_id),
            _ => None,
        }).collect();
        assert_eq!(page_ids, vec![0, 1]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn aborting_a_multi_page_update_changes_nothing_and_the_storage_keeps_working() {
        let (mut s, dir) = setup("abort_multi_page_update");
        let big = "x".repeat(1000);
        for i in 10..14 {
            s.insert_row("t", row(i, &big)).unwrap();
            s.commit_changes().unwrap();
        }
        let read_files = |dir: &str| {
            let base = std::path::Path::new(dir);
            (
                std::fs::read(base.join("pages.dat")).unwrap(),
                std::fs::read(base.join("catalog.json")).unwrap(),
                std::fs::read(base.join("wal.log")).unwrap(),
            )
        };
        let files_before = read_files(&dir);
        let catalog_before = (s.catalog.next_page_id, s.catalog.tables["t"].next_row_id, s.catalog.tables["t"].last_page_id);
        let rows_before = s.get_all_rows("t").unwrap();

        // every row grows, so rows move to new pages (this stages several pages)
        s.update_rows("t", &set_name(&"y".repeat(1020)), None).unwrap();
        assert!(!s.pending_pages.is_empty());
        // ... and then the SQL fails before commit_changes
        s.abort_changes();

        assert_eq!(read_files(&dir), files_before);
        let catalog_after = (s.catalog.next_page_id, s.catalog.tables["t"].next_row_id, s.catalog.tables["t"].last_page_id);
        assert_eq!(catalog_after, catalog_before);
        assert_eq!(s.get_all_rows("t").unwrap(), rows_before);

        // right after the failure, the same table accepts INSERT and SELECT
        s.insert_row("t", row(99, "after")).unwrap();
        s.commit_changes().unwrap();
        let rows = s.get_all_rows("t").unwrap();
        assert_eq!(rows.len(), rows_before.len() + 1);
        assert!(rows.contains(&row(99, "after")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_statement_is_refused_until_the_previous_one_is_committed_or_aborted() {
        let (mut s, dir) = setup("begin_guard");
        s.insert_row("t", row(4, "dddddddddd")).unwrap();

        let error = s.insert_row("t", row(5, "eeeeeeeeee")).unwrap_err();
        assert!(error.contains("neither committed nor aborted"));

        // the refused call did not disturb the open statement
        s.commit_changes().unwrap();
        assert_eq!(s.get_all_rows("t").unwrap().len(), 4);

        // after an abort the next statement is accepted again
        s.insert_row("t", row(5, "eeeeeeeeee")).unwrap();
        s.abort_changes();
        s.insert_row("t", row(6, "ffffffffff")).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(s.get_all_rows("t").unwrap().len(), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn working_catalog_is_separate_from_committed_catalog() {
        let (mut s, dir) = setup("working_catalog");
        let committed_next = s.catalog.next_page_id;

        // before begin: no working copy, so modification is refused
        assert!(s.active_catalog_mut().is_err());
        assert_eq!(s.active_catalog().next_page_id, committed_next);

        s.begin_working_catalog();
        s.active_catalog_mut().unwrap().next_page_id += 5;

        // the working copy changes, the committed catalog does not
        assert_eq!(s.active_catalog().next_page_id, committed_next + 5);
        assert_eq!(s.catalog.next_page_id, committed_next);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_page_prefers_pending_pages_over_disk() {
        let (mut s, dir) = setup("pending_read");

        // page 0 exists on disk; a different image in pending_pages must win
        let on_disk = s.read_page(0).unwrap();
        let mut staged = on_disk.clone();
        staged.as_mut_slice()[PAGE_SIZE - 1] = 0xAB;
        assert_ne!(on_disk.as_slice(), staged.as_slice());
        s.pending_pages.insert(0, staged.clone());
        assert_eq!(s.read_page(0).unwrap().as_slice(), staged.as_slice());

        // a page beyond the end of the file is readable through the map
        let beyond = 100;
        assert!(s.read_page(beyond).is_err());
        s.pending_pages.insert(beyond, staged.clone());
        assert_eq!(s.read_page(beyond).unwrap().as_slice(), staged.as_slice());

        // without a pending entry, the disk content is read again
        s.pending_pages.clear();
        assert_eq!(s.read_page(0).unwrap().as_slice(), on_disk.as_slice());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn page_header_roundtrips_with_last_applied_lsn() {
        let mut header = PageHeader::new(7);
        assert_eq!({ header.last_applied_lsn }, 0);
        header.last_applied_lsn = 0x0102_0304_0506_0708;
        let decoded = PageHeader::from_bytes(&header.to_bytes()).unwrap();
        assert_eq!({ decoded.page_id }, 7);
        assert_eq!({ decoded.free_space_offset }, PAGE_HEADER_SIZE as u16);
        assert_eq!({ decoded.last_applied_lsn }, 0x0102_0304_0506_0708);
    }

    #[test]
    fn page_header_size_matches_constant() {
        assert_eq!(std::mem::size_of::<PageHeader>(), PAGE_HEADER_SIZE);
    }

    #[test]
    fn shrinking_update_keeps_following_rows_readable() {
        let (mut s, dir) = setup("shrink_first");
        let n = s.update_rows("t", &set_name("x"), Some(&id_eq(1))).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            s.get_all_rows("t").unwrap(),
            vec![row(1, "x"), row(2, "bbbbbbbbbb"), row(3, "cccccccccc")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shrinking_update_without_filter_updates_every_row() {
        let (mut s, dir) = setup("shrink_all");
        let n = s.update_rows("t", &set_name("x"), None).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(n, 3);
        assert_eq!(s.get_all_rows("t").unwrap(), vec![row(1, "x"), row(2, "x"), row(3, "x")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_after_shrinking_update_finds_following_rows() {
        let (mut s, dir) = setup("shrink_delete");
        s.update_rows("t", &set_name("x"), Some(&id_eq(1))).unwrap();
        s.commit_changes().unwrap();
        let n = s.delete_rows("t", Some(&id_eq(3))).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.get_all_rows("t").unwrap(), vec![row(1, "x"), row(2, "bbbbbbbbbb")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shrunk_row_can_be_updated_again_to_original_length() {
        let (mut s, dir) = setup("shrink_regrow");
        s.update_rows("t", &set_name("x"), Some(&id_eq(2))).unwrap();
        s.commit_changes().unwrap();
        s.update_rows("t", &set_name("yyyyyyyyyy"), Some(&id_eq(2))).unwrap();
        s.commit_changes().unwrap();
        assert_eq!(
            s.get_all_rows("t").unwrap(),
            vec![row(1, "aaaaaaaaaa"), row(2, "yyyyyyyyyy"), row(3, "cccccccccc")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shrinking_update_is_visible_after_reopen() {
        let (mut s, dir) = setup("shrink_reopen");
        s.update_rows("t", &set_name("x"), Some(&id_eq(1))).unwrap();
        s.commit_changes().unwrap();
        drop(s);
        let reopened = PagedStorage::new(dir.clone()).unwrap();
        assert_eq!(
            reopened.get_all_rows("t").unwrap(),
            vec![row(1, "x"), row(2, "bbbbbbbbbb"), row(3, "cccccccccc")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Recovery with a real storage (the other Recovery tests are in recovery.rs) ----

    /// A FullPageWrite whose page is filled with `fill`
    fn filled_page_record(lsn: u64, page_id: u32, fill: u8) -> WalRecord {
        WalRecord {
            lsn,
            body: WalBody::FullPageWrite { page_id, page_data: [fill; PAGE_SIZE] },
        }
    }

    fn page_in(file: &[u8], page_id: usize) -> &[u8] {
        &file[page_id * PAGE_SIZE..(page_id + 1) * PAGE_SIZE]
    }

    /// Stop a real storage after some committed work and return (dir, pages.dat bytes).
    /// The directory holds the WAL and pages.dat the storage produced.
    fn crashed_storage(test_name: &str) -> (String, Vec<u8>) {
        let (s, dir) = setup_keeping_wal(test_name);
        drop(s);
        let pages = std::fs::read(std::path::Path::new(&dir).join("pages.dat")).unwrap();
        (dir, pages)
    }

    #[test]
    fn recover_restores_pages_from_the_wal_when_pages_dat_is_missing() {
        let (dir, original_pages) = crashed_storage("restore_missing");
        let pages_path = std::path::Path::new(&dir).join("pages.dat");
        std::fs::remove_file(&pages_path).unwrap();

        recovery::recover(Path::new(&dir)).unwrap();

        let restored = std::fs::read(&pages_path).unwrap();
        assert_eq!(restored, original_pages);
        // last_applied_lsn equals the LSN of the page's last FullPageWrite in the WAL
        let last_lsn = read_wal_records(&dir)
            .iter()
            .filter(|r| matches!(r.body, WalBody::FullPageWrite { page_id: 0, .. }))
            .map(|r| r.lsn)
            .max()
            .unwrap();
        let header = PageHeader::from_bytes(&page_in(&restored, 0)[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!({ header.last_applied_lsn }, last_lsn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_repairs_a_torn_page_from_the_wal() {
        let (dir, original_pages) = crashed_storage("restore_torn");
        let pages_path = std::path::Path::new(&dir).join("pages.dat");
        // the first half of the page is the new image, the second half is garbage
        let mut torn = original_pages.clone();
        torn[PAGE_SIZE / 2..PAGE_SIZE].fill(0xFF);
        std::fs::write(&pages_path, &torn).unwrap();

        recovery::recover(Path::new(&dir)).unwrap();

        assert_eq!(std::fs::read(&pages_path).unwrap(), original_pages);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_replaces_a_broken_catalog_json_with_the_one_in_the_wal() {
        let (dir, _) = crashed_storage("catalog_broken");
        let catalog_path = std::path::Path::new(&dir).join("catalog.json");
        let original_catalog = std::fs::read(&catalog_path).unwrap();
        std::fs::write(&catalog_path, b"{\"tables\": {\"t\": ").unwrap(); // torn write

        recovery::recover(Path::new(&dir)).unwrap();

        assert_eq!(std::fs::read(&catalog_path).unwrap(), original_catalog);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recover_restores_both_files_when_both_are_missing() {
        let (dir, original_pages) = crashed_storage("catalog_and_pages_missing");
        let dir_path = std::path::Path::new(&dir);
        let original_catalog = std::fs::read(dir_path.join("catalog.json")).unwrap();
        std::fs::remove_file(dir_path.join("catalog.json")).unwrap();
        std::fs::remove_file(dir_path.join("pages.dat")).unwrap();

        recovery::recover(dir_path).unwrap();

        assert_eq!(std::fs::read(dir_path.join("catalog.json")).unwrap(), original_catalog);
        assert_eq!(std::fs::read(dir_path.join("pages.dat")).unwrap(), original_pages);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Startup: PagedStorage::new runs Recovery before loading catalog.json ----
    //
    // A crash is reproduced by dropping the storage and then rewriting the files.

    /// The bytes of (wal.log, pages.dat, catalog.json); a missing file is `None`
    type Files = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

    fn start_files(dir: &str) -> Files {
        let read = |name: &str| std::fs::read(std::path::Path::new(dir).join(name)).ok();
        (read("wal.log"), read("pages.dat"), read("catalog.json"))
    }

    fn start_write(dir: &str, name: &str, bytes: &[u8]) {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }

    /// Open the storage, assert that it fails and that no file changed
    fn assert_new_fails_and_changes_nothing(dir: &str) {
        let before = start_files(dir);
        assert!(PagedStorage::new(dir.to_string()).is_err());
        assert_eq!(start_files(dir), before);
    }

    fn start_rows(dir: &str) -> Vec<Row> {
        PagedStorage::new(dir.to_string()).unwrap().get_all_rows("t").unwrap()
    }

    fn three_rows_and(more: &[(i64, &str)]) -> Vec<Row> {
        let mut rows = vec![row(1, "aaaaaaaaaa"), row(2, "bbbbbbbbbb"), row(3, "cccccccccc")];
        rows.extend(more.iter().map(|(id, name)| row(*id, name)));
        rows
    }

    fn insert_and_commit(s: &mut PagedStorage, id: i64, name: &str) {
        s.insert_row("t", vec![Value::Int(id), Value::Text(name.into())]).unwrap();
        s.commit_changes().unwrap();
    }

    #[test]
    fn startup_recovers_when_only_the_wal_is_up_to_date() {
        let (mut s, dir) = setup("start_wal_only");
        let (_, old_pages, old_catalog) = start_files(&dir);
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        // crash after the Commit was made durable, before pages.dat and catalog.json were written
        start_write(&dir, "pages.dat", &old_pages.unwrap());
        start_write(&dir, "catalog.json", &old_catalog.unwrap());

        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd")]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_recovers_a_new_page_when_pages_dat_is_short() {
        let (mut s, dir) = setup("start_new_page");
        let long = |id: i64| format!("{:0>900}", id);
        let mut expected = three_rows_and(&[]);
        let mut id = 4;
        // insert until an INSERT creates a new page, keeping the files from just before it
        let (before_new_page, after_new_page) = loop {
            let before = start_files(&dir);
            insert_and_commit(&mut s, id, &long(id));
            expected.push(row(id, &long(id)));
            let after = start_files(&dir);
            if after.1.as_ref().unwrap().len() > before.1.as_ref().unwrap().len() {
                break (before, after);
            }
            id += 1;
            assert!(id < 30, "no new page was created");
        };
        drop(s);
        start_write(&dir, "pages.dat", &before_new_page.1.unwrap());
        start_write(&dir, "catalog.json", &before_new_page.2.unwrap());

        assert_eq!(start_rows(&dir), expected);
        // the file was extended again by Recovery
        assert_eq!(start_files(&dir).1, after_new_page.1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_repairs_a_torn_page() {
        let (mut s, dir) = setup("start_torn");
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        let (_, pages, _) = start_files(&dir);
        let pages = pages.unwrap();
        let mut torn = pages.clone();
        torn[PAGE_SIZE / 2..PAGE_SIZE].fill(0xFF);
        start_write(&dir, "pages.dat", &torn);

        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd")]));

        // the restored page is the WAL's image: same bytes, last_applied_lsn = its LSN
        let restored = start_files(&dir).1.unwrap();
        assert_eq!(restored, pages);
        let wal_lsn = read_wal_records(&dir)
            .iter()
            .filter(|r| matches!(r.body, WalBody::FullPageWrite { page_id: 0, .. }))
            .map(|r| r.lsn)
            .max()
            .unwrap();
        let header = PageHeader::from_bytes(&restored[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!({ header.last_applied_lsn }, wal_lsn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_recovers_a_broken_catalog_json_from_the_wal() {
        let (mut s, dir) = setup("start_catalog_broken");
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        let original_catalog = start_files(&dir).2.unwrap();
        start_write(&dir, "catalog.json", b"{\"tables\": {\"t\": ");

        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd")]));
        assert_eq!(start_files(&dir).2.unwrap(), original_catalog);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_fails_when_catalog_json_is_broken_and_the_wal_has_no_catalog_write() {
        let (s, dir) = setup("start_catalog_lost"); // the WAL is empty
        drop(s);
        start_write(&dir, "catalog.json", b"{\"tables\": {\"t\": ");

        let error = PagedStorage::new(dir.clone()).err().expect("startup must fail");
        assert!(error.contains("catalog.json"), "unexpected error: {}", error);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_with_an_empty_wal_changes_neither_pages_dat_nor_catalog_json() {
        let (s, dir) = setup("start_empty_wal");
        drop(s);
        let before = start_files(&dir);

        assert_eq!(start_rows(&dir), three_rows_and(&[]));

        let after = start_files(&dir);
        assert_eq!(after.1, before.1);
        assert_eq!(after.2, before.2);
        assert_eq!(after.0.unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_recreates_a_missing_pages_dat_from_the_wal() {
        let (mut s, dir) = setup("start_pages_missing");
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        std::fs::remove_file(std::path::Path::new(&dir).join("pages.dat")).unwrap();

        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd")]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_cuts_an_uncommitted_tail_and_never_applies_it_later() {
        let (mut s, dir) = setup("start_uncommitted_tail");
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        // a transaction that wrote its records but crashed before its Commit record:
        // if it were applied it would wipe page 0
        let committed_wal = start_files(&dir).0.unwrap();
        let next_lsn = read_wal_records(&dir).last().unwrap().lsn + 1;
        let mut with_tail = committed_wal.clone();
        with_tail.extend(filled_page_record(next_lsn, 0, 0xEE).to_bytes());
        start_write(&dir, "wal.log", &with_tail);

        // startup: the tail is not applied and the WAL ends at the last Commit
        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd")]));
        assert_eq!(start_files(&dir).0.unwrap(), committed_wal);

        // a new transaction is committed after the cut, then the next start
        let mut s = PagedStorage::new(dir.clone()).unwrap();
        assert_eq!(s.wal.next_lsn(), next_lsn); // resumes after the last record that is left
        insert_and_commit(&mut s, 5, "eeeeeeeeee");
        drop(s);
        assert_eq!(start_rows(&dir), three_rows_and(&[(4, "dddddddddd"), (5, "eeeeeeeeee")]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_fails_on_a_broken_wal_and_changes_nothing() {
        // a record cut off at the end
        let (mut s, dir) = setup("start_wal_cut");
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        let mut wal = start_files(&dir).0.unwrap();
        wal.extend_from_slice(&filled_page_record(99, 1, 0xEE).to_bytes()[..100]);
        start_write(&dir, "wal.log", &wal);
        assert_new_fails_and_changes_nothing(&dir);
        let _ = std::fs::remove_dir_all(dir);

        // a checksum mismatch in the middle (a valid record follows it)
        let (mut s, dir) = setup("start_wal_checksum");
        insert_and_commit(&mut s, 4, "dddddddddd");
        insert_and_commit(&mut s, 5, "eeeeeeeeee");
        drop(s);
        let mut wal = start_files(&dir).0.unwrap();
        wal[200] ^= 0xFF; // inside the page data of the first record
        start_write(&dir, "wal.log", &wal);
        assert_new_fails_and_changes_nothing(&dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn starting_twice_after_a_crash_gives_the_same_files() {
        let (mut s, dir) = setup("start_twice");
        let (_, old_pages, old_catalog) = start_files(&dir);
        insert_and_commit(&mut s, 4, "dddddddddd");
        drop(s);
        start_write(&dir, "pages.dat", &old_pages.unwrap());
        start_write(&dir, "catalog.json", &old_catalog.unwrap());

        drop(PagedStorage::new(dir.clone()).unwrap());
        let after_first = start_files(&dir);
        drop(PagedStorage::new(dir.clone()).unwrap());

        assert_eq!(start_files(&dir), after_first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_recovers_after_a_fatal_commit_error() {
        let (mut s, dir) = setup("start_after_fatal");
        let new_next_row_id = stage_page_and_catalog_change(&mut s);
        s.page_file = None; // pages.dat cannot be written: the commit fails after the WAL commit
        assert!(s.commit_changes().is_err());
        drop(s);

        let mut reopened = PagedStorage::new(dir.clone()).unwrap();

        assert_eq!(reopened.read_page_from_file(0).unwrap().as_slice()[PAGE_SIZE - 1], 0xEE);
        assert_eq!(reopened.catalog.tables["t"].next_row_id as u32, new_next_row_id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
