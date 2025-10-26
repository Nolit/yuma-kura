// src/storage/csv.rs
// CSV file storage with .idx (index) and .del (deleted records) support

use crate::ast::{ColumnDef, Assignment, Predicate};
use crate::engine::value::Value;
use super::{Storage, Row};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write, Seek, SeekFrom};
use std::path::PathBuf;

/// CSV storage implementation with index and deletion support
/// 
/// File structure:
/// - data.csv: Main CSV data file
/// - data.csv.idx: Index file with row offsets
/// - data.csv.del: Deleted row IDs (for future vacuum)
pub struct CsvStorage {
    base_path: PathBuf,
    data_file: Option<File>,
    index_file: Option<File>,
    deleted_file: Option<File>,
    
    // In-memory caches for performance
    row_offsets: HashMap<usize, u64>, // row_id -> file_offset
    deleted_rows: std::collections::HashSet<usize>, // deleted row IDs
    
    // Table metadata
    tables: HashMap<String, CsvTableInfo>,
}

/// Metadata for each table in CSV storage
#[derive(Debug)]
struct CsvTableInfo {
    schema: Vec<ColumnDef>,
    row_count: usize,
    next_row_id: usize,
}

impl CsvStorage {
    pub fn new(file_path: String) -> Self {
        let base_path = PathBuf::from(file_path);
        Self {
            base_path,
            data_file: None,
            index_file: None,
            deleted_file: None,
            row_offsets: HashMap::new(),
            deleted_rows: std::collections::HashSet::new(),
            tables: HashMap::new(),
        }
    }

    /// Load index file (.idx) to populate row_offsets
    fn load_index(&mut self, table_name: &str) -> Result<(), String> {
        let idx_path = self.get_index_path(table_name);
        if !idx_path.exists() {
            return Ok(()); // No index file yet
        }

        let file = File::open(&idx_path)
            .map_err(|e| format!("Failed to open index file: {}", e))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line.map_err(|e| format!("Failed to read index line: {}", e))?;
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() == 2 {
                let row_id: usize = parts[0].parse()
                    .map_err(|e| format!("Invalid row ID in index: {}", e))?;
                let offset: u64 = parts[1].parse()
                    .map_err(|e| format!("Invalid offset in index: {}", e))?;
                self.row_offsets.insert(row_id, offset);
            }
        }

        Ok(())
    }

    /// Load deleted records file (.del) to populate deleted_rows
    fn load_deleted(&mut self, table_name: &str) -> Result<(), String> {
        let del_path = self.get_deleted_path(table_name);
        if !del_path.exists() {
            return Ok(()); // No deleted file yet
        }

        let file = File::open(&del_path)
            .map_err(|e| format!("Failed to open deleted file: {}", e))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line.map_err(|e| format!("Failed to read deleted line: {}", e))?;
            if let Ok(row_id) = line.parse::<usize>() {
                self.deleted_rows.insert(row_id);
            }
        }

        Ok(())
    }

    /// Save index file (.idx) with current row_offsets
    fn save_index(&mut self, table_name: &str) -> Result<(), String> {
        let idx_path = self.get_index_path(table_name);
        let mut file = File::create(&idx_path)
            .map_err(|e| format!("Failed to create index file: {}", e))?;

        for (row_id, offset) in &self.row_offsets {
            writeln!(file, "{},{}", row_id, offset)
                .map_err(|e| format!("Failed to write index: {}", e))?;
        }

        Ok(())
    }

    /// Save deleted records file (.del) with current deleted_rows
    fn save_deleted(&mut self, table_name: &str) -> Result<(), String> {
        let del_path = self.get_deleted_path(table_name);
        let mut file = File::create(&del_path)
            .map_err(|e| format!("Failed to create deleted file: {}", e))?;

        for row_id in &self.deleted_rows {
            writeln!(file, "{}", row_id)
                .map_err(|e| format!("Failed to write deleted record: {}", e))?;
        }

        Ok(())
    }

    /// Get random access to a specific row by ID
    fn get_row_by_id(&mut self, table_name: &str, row_id: usize) -> Result<Option<Row>, String> {
        // Check if row is deleted
        if self.deleted_rows.contains(&row_id) {
            return Ok(None);
        }

        // Get offset from index
        let offset = self.row_offsets.get(&row_id)
            .ok_or_else(|| format!("Row {} not found in index", row_id))?;

        // Open data file and seek to offset
        let data_path = self.get_data_path(table_name);
        let mut file = File::open(&data_path)
            .map_err(|e| format!("Failed to open data file: {}", e))?;
        
        file.seek(SeekFrom::Start(*offset))
            .map_err(|e| format!("Failed to seek to offset: {}", e))?;

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line)
            .map_err(|e| format!("Failed to read line: {}", e))?;

        // Parse CSV line into Row (simplified - would need proper CSV parsing)
        let values: Vec<Value> = line.trim().split(',')
            .map(|s| Value::Text(s.to_string())) // Simplified parsing
            .collect();

        Ok(Some(values))
    }

    /// Helper methods for file paths
    fn get_data_path(&self, table_name: &str) -> PathBuf {
        self.base_path.join(format!("{}.csv", table_name))
    }

    fn get_index_path(&self, table_name: &str) -> PathBuf {
        self.base_path.join(format!("{}.csv.idx", table_name))
    }

    fn get_deleted_path(&self, table_name: &str) -> PathBuf {
        self.base_path.join(format!("{}.csv.del", table_name))
    }
}

impl Storage for CsvStorage {
    fn create_table(&mut self, _name: &str, _schema: Vec<ColumnDef>) -> Result<(), String> {
        // TODO: Implement CSV table creation
        // 1. Create empty CSV file with headers
        // 2. Initialize empty index and deleted files
        // 3. Store table metadata
        Err("CSV storage not yet implemented".to_string())
    }

    fn insert_row(&mut self, _table: &str, _row: Row) -> Result<(), String> {
        // TODO: Implement CSV row insertion
        // 1. Append row to CSV file
        // 2. Update index file with new offset
        // 3. Update table metadata
        Err("CSV storage not yet implemented".to_string())
    }

    fn schema(&self, _table: &str) -> Result<&[ColumnDef], String> {
        // TODO: Return schema from table metadata
        Err("CSV storage not yet implemented".to_string())
    }

    fn rows<'a>(&'a self, _table: &str) -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String> {
        // TODO: Return iterator over CSV rows (excluding deleted)
        // This would need to be implemented differently for CSV since we can't return references
        // to data that's not in memory
        Err("CSV storage not yet implemented".to_string())
    }

    fn update_rows(&mut self, _table: &str, _assignments: &[Assignment], _filter: Option<&Predicate>) -> Result<usize, String> {
        // TODO: Implement CSV row updates
        // 1. Read all rows from CSV file
        // 2. Apply updates to matching rows
        // 3. Write updated rows back to CSV file
        // 4. Update index file if needed
        Err("CSV storage not yet implemented".to_string())
    }
}
