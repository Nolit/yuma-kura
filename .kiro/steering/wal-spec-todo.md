# WAL Spec TODO

前セッションで確認した、`.kiro/specs/wal-implementation/` のspec整備タスクです。
実装を始める前にこれらを対応してください。

## 優先度高

- [ ] `.kiro/specs/wal-implementation/.config.kiro` を作成する
  - Kiroがspecのワークフロー種別を認識するためのメタデータファイル
  - 内容例: `{ "specType": "feature", "workflowType": "requirements-first" }`

- [ ] `requirements.md` をKiro標準フォーマットに整備する
  - User Story形式（`As a ... I want ... so that ...`）の追加
  - Acceptance Criteria（受け入れ基準）の追加
  - Correctness Properties（不変条件）の追加

## 優先度中

- [ ] `tasks.md` の各タスクにチェックボックス（`- [ ]`）を追加する
- [ ] `tasks.md` の各ステップを細かい粒度のサブタスクに分割する
  - 例: ステップ1 → 「PendingChange型の定義」「Engineへの組み込み」「ストレージへの反映」など
- [ ] `tasks.md` のステップ4（Crash Recovery）を具体化する

## 優先度低（design.mdの細部）

- [ ] チェックサムアルゴリズムを明記する（例: CRC32）
- [ ] ページ側の `last_applied_lsn` フィールドの設計を追加する
- [ ] LSNの型・初期値・採番ルールを定義する
