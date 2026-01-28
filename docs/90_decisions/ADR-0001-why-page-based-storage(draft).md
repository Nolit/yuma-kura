# Why do we need a page layout? (Draft)

## Context
To read and write data from/to a file, we need to define a format that both the writer and the reader agree on.

Common examples are CSV or JSON, but database storage engines often use a page-based layout instead.
Why is a page layout adopted, and why do we typically see page sizes like 4KB or 8KB?

## Notes (WIP)
Let us start with CSV as a simple example.  
When opened in tools such as Excel, CSV files are displayed in a tabular format.  
However, in reality, a CSV file is nothing more than a sequence of bytes laid out sequentially.
In the example below, the first column represents a name and the second column represents an age.

```
| Tom,30\n Eddie,20\n Jack,50\n |
^ ^
0 EOF
```


Using this file, we will examine how insert, update, and delete operations would be implemented.

---

### Insert

First, consider inserting a new record for Bob, aged 40.  
A naive implementation would follow these steps:

1. Check the file size  
2. Append the new data (`Bob,40\n`) to the end of the file  

Since this operation does not require moving any existing data and only appends to the end of the file, it can be completed in O(1) time.

---

### Update

Next, consider updating the second row, changing `Eddie` to `Jack`.

1. Read the file from the beginning and count newline characters to locate the start of the second row  
2. Since `Tom,30\n` is 7 bytes long, the second row starts at offset 7  
3. Overwrite `Eddie` with `Jack`  
4. Because the byte length of the strings differs, leftover bytes remain after overwriting  
   To fix this, all subsequent bytes must be shifted one byte forward  

Step 4 affects the file all the way to the end, resulting in an O(n) operation proportional to the file size.

For small files, it may be possible to load the entire file into memory and rewrite it.  
However, for large datasets typically handled by databases, this would require buffering data from storage and repeatedly shifting bytes forward, which is prohibitively expensive.

---

### Delete

Now consider deleting the second row, `Eddie,20`.

1. Read from the beginning of the file and count newline characters to locate the start of the target row  
2. Continue reading until the next newline to determine that the target record is 8 bytes long  
3. Shift all subsequent data forward by 8 bytes  

As with updates, deletion requires moving all data that follows the deleted record, resulting in an O(n) operation.

While it is possible to temporarily optimize deletions by tracking deleted records separately, the same data movement problem reappears when performing a vacuum (reorganization) operation.

---

### The Core Problem with CSV

The fundamental problem with CSV is that  
**the impact of data modification tends to propagate across the entire file**.

Because records are stored directly as contiguous byte sequences in the file, insertions, updates, and deletions can cause the byte layout to shift.  
When this happens, the effect propagates all the way to the end of the file.

In other words, with CSV,  
**the effective unit of data management becomes the file itself**.

---

## Page-Based Data Management

To address this issue, databases manage data in fixed-size pages.

In the following sections, we will use a 4KB fixed-size page as an example and examine how insert, update, and delete operations are performed.

```
4KB Page
+--------------------------------------------------+
| Page Header                                      |
|  - Page ID                                      |
|  - Free Space Offset  -> 2600                   |
|  - Slot Count                                   |
+--------------------------------------------------+
| Slot Directory                                  |
|--------------------------------------------------|
| Slot 0 -> offset 3200                           |
| Slot 1 -> offset 3000                           |
| Slot 2 -> FREE                                 |
| Slot 3 -> offset 2600                           |
+--------------------------------------------------+
|                                                  |
|               (free space)                       |
|                                                  |
| Row Data                                        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3200        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3000        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 2600        |
| +----------------------+                        |
+--------------------------------------------------+
```


---

### Inserting a Row into a Page

Using this page layout, insertion proceeds as follows:

1. Since the Free Space Offset is 2600, new row data must be placed before this boundary  
2. If the row size is 100 bytes, the new row starts at `2600 - 100 = 2500`  
3. Write the row data into the range `2500–2600`  
4. Reuse the free slot (Slot 2) and set `Slot 2 -> 2500`  
5. Update the Free Space Offset to 2500, establishing the new insertion boundary  

Because the Slot Directory grows from the beginning of the page, row data is placed from the end of the page toward the front.  
This structure allows variable-length records to be inserted while keeping the operation confined within a single page.


## Questions
- What advantages does a fixed-size page provide? (I/O efficiency, caching, allocation, concurrency, WAL, ...)
- Why 4KB / 8KB specifically? (OS page size, filesystem blocks, trade-offs)

