# Why do we need a page layout? (Draft)

## Context
To read and write data from/to a file, we need to define a format that both the writer and the reader agree on.

Common examples are CSV or JSON, but database storage engines often use a page-based layout instead.
Why is a page layout adopted, and why do we typically see page sizes like 4KB or 8KB?

## Notes (WIP)
If we take CSV as an example,

## Questions
- What problems do CSV/JSON have for a DB storage engine? (updates, deletes, random access, indexing, etc.)
- What advantages does a fixed-size page provide? (I/O efficiency, caching, allocation, concurrency, WAL, ...)
- Why 4KB / 8KB specifically? (OS page size, filesystem blocks, trade-offs)
