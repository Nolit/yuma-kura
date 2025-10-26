// src/storage/csv.rs
// CSV file storage with .idx (index) and .del (deleted records) support

use crate::ast::{ColumnDef, Assignment, Predicate};
use crate::engine::value::Value;
use super::{Storage, Row};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write, Seek, SeekFrom};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// CSV storage implementation with index and deletion support
/// 
/// File structure:
/// - data.csv: Main CSV data file
/// - data.csv.idx: Index file with row offsets
/// - data.csv.del: Deleted row IDs (for future vacuum)
/// - catalog.json: Table schema definitions
pub struct CsvStorage {
    base_path: PathBuf,
    
    // In-memory caches for performance
    row_offsets: HashMap<usize, u64>, // row_id -> file_offset
    deleted_rows: std::collections::HashSet<usize>, // deleted row IDs
    
    // Table metadata
    tables: HashMap<String, CsvTableInfo>,
}

/// Serializable table metadata for catalog.json
#[derive(Debug, Serialize, Deserialize)]
struct TableMetadata {
    schema: Vec<ColumnDef>,
    next_row_id: usize,
}

/// Metadata for each table in CSV storage
#[derive(Debug)]
struct CsvTableInfo {
    schema: Vec<ColumnDef>,
    next_row_id: usize,
}

impl CsvStorage {
    pub fn new(file_path: String) -> Result<Self, String> {
        let base_path = PathBuf::from(file_path);
        
        // Create directory if it doesn't exist
        std::fs::create_dir_all(&base_path)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
        
        let mut storage = Self {
            base_path,
            row_offsets: HashMap::new(),
            deleted_rows: std::collections::HashSet::new(),
            tables: HashMap::new(),
        };
        
        // Load existing catalog (create empty if doesn't exist)
        storage.load_catalog()?;
        
        Ok(storage)
    }

    /// Load catalog.json and initialize table metadata
    fn load_catalog(&mut self) -> Result<(), String> {
        let catalog_path = self.base_path.join("catalog.json");
        if !catalog_path.exists() {
            // Create empty catalog file
            self.save_catalog()?;
            return Ok(());
        }

        let content = std::fs::read_to_string(&catalog_path)
            .map_err(|e| format!("Failed to read catalog.json: {}", e))?;
        
        let catalog: HashMap<String, TableMetadata> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse catalog.json: {}", e))?;

        for (table_name, metadata) in catalog {
            self.tables.insert(table_name.clone(), CsvTableInfo {
                schema: metadata.schema,
                next_row_id: metadata.next_row_id,
            });
        }

        Ok(())
    }

    /// Save catalog.json with current table metadata
    fn save_catalog(&self) -> Result<(), String> {
        let catalog_path = self.base_path.join("catalog.json");
        let mut catalog = HashMap::new();

        for (table_name, table_info) in &self.tables {
            catalog.insert(table_name.clone(), TableMetadata {
                schema: table_info.schema.clone(),
                next_row_id: table_info.next_row_id,
            });
        }

        let content = serde_json::to_string_pretty(&catalog)
            .map_err(|e| format!("Failed to serialize catalog: {}", e))?;
        
        std::fs::write(&catalog_path, content)
            .map_err(|e| format!("Failed to write catalog.json: {}", e))?;

        Ok(())
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

    /// Append new index entry to .idx file
    fn append_index(&mut self, table_name: &str, row_id: usize, offset: u64) -> Result<(), String> {
        let idx_path = self.get_index_path(table_name);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&idx_path)
            .map_err(|e| format!("Failed to open index file for append: {}", e))?;

        writeln!(file, "{},{}", row_id, offset)
            .map_err(|e| format!("Failed to append to index: {}", e))?;

        // Update in-memory cache
        self.row_offsets.insert(row_id, offset);
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

    /// Append deleted row ID to .del file
    fn append_deleted(&mut self, table_name: &str, row_id: usize) -> Result<(), String> {
        let del_path = self.get_deleted_path(table_name);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&del_path)
            .map_err(|e| format!("Failed to open deleted file for append: {}", e))?;

        writeln!(file, "{}", row_id)
            .map_err(|e| format!("Failed to append to deleted file: {}", e))?;

        // Update in-memory cache
        self.deleted_rows.insert(row_id);
        Ok(())
    }

