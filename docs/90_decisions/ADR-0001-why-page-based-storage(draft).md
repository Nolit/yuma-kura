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
| | Row Header | Data    |  <- offset 2600        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3000        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3200        |
| +----------------------+                        |
+--------------------------------------------------+

Row
+----------------------------------+
| Row Header                       |
|  - total_length                  |
|  - flags (deleted, moved, etc)   |
+----------------------------------+
| User Data (payload)              |
+----------------------------------+

```


---

### Inserting a Row into a Page


ここに100バイトのでーたを入れるケースを考えてみます。
挿入後のページデータは以下のようになります。

```
Before (Insert前)                                  After (Insert後)
4KB Page                                          4KB Page
+--------------------------------------------------+      +--------------------------------------------------+
| Page Header                                      |      | Page Header                                      |
|  - Page ID                                      |      |  - Page ID                                      |
|  - Free Space Offset  -> 2600                   |  ==> |  - Free Space Offset  -> 2500                   |
|  - Slot Count                                   |      |  - Slot Count                                   |
+--------------------------------------------------+      +--------------------------------------------------+
| Slot Directory                                  |      | Slot Directory                                  |
|--------------------------------------------------|      |--------------------------------------------------|
| Slot 0 -> offset 3200                           |      | Slot 0 -> offset 3200                           |
| Slot 1 -> offset 3000                           |      | Slot 1 -> offset 3000                           |
| Slot 2 -> FREE                                 |      | Slot 2 -> offset 2500                           |
| Slot 3 -> offset 2600                           |      | Slot 3 -> offset 2600                           |
+--------------------------------------------------+      +--------------------------------------------------+
|                                                  |      |                                                  |
|               (free space)                       |      |               (free space)                       |
|                                                  |      |                                                  |
| Row Data                                        |      | Row Data                                        |
| +----------------------+                        |      | +----------------------+                        |
| | Row Header | Data    |  <- offset 2600        |      | | Row Header | Data    |  <- offset 2500        |
| +----------------------+                        |      | +----------------------+                        |
| | Row Header | Data    |  <- offset 3000        |      | | Row Header | Data    |  <- offset 2600        |
| +----------------------+                        |      | +----------------------+                        |
| | Row Header | Data    |  <- offset 3200        |      | | Row Header | Data    |  <- offset 3000        |
| +----------------------+                        |      | +----------------------+                        |
|                                                  |      | | Row Header | Data    |  <- offset 3200        |
|                                                  |      | +----------------------+                        |
+--------------------------------------------------+      +--------------------------------------------------+

```

Insertion proceeds as follows:

1. Since the Free Space Offset is 2600, new row data must be placed before this boundary  
2. If the row size is 100 bytes, the new row starts at `2600 - 100 = 2500`  
3. Write the row data into the range `2500–2600`  
4. Reuse the free slot (Slot 2) and set `Slot 2 -> 2500`  
5. Update the Free Space Offset to 2500, establishing the new insertion boundary  

Because the Slot Directory grows from the beginning of the page, row data is placed from the end of the page toward the front.  
This structure allows variable-length records to be inserted while keeping the operation confined within a single page.
これはあくまで4KBの一つのページ内の話であり、データが追加されていき4KBを超えれば次のページが作られることとなる。

```
[ existing pages ][ page 40 ][ page 41 ][ page 42 ]
                                             ↑
                                       newly appended
```

ちなみに挿入先ページの探索だが、本来であればメタ情報として各ページの空き容量を保持するが現在は頭からページを読み込んでいる。挿入毎にO(N)の計算量が必要となるのでもちろん効率は良くない

## 更新の場合
先ほどのページを例に次は更新処理を見ていきたい

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
| | Row Header | Data    |  <- offset 2600        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3000        |
| +----------------------+                        |
| | Row Header | Data    |  <- offset 3200        |
| +----------------------+                        |
+--------------------------------------------------+
```

