# WAL Implementation Tasks

## 実装ステップ1: Pending ChangesとWALの導入

pending_changesとWALログの仕様については `.kiro/specs/wal-implementation/design.md` を参考にしてください。

- [ ] `src/engine/pending.rs` を作成し、`TxId`・`PendingChange`・`TransactionState` を定義する（page_id / slot_id を含む）
- [ ] `src/engine.rs` の `Engine` 構造体に `pending_changes: HashMap<TxId, TransactionState>` フィールドを追加する
- [ ] `Storage` トレイトの `insert_row` の戻り値を `Result<(u32, u32), String>`（page_id, slot_id）に変更し、各実装を更新する（MemStorage・CsvStorageは `(0, 0)` を返す）
- [ ] `Storage` トレイトの `update_rows` / `delete_rows` の戻り値を `Result<Vec<(u32, u32)>, String>` に変更し、各実装を更新する
- [ ] `PagedStorage::insert_row` / `update_rows` / `delete_rows` を、物理書き込みをせずpage_id / slot_idを返すだけに変更する
- [ ] `executor.rs` の INSERT / UPDATE / DELETE 処理で、`storage_mut().insert_row()` 等の戻り値を使って `pending_changes` へ登録する（TxId は `1` 固定）
- [ ] `src/storage/wal.rs` を作成し、WALレコードのデータ構造（ヘッダ・ボディ）を定義する
- [ ] `src/storage/wal.rs` にWALファイルへのappendロジックを実装する（LSNの採番を含む）
- [ ] `PagedStorage` に `wal: Wal` フィールドを追加する
- [ ] `Storage` トレイトに `commit_changes(&[PendingChange]) -> Result<(), String>` を追加する
- [ ] `PagedStorage::commit_changes` で、PendingChangesをFIFO順でWALにappendし、Commitレコードをappendしてfsyncし、その後で物理ページに書き込む
- [ ] `MemStorage::commit_changes` / `CsvStorage::commit_changes` はPendingChangesを元に単純に実データへ反映する
- [ ] `executor.rs` で INSERT / UPDATE / DELETE 実行後、TxId=1固定で `storage.commit_changes(&pending_changes)` を呼ぶ（auto commit）

## 実装ステップ2: トランザクションの導入

- [ ] `ast.rs` の `Stmt` enum に `BeginTransaction` / `CommitTransaction` バリアントを追加する
- [ ] `src/parser/` に `BEGIN TRANSACTION` / `COMMIT TRANSACTION` の構文解析を追加する
- [ ] `Engine` に `active_tx_id: Option<TxId>` と `next_tx_id: TxId` フィールドを追加する
- [ ] `executor.rs` に `BeginTransaction` ハンドラを追加し、`next_tx_id` を採番して `active_tx_id` にセットする
- [ ] `executor.rs` の INSERT / UPDATE / DELETE 処理で、`active_tx_id` が `Some(id)` ならそのTxId、`None` なら `next_tx_id` を採番した一時TxIdで `pending_changes` に登録する
- [ ] `active_tx_id` が `None`（auto commit）の場合、登録直後に `storage.commit_changes` を呼んで `pending_changes` を破棄する
- [ ] `executor.rs` に `CommitTransaction` ハンドラを追加し、`storage.commit_changes(&pending_changes[active_tx_id])` を呼んで `active_tx_id` を `None` に戻す

## 実装ステップ3: Crash Recovery

（詳細は今後追記）