    /// Convert Value to CSV string representation
    fn value_to_csv_string(value: &Value) -> String {
        match value {
            Value::Int(i) => i.to_string(),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "NULL".to_string(),
        }
    }

    /// Convert CSV string to Value based on column type
    fn csv_string_to_value(s: &str, column_type: &crate::ast::ColumnType) -> Result<Value, String> {
        match column_type {
            crate::ast::ColumnType::Int => s.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| format!("Invalid integer: {}", s)),
            crate::ast::ColumnType::Text => Ok(Value::Text(s.to_string())),
            crate::ast::ColumnType::Bool => match s.to_ascii_lowercase().as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(format!("Invalid boolean: {}", s)),
            },
        }
    }

    /// Convert Row to CSV line
    fn row_to_csv_line(row: &Row) -> String {
        row.iter()
            .map(Self::value_to_csv_string)
            .collect::<Vec<String>>()
            .join(",")
    }

    /// Parse CSV line into Row based on schema
    fn csv_line_to_row(line: &str, schema: &[ColumnDef]) -> Result<Row, String> {
        let parts: Vec<&str> = line.trim().split(',').collect();
        if parts.len() != schema.len() {
            return Err(format!("Column count mismatch: expected {}, got {}", schema.len(), parts.len()));
        }

        let mut row = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let value = Self::csv_string_to_value(part, &schema[i].data_type)?;
            row.push(value);
        }
        Ok(row)
    }

    /// Append row to CSV file and return the offset
    fn append_row_to_csv(&mut self, table_name: &str, row: &Row) -> Result<u64, String> {
        let data_path = self.get_data_path(table_name);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&data_path)
            .map_err(|e| format!("Failed to open CSV file for append: {}", e))?;

        // Get current file size (this will be the offset for the new row)
        let offset = file.seek(SeekFrom::End(0))
            .map_err(|e| format!("Failed to get file size: {}", e))?;

        let csv_line = Self::row_to_csv_line(row);
        writeln!(file, "{}", csv_line)
            .map_err(|e| format!("Failed to write CSV line: {}", e))?;

        Ok(offset)
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

    /// Get all rows from a table (for SELECT queries)
    fn get_all_rows_internal(&self, table_name: &str) -> Result<Vec<Row>, String> {
        let table_info = self.tables.get(table_name)
            .ok_or("table not found")?;

        // Load index and deleted files for this table
        let mut row_offsets = HashMap::new();
        let mut deleted_rows = std::collections::HashSet::new();
        
        // Load index
        let idx_path = self.get_index_path(table_name);
        if idx_path.exists() {
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
                    row_offsets.insert(row_id, offset);
                }
            }
        }

        // Load deleted rows
        let del_path = self.get_deleted_path(table_name);
        if del_path.exists() {
            let file = File::open(&del_path)
                .map_err(|e| format!("Failed to open deleted file: {}", e))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("Failed to read deleted line: {}", e))?;
                if let Ok(row_id) = line.parse::<usize>() {
                    deleted_rows.insert(row_id);
                }
            }
        }

        // Use random access with index file for efficient reading
        let data_path = self.get_data_path(table_name);
        let mut all_rows = Vec::new();

        // Get all non-deleted row IDs and sort them by offset for efficient reading
        let mut valid_row_ids: Vec<usize> = row_offsets.keys()
            .filter(|&&row_id| !deleted_rows.contains(&row_id))
            .copied()
            .collect();
        
        // Sort by offset to read file sequentially (better for performance)
        valid_row_ids.sort_by_key(|&row_id| row_offsets[&row_id]);

        // Read rows using random access
        for row_id in valid_row_ids {
            let offset = row_offsets[&row_id];
            let row = self.read_row_at_offset(&data_path, offset, &table_info.schema)?;
            all_rows.push(row);
        }

        Ok(all_rows)
    }

    /// Read a single row at a specific offset using random access
    fn read_row_at_offset(&self, data_path: &PathBuf, offset: u64, schema: &[ColumnDef]) -> Result<Row, String> {
        let mut file = File::open(data_path)
            .map_err(|e| format!("Failed to open CSV file: {}", e))?;
        
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to offset: {}", e))?;

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line)
            .map_err(|e| format!("Failed to read line: {}", e))?;

        Self::csv_line_to_row(&line, schema)
    }
}

