// src/storage/paged.rs
// 4KB fixed-page storage implementation
// Page data is strictly 4KB, metadata is separated into catalog.json

use crate::ast::{ColumnDef, Assignment, Predicate, CmpOp};
use crate::engine::value::Value;
use super::{Storage, Row};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::PathBuf;
use serde::{Serialize, Deserialize};

/// Page size: 4KB (4096 bytes)
const PAGE_SIZE: usize = 4096;
/// Maximum size of a single data item: 1KB
const MAX_DATA_SIZE: usize = 1024;
/// Page header size
const PAGE_HEADER_SIZE: usize = 20;

/// Page-aligned memory structure
#[repr(align(4096))]
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

/// Page header (first 20 bytes of a page)
#[repr(C, packed)]
struct PageHeader {
    magic: u32,           // Magic number: 0x50414745 ("PAGE")
    page_id: u32,         // Page ID (global)
    free_space_offset: u16, // Start offset of free space (from data section)
    row_count: u16,       // Number of rows stored in this page
    next_page: u32,       // Next page ID (for overflow pages, 0=none)
    flags: u16,           // Flags (for future use)
    _reserved: u16,       // Reserved (to align to 20 bytes)
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
    const SIZE: usize = 16; // row_id(4) + data_size(2) + deleted(1) + overflow(1) + next_page(4) + toast_id(8)
    
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
#[derive(Debug, Serialize, Deserialize)]
struct TableMetadata {
    schema: Vec<ColumnDef>,
    next_row_id: usize,
    first_page_id: u32,   // First page ID of this table
    last_page_id: u32,     // Last page ID of this table
}

/// Catalog information (stored in catalog.json)
#[derive(Debug, Serialize, Deserialize)]
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
}

impl PagedStorage {
    pub fn new(file_path: String) -> Result<Self, String> {
        let base_path = PathBuf::from(file_path);
        
        // Create directory
        std::fs::create_dir_all(&base_path)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
        
        // Load catalog
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
        
        Ok(Self {
            base_path,
            catalog,
            page_file: Some(page_file),
        })
    }
    
    /// Save catalog
    fn save_catalog(&self) -> Result<(), String> {
        let catalog_path = self.base_path.join("catalog.json");
        let content = serde_json::to_string_pretty(&self.catalog)
            .map_err(|e| format!("Failed to serialize catalog: {}", e))?;
        
        std::fs::write(&catalog_path, content)
            .map_err(|e| format!("Failed to write catalog.json: {}", e))?;
        
        Ok(())
    }
    
    /// Read a page from file
    fn read_page(&mut self, page_id: u32) -> Result<Box<PageBytes>, String> {
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
    
    /// Write a page to file
    fn write_page(&mut self, page_id: u32, page: &PageBytes) -> Result<(), String> {
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
        let page_id = self.catalog.next_page_id;
        self.catalog.next_page_id += 1;
        
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
        let table_meta = self.catalog.tables.get(table_name)
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
                            // Row size stayed the same or decreased: update in place
                            page.as_mut_slice()[row_data_start..row_data_start + new_row_data.len()]
                                .copy_from_slice(&new_row_data);
                            // Update row header with new data size
                            row_header.data_size = new_row_data.len() as u16;
                            page.as_mut_slice()[data_offset..data_offset + RowHeader::SIZE]
                                .copy_from_slice(&row_header.to_bytes());
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
                                
                                // Update table metadata to track new page
                                if let Some(table_meta) = self.catalog.tables.get_mut(table_name) {
                                    if new_page_id > table_meta.last_page_id {
                                        table_meta.last_page_id = new_page_id;
                                    }
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

impl Storage for PagedStorage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String> {
        let table_name = name.to_lowercase();
        
        if self.catalog.tables.contains_key(&table_name) {
            return Err("table exists".to_string());
        }
        
        // Create a new page
        let first_page_id = self.create_new_page()?;
        
        // Register table metadata
        self.catalog.tables.insert(table_name.clone(), TableMetadata {
            schema: schema.clone(),
            next_row_id: 1,
            first_page_id,
            last_page_id: first_page_id,
        });
        
        self.save_catalog()?;
        
        Ok(())
    }
    
    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String> {
        let table_name = table.to_lowercase();
        
        let (schema, row_id) = {
            let table_meta = self.catalog.tables.get(&table_name)
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
                let new_page_id = self.create_new_page()?;
                
                // Update metadata
                if let Some(table_meta) = self.catalog.tables.get_mut(&table_name) {
                    table_meta.last_page_id = new_page_id;
                }
                
                new_page_id
            }
        };
        
        // Add row to page
        self.add_row_to_page(target_page_id, row_id, &row_data)?;
        
        // Update metadata
        if let Some(table_meta) = self.catalog.tables.get_mut(&table_name) {
            table_meta.next_row_id += 1;
        }
        
        self.save_catalog()?;
        
        Ok(())
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
    
    fn update_rows(&mut self, table: &str, assignments: &[Assignment], filter: Option<&Predicate>) -> Result<usize, String> {
        let table_name = table.to_lowercase();
        let (schema, first_page_id, last_page_id) = {
            let table_meta = self.catalog.tables.get(&table_name)
                .ok_or("table not found")?;
            (table_meta.schema.clone(), table_meta.first_page_id, table_meta.last_page_id)
        };
        
        // TODO: Traverse page chain
        // Note: New pages created during update already contain updated rows,
        // so we don't need to process them again
        let mut total_updated = 0;
        for page_id in first_page_id..=last_page_id {
            let updated = self.update_rows_in_page(&table_name, page_id, &schema, Some(assignments), filter)?;
            total_updated += updated;
        }
        
        // Save catalog if new pages were created (metadata updated)
        self.save_catalog()?;
        
        Ok(total_updated)
    }
    
    fn delete_rows(&mut self, table: &str, filter: Option<&Predicate>) -> Result<usize, String> {
        let table_name = table.to_lowercase();
        let (schema, first_page_id, last_page_id) = {
            let table_meta = self.catalog.tables.get(&table_name)
                .ok_or("table not found")?;
            (table_meta.schema.clone(), table_meta.first_page_id, table_meta.last_page_id)
        };
        
        // TODO: Traverse page chain
        let mut total_deleted = 0;
        for page_id in first_page_id..=last_page_id {
            let deleted = self.update_rows_in_page(&table_name, page_id, &schema, None, filter)?;
            total_deleted += deleted;
        }
        
        Ok(total_deleted)
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
        
        while current_page_id <= table_meta.last_page_id {
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
