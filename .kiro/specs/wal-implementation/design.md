# WAL Implementation Design

## WALログ

WALの実ログファイル。

### データ構造

**ヘッダ**

| フィールド | 説明 |
|---|---|
| `lsn` | WALログの各レコードに一意に割り当てられるインクリメンタルなID。各ページはどのlsnまで適用済みかを持ち、未適用のWALレコードでリカバリする |
| `tx_id` | トランザクションID |
| `type` | Insert / Update / Delete / Commit |
| `len` | WALレコード全体のサイズ。recovery時に次のrecord位置を特定するために使用 |
| `checksum` | record破損やpartial writeを検知するためのチェックサム |

**ボディ**

```
- Insert(table_id, page_id, slot_id, row_data)
- Update(table_id, page_id, slot_id, row_data)
- Delete(table_id, page_id, slot_id)
- Commit(Empty)
```

### ライフサイクル

- Commit時、Pending Changesを元にFIFO順でappendされる
- トランザクションCommit時の最後にCommit recordがappendされる
- ページデータ更新前にfsyncされる
- WALログはappend専用であり、fsync後にWALレコードが更新されることはない
- 現時点ではコンパクションもせず、削除されることもない（将来対応予定）

---

## Pending Changes

データ変更を伴うSQLの実行結果は、commit 前の未確定変更としてメモリ上に保持される。
この未確定変更を Pending Changes と呼ぶ。
Pending Changes は commit 時に WAL record へ変換され、永続化の対象となる。

### データ構造

以下のようにトランザクションID(TxId)とTransactionStateのハッシュマップとする。

```
HashMap<TxId, TransactionState>
```

TransactionStateは以下のようにPendingChangeのリストとし、FIFOの順序を保証するものとする。

```
TransactionState {
    changes: Vec<PendingChange>   // FIFO順を保証
}
```

PendingChangeは以下のような各変更種別のenumとする。

```
enum PendingChange {
    Insert(table_id, page_id, slot_id, row_data),
    Update(table_id, page_id, slot_id, row_data),
    Delete(table_id, page_id, slot_id),
}
```

### 制約

現時点では1トランザクション = 1 SQLとする。
この制約により、insert_row / update_rows / delete_rows呼び出し時に
同一トランザクション内の未コミット変更が存在しないことが保証される。
そのため、ページの空き場所の計算は既存のページファイルの状態のみを見れば十分であり、
Buffer Poolなしで page_id / slot_id を正確に算出できる。

将来的にBuffer Poolを導入する際は、この制約を撤廃し、
メモリ上のページキャッシュを空き場所算出の基準とする形に再設計する。

### ライフサイクル

- ページデータへの変更がある場合、pending changesを生成しトランザクションID毎にバッファリング
- Commit時にpending changesを元にredoログのレコードへ順番に変換
- WALのflush後にpending changesを元にページデータを変更
- transaction完了時に対応するpending changesを破棄

### 非スコープ

- ロールバック対応
- buffer poolとの統合は将来的に対応
- MVCCは将来的に対応