/// Streaming iterator for CSV rows using random access
pub struct CsvStreamingIterator {
    data_path: PathBuf,
    valid_row_ids: Vec<usize>,
    row_offsets: HashMap<usize, u64>,
    schema: Vec<ColumnDef>,
    current_index: usize,
}

impl CsvStreamingIterator {
    fn new(data_path: PathBuf, valid_row_ids: Vec<usize>, row_offsets: HashMap<usize, u64>, schema: Vec<ColumnDef>) -> Result<Self, String> {
        Ok(Self {
            data_path,
            valid_row_ids,
            row_offsets,
            schema,
            current_index: 0,
        })
    }
}

impl Iterator for CsvStreamingIterator {
    type Item = Result<Row, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_index >= self.valid_row_ids.len() {
            return None;
        }

        let row_id = self.valid_row_ids[self.current_index];
        let offset = self.row_offsets[&row_id];
        
        // Read row at specific offset
        let result = self.read_row_at_offset(offset);
        self.current_index += 1;
        
        Some(result)
    }
}

impl CsvStreamingIterator {
    fn read_row_at_offset(&self, offset: u64) -> Result<Row, String> {
        let mut file = File::open(&self.data_path)
            .map_err(|e| format!("Failed to open CSV file: {}", e))?;
        
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to offset: {}", e))?;

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line)
            .map_err(|e| format!("Failed to read line: {}", e))?;

        CsvStorage::csv_line_to_row(&line, &self.schema)
    }
}

/// Iterator for streaming CSV rows (legacy)
pub struct CsvRowIterator {
    reader: BufReader<File>,
    schema: Vec<ColumnDef>,
    deleted_rows: std::collections::HashSet<usize>,
    row_offsets: HashMap<usize, u64>,
    current_row_id: usize,
    max_row_id: usize,
}

impl CsvRowIterator {
    fn new(base_path: &PathBuf, table_name: &str, schema: Vec<ColumnDef>, deleted_rows: std::collections::HashSet<usize>, row_offsets: HashMap<usize, u64>) -> Result<Self, String> {
        let data_path = base_path.join(format!("{}.csv", table_name));
        let file = File::open(&data_path)
            .map_err(|e| format!("Failed to open CSV file: {}", e))?;
        
        let reader = BufReader::new(file);
        let max_row_id = row_offsets.keys().max().copied().unwrap_or(0);

        Ok(Self {
            reader,
            schema,
            deleted_rows,
            row_offsets,
            current_row_id: 1,
            max_row_id,
        })
    }
}

impl Iterator for CsvRowIterator {
    type Item = Result<Row, String>;

    fn next(&mut self) -> Option<Self::Item> {
        // Find next non-deleted row
        while self.current_row_id <= self.max_row_id {
            if self.deleted_rows.contains(&self.current_row_id) {
                self.current_row_id += 1;
                continue;
            }

            // Get offset for this row
            if let Some(&offset) = self.row_offsets.get(&self.current_row_id) {
                // Seek to the row position
                if let Err(e) = self.reader.seek(SeekFrom::Start(offset)) {
                    return Some(Err(format!("Failed to seek to row {}: {}", self.current_row_id, e)));
                }

                // Read the line
                let mut line = String::new();
                match self.reader.read_line(&mut line) {
                    Ok(0) => {
                        self.current_row_id += 1;
                        continue; // End of file
                    }
                    Ok(_) => {
                        // Parse the CSV line
                        match CsvStorage::csv_line_to_row(&line, &self.schema) {
                            Ok(row) => {
                                self.current_row_id += 1;
                                return Some(Ok(row));
                            }
                            Err(e) => return Some(Err(e)),
                        }
                    }
                    Err(e) => return Some(Err(format!("Failed to read line: {}", e))),
                }
            } else {
                self.current_row_id += 1;
                continue;
            }
        }

        None // No more rows
    }
}

impl Storage for CsvStorage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String> {
        let table_name = name.to_lowercase();
        
        // Check if table already exists
        if self.tables.contains_key(&table_name) {
            return Err("table exists".to_string());
        }