ここではSlot1のデータを変更するとする。データのオフセットを見るにSlot1のデータ量は3200-3000=200バイトなのだが変更後に100バイトに減るとする
サイズが減る場合は変更後の100バイトを書き込み、Row Headerを変えるだけで良く他のページはおろか他のRowにも影響を与えない。

```
Before (slot1: 200B)                                After (slot1: 100B)
offset = 3000                                       offset = 3000
+------------------------------------------------+  +------------------------------------------------+
| Row Header                                     |  | Row Header                                     |
|  - row_length = 200                            |  |  - row_length = 100                            |
|  - flags = 0                                   |  |  - flags = 0                                   |
+------------------------------------------------+  +------------------------------------------------+
| Payload (200B)                                 |  | Payload (100B)                                 |
|  [............................................] |  |  [....................]                        |
|  [............................................] |  |                                                 |
+------------------------------------------------+  +------------------------------------------------+
                                                   |  +------------------------------------------------+
                                                   |  | Unused / Padding (100B)                        |
                                                   |  |  [............................................] |
                                                   |  +------------------------------------------------+

```

Slot1のデータサイズを400Bに増やす場合で考えてみる。
この場合、元の位置に400Bを書き込むと次のslot0を上書きしてしまうので、slot1が400B空いているスペースを指す必要がある

```
Before (slot1: 200B at 3000)                         After (slot1: 400B moved, dead space at 3000-3200)
4KB Page                                             4KB Page
+--------------------------------------------------+  +--------------------------------------------------+
| Page Header                                      |  | Page Header                                      |
|  - Page ID                                      |  |  - Page ID                                      |
|  - Free Space Offset  -> 2600                   |  |  - Free Space Offset  -> 2200                   |
|  - Slot Count                                   |  |  - Slot Count                                   |
+--------------------------------------------------+  +--------------------------------------------------+
| Slot Directory                                  |  | Slot Directory                                  |
|--------------------------------------------------|  |--------------------------------------------------|
| Slot 0 -> offset 3200                           |  | Slot 0 -> offset 3200                           |
| Slot 1 -> offset 3000                           |  | Slot 1 -> offset 2200   (UPDATED)               |
| Slot 2 -> FREE                                 |  | Slot 2 -> FREE                                 |
| Slot 3 -> offset 2600                           |  | Slot 3 -> offset 2600                           |
+--------------------------------------------------+  +--------------------------------------------------+
|                                                  |  |                                                  |
|               (free space)                       |  |               (free space)                       |
|                                                  |  |                                                  |
| Row Data                                        |  | Row Data                                        |
| +----------------------+                        |  | +----------------------+                        |
| | Row Header | Data    |  <- offset 2600        |  | | Row Header | Data    |  <- offset 2200        |
| +----------------------+                        |  | |   (slot1, 400B NEW)  |                        |
| | Row Header | Data    |  <- offset 3000        |  | +----------------------+                        |
| |   (slot1 old, 200B)  |                        |  | | Row Header | Data    |  <- offset 2600        |
| +----------------------+                        |  | +----------------------+                        |
| | Row Header | Data    |  <- offset 3200        |  | | (dead space: 3000–3200)                        |
| +----------------------+                        |  | +----------------------+                        |
|                                                  |  | | Row Header | Data    |  <- offset 3200        |
+--------------------------------------------------+  +--------------------------------------------------+

```

ここでCSVのケースと違いを意識したいのが、CSVの場合はファイルのそれ以降の全データに影響を与えたが、今回は変更が1つのページ内に閉じている点である。
dead spaceについては特定のタイミングでまとめられると想像するがまだその実装はできていない。

## 削除の場合
TODO
- Row Headerのフラグを立てる or slotをFreeにしたら良さそう

## Questions
- What advantages does a fixed-size page provide? (I/O efficiency, caching, allocation, concurrency, WAL, ...)
- Why 4KB / 8KB specifically? (OS page size, filesystem blocks, trade-offs)


