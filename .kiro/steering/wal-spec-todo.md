# WAL Spec TODO

`.kiro/specs/wal-implementation/` のspec整備タスクです。
**すべて対応済みです**（`tasks.md` のステップ1・ステップ2の実装も完了している）。以降は、整備した内容の記録として残しています。

## 優先度高

- [x] `.kiro/specs/wal-implementation/.config.kiro` を作成する
  - Kiroがspecのワークフロー種別を認識するためのメタデータファイル
  - 内容: `{ "specType": "feature", "workflowType": "requirements-first" }`

- [x] `requirements.md` をKiro標準フォーマットに整備する
  - User Story形式（`As a ... I want ... so that ...`）の追加（REQ-1〜REQ-5）
  - Acceptance Criteria（受け入れ基準）の追加
  - Correctness Properties（不変条件）の追加（CP-1〜CP-7）

## 優先度中

- [x] `tasks.md` の各タスクにチェックボックス（`- [ ]`）を追加する
- [x] `tasks.md` の各ステップを細かい粒度のサブタスクに分割する
  - ステップ1 → 「`PendingChange` 型の定義」「ページヘッダの拡張」「一時マップと作業用カタログ」「WALの追記ロジック」「`commit_changes` / `abort_changes`」など
- [x] `tasks.md` のCrash Recoveryを具体化する
  - 現在の `tasks.md` では「実装ステップ2: Crash Recovery」。Recoveryの枠、WALの検査、`pages.dat` と `catalog.json` の書き戻し、WALの切り詰め、起動順序の変更、`Engine::new` のフォールバック廃止に分けている

## 優先度低（design.mdの細部）

- [x] チェックサムアルゴリズムを明記する（CRC32。`checksum` フィールドを除く、ヘッダとボディ全体が対象）
- [x] ページ側の `last_applied_lsn` フィールドの設計を追加する（`u64`、0は「WAL未適用」。「`last_applied_lsn`」の節）
- [x] LSNの型・初期値・採番ルールを定義する（`u64`、初期値1、全レコードで+1、0は予約。WAL「ヘッダ」の `lsn`）

## 関連

- 決定事項とその理由は `docs/decisions/ADR-0003-wal.md` の「Decision」に記録している