        // Create empty CSV file
        let data_path = self.get_data_path(&table_name);
        File::create(&data_path)
            .map_err(|e| format!("Failed to create CSV file: {}", e))?;

        // Initialize table metadata
        self.tables.insert(table_name.clone(), CsvTableInfo {
            schema: schema.clone(),
            next_row_id: 1,
        });

        // Load existing index and deleted files for this table
        self.load_index(&table_name)?;
        self.load_deleted(&table_name)?;

        // Save catalog
        self.save_catalog()?;

        Ok(())
    }

    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String> {
        let table_name = table.to_lowercase();
        
        // Get schema and next_row_id before borrowing self
        let (schema_len, next_row_id) = {
            let table_info = self.tables.get(&table_name)
                .ok_or("table not found")?;
            (table_info.schema.len(), table_info.next_row_id)
        };

        // Validate row length
        if row.len() != schema_len {
            return Err("column count mismatch".to_string());
        }

        // Append row to CSV file
        let offset = self.append_row_to_csv(&table_name, &row)?;

        // Update index
        self.append_index(&table_name, next_row_id, offset)?;

        // Update table metadata
        if let Some(table_info) = self.tables.get_mut(&table_name) {
            table_info.next_row_id += 1;
        }

        // Save catalog
        self.save_catalog()?;

        Ok(())
    }

    fn schema(&self, table: &str) -> Result<&[ColumnDef], String> {
        let table_name = table.to_lowercase();
        self.tables
            .get(&table_name)
            .map(|t| t.schema.as_slice())
            .ok_or("table not found".to_string())
    }

    fn rows<'a>(&'a self, _table: &str) -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String> {
        // For CSV storage, we need to return a different iterator type
        // This is a limitation of the current trait design
        Err("CSV storage rows() method requires different implementation - use streaming iterator instead".to_string())
    }

    fn update_rows(&mut self, table: &str, assignments: &[Assignment], filter: Option<&Predicate>) -> Result<usize, String> {
        // For simplicity, implement as delete + insert
        // This avoids complex index updates
        let table_name = table.to_lowercase();
        let table_info = self.tables.get(&table_name)
            .ok_or("table not found")?;

        // Read all rows and apply updates
        let data_path = self.get_data_path(&table_name);
        let file = File::open(&data_path)
            .map_err(|e| format!("Failed to open CSV file: {}", e))?;
        let reader = BufReader::new(file);

        let mut updated_rows = Vec::new();
        let mut updated_count = 0;

        for line in reader.lines() {
            let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
            let mut row = Self::csv_line_to_row(&line, &table_info.schema)?;

            // Check if row matches filter and apply updates
            let matches = if let Some(predicate) = filter {
                evaluate_predicate(predicate, &row, &table_info.schema)
            } else {
                true
            };

            if matches {
                // Apply assignments
                for assignment in assignments {
                    if let Some(col_idx) = table_info.schema.iter().position(|c| c.name == assignment.column) {
                        let parsed_value = parse_value(&assignment.value, &table_info.schema[col_idx].data_type)?;
                        row[col_idx] = parsed_value;
                    } else {
                        return Err(format!("Column '{}' not found", assignment.column));
                    }
                }
                updated_count += 1;
            }

            updated_rows.push(row);
        }

        // Rewrite the entire CSV file
        let mut file = File::create(&data_path)
            .map_err(|e| format!("Failed to create CSV file: {}", e))?;

        let mut new_row_offsets = HashMap::new();
        let mut current_offset = 0u64;

        for row in updated_rows.iter() {
            let csv_line = Self::row_to_csv_line(row);
            writeln!(file, "{}", csv_line)
                .map_err(|e| format!("Failed to write CSV line: {}", e))?;

            // Find corresponding row_id for this offset
            if let Some((&row_id, _)) = self.row_offsets.iter().find(|(_, &old_offset)| old_offset == current_offset) {
                new_row_offsets.insert(row_id, current_offset);
            }

            current_offset = file.seek(SeekFrom::Current(0))
                .map_err(|e| format!("Failed to get current position: {}", e))?;
        }

        // Update index file
        self.row_offsets = new_row_offsets;
        let idx_path = self.get_index_path(&table_name);
        let mut idx_file = File::create(&idx_path)
            .map_err(|e| format!("Failed to create index file: {}", e))?;

        for (row_id, offset) in &self.row_offsets {
            writeln!(idx_file, "{},{}", row_id, offset)
                .map_err(|e| format!("Failed to write index: {}", e))?;
        }

        Ok(updated_count)
    }

    fn delete_rows(&mut self, table: &str, filter: Option<&Predicate>) -> Result<usize, String> {
        let table_name = table.to_lowercase();
        
        // Get schema before borrowing self
        let schema = self.tables.get(&table_name)
            .ok_or("table not found")?
            .schema.clone();

        // Load index and deleted files for this table
        self.load_index(&table_name)?;
        self.load_deleted(&table_name)?;

        // Read all rows and mark matching ones as deleted
        let data_path = self.get_data_path(&table_name);
        let file = File::open(&data_path)
            .map_err(|e| format!("Failed to open CSV file: {}", e))?;
        let reader = BufReader::new(file);

        let mut deleted_count = 0;
        let mut row_id = 1;

        for line in reader.lines() {
            let line = line.map_err(|e| format!("Failed to read line: {}", e))?;
            let row = Self::csv_line_to_row(&line, &schema)?;

            // Check if row matches filter
            let matches = if let Some(predicate) = filter {
                evaluate_predicate(predicate, &row, &schema)
            } else {
                true
            };

            if matches {
                // Mark as deleted
                self.append_deleted(&table_name, row_id)?;
                deleted_count += 1;
            }

            row_id += 1;
        }

        Ok(deleted_count)
    }

    fn get_all_rows(&self, table: &str) -> Result<Vec<Row>, String> {
        // Call the internal implementation
        self.get_all_rows_internal(table)
    }

    fn rows_streaming(&self, table: &str) -> Result<Box<dyn Iterator<Item = Result<Row, String>> + Send>, String> {
        // Create streaming iterator using random access
        let table_info = self.tables.get(table)
            .ok_or("table not found")?
            .clone();

        // Load index and deleted files
        let mut row_offsets = HashMap::new();
        let mut deleted_rows = std::collections::HashSet::new();
        
        // Load index
        let idx_path = self.get_index_path(table);
        if idx_path.exists() {
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
                    row_offsets.insert(row_id, offset);
                }
            }
        }

        // Load deleted rows
        let del_path = self.get_deleted_path(table);
        if del_path.exists() {
            let file = File::open(&del_path)
                .map_err(|e| format!("Failed to open deleted file: {}", e))?;
            let reader = BufReader::new(file);

            for line in reader.lines() {
                let line = line.map_err(|e| format!("Failed to read deleted line: {}", e))?;
                if let Ok(row_id) = line.parse::<usize>() {
                    deleted_rows.insert(row_id);
                }
            }
        }

        // Create iterator over valid row IDs
        let valid_row_ids: Vec<usize> = row_offsets.keys()
            .filter(|&&row_id| !deleted_rows.contains(&row_id))
            .copied()
            .collect();

        let data_path = self.get_data_path(table);
        let iterator = CsvStreamingIterator::new(data_path, valid_row_ids, row_offsets, table_info.schema.clone())?;
        
        Ok(Box::new(iterator))
    }
}

// Helper functions from mem.rs
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

fn evaluate_comparison(left: &Value, op: &crate::ast::CmpOp, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(l), Value::Int(r)) => match op {
            crate::ast::CmpOp::Eq => l == r,
            crate::ast::CmpOp::Ne => l != r,
            crate::ast::CmpOp::Lt => l < r,
            crate::ast::CmpOp::Le => l <= r,
            crate::ast::CmpOp::Gt => l > r,
            crate::ast::CmpOp::Ge => l >= r,
        },
        (Value::Text(l), Value::Text(r)) => match op {
            crate::ast::CmpOp::Eq => l == r,
            crate::ast::CmpOp::Ne => l != r,
            crate::ast::CmpOp::Lt => l < r,
            crate::ast::CmpOp::Le => l <= r,
            crate::ast::CmpOp::Gt => l > r,
            crate::ast::CmpOp::Ge => l >= r,
        },
        (Value::Bool(l), Value::Bool(r)) => match op {
            crate::ast::CmpOp::Eq => l == r,
            crate::ast::CmpOp::Ne => l != r,
            _ => false,
        },
        _ => false,
    }
}
