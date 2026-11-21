# データベースアーキテクチャ - ファイル分類

このドキュメントでは、リポジトリ内の各ファイルがデータベースの標準的な処理フロー（Parser → Optimizer → Executor → Storage）のどこに属するかをまとめています。

## 標準的なデータベースの処理フロー

```
SQLクエリ → Parser → Optimizer → Executor → Storage
              ↓         ↓          ↓         ↓
            AST    実行計画    実行結果    データ永続化
```

## ファイル分類

### 1. Parser（構文解析層）

SQLクエリを解析してAST（抽象構文木）に変換する層。

#### 主要ファイル
- **`src/parser.rs`**
  - パーサーモジュールのエントリーポイント
  - SQL文の種類に応じて適切なパーサーにルーティング

#### 各SQL文のパーサー
- **`src/parser/select.rs`**
  - SELECT文の構文解析
  - WHERE句の解析も含む

- **`src/parser/insert.rs`**
  - INSERT文の構文解析
  - VALUES句の解析

- **`src/parser/create.rs`**
  - CREATE TABLE文の構文解析
  - カラム定義の解析

- **`src/parser/update.rs`**
  - UPDATE文の構文解析
  - SET句とWHERE句の解析

- **`src/parser/delete.rs`**
  - DELETE文の構文解析
  - WHERE句の解析

#### AST定義
- **`src/ast.rs`**
  - 抽象構文木（AST）のデータ構造定義
  - `Stmt`（ステートメント）、`Predicate`（述語）、`ColumnDef`（カラム定義）など

---

### 2. Optimizer（最適化層）

**現在、このプロジェクトにはOptimizerは実装されていません。**

ASTを最適化して実行計画を生成する層ですが、現在はASTを直接Executorに渡しています。

将来的に追加する場合の想定機能：
- クエリ最適化（インデックス利用の決定など）
- 実行計画の生成
- コストベース最適化

---

### 3. Executor（実行層）

ASTを受け取り、実際にクエリを実行して結果を返す層。

#### 主要ファイル
- **`src/engine.rs`**
  - エンジンのメインインターフェース
  - Storageの初期化とExecutorへの委譲を行う
  - 外部からは`Engine::execute()`でクエリを実行

- **`src/engine/executor.rs`**
  - 実際のクエリ実行ロジック
  - ASTの各ステートメントタイプに応じて処理を分岐
  - 述語（Predicate）の評価も行う

#### メタデータ管理
- **`src/engine/catalog.rs`**
  - カタログ管理（テーブルメタデータの管理）
  - テーブルの作成・取得

- **`src/engine/table.rs`**
  - テーブル構造の定義
  - スキーマと行データの保持

#### データ型
- **`src/engine/value.rs`**
  - 値型（Value）の定義
  - Int, Text, Bool, Null の各型

---

### 4. Storage（ストレージ層）

データの永続化と読み書きを担当する層。複数の実装を抽象化。

#### 抽象化レイヤー
- **`src/storage.rs`**
  - `Storage`トレイトの定義
  - ストレージタイプ（Memory, Csv, Paged）の定義
  - 統一されたストレージインターフェース

#### 実装
- **`src/storage/mem.rs`**
  - メモリストレージ実装
  - データをメモリ上（HashMap）に保持

- **`src/storage/csv.rs`**
  - CSVファイルストレージ実装
  - データをCSVファイルに永続化
  - インデックスファイル（.idx）と削除マーカーファイル（.del）をサポート

- **`src/storage/paged.rs`**
  - ページストレージ実装（4KB固定ページ）
  - より高度なストレージ管理

---

### 5. その他

- **`src/main.rs`**
  - アプリケーションのエントリーポイント
  - REPL（Read-Eval-Print Loop）の実装
  - ユーザー入力の受け取りと結果の表示

---

## 処理フローの例

### SELECTクエリの実行フロー

```
1. main.rs
   ↓ ユーザー入力: "SELECT * FROM users WHERE id = 1"
   
2. parser.rs
   ↓ parse_sql() でルーティング
   
3. parser/select.rs
   ↓ parse_select_all() でASTに変換
   
4. ast.rs
   ↓ Stmt::Select { table, columns, filter } を生成
   
5. engine.rs
   ↓ Engine::execute() でExecutorに委譲
   
6. engine/executor.rs
   ↓ execute() でSELECT処理を実行
   ↓ storageからデータを取得し、filterを適用
   
7. storage.rs (trait)
   ↓ Storage::rows_streaming() を呼び出し
   
8. storage/mem.rs または storage/csv.rs
   ↓ 実際のデータ読み取り
   
9. engine/executor.rs
   ↓ 結果をQueryResult::Rowsとして返す
   
10. main.rs
    ↓ 結果を表示
```

---

## 現在のアーキテクチャの特徴

1. **Optimizerが存在しない**
   - ASTを直接Executorに渡している
   - シンプルな実装だが、最適化の余地あり

2. **Storage抽象化**
   - 複数のストレージ実装を統一インターフェースで扱える
   - メモリ、CSV、ページストレージを切り替え可能

3. **Executor内での述語評価**
   - WHERE句の評価はExecutor内で行っている
   - 将来的にはStorage層でプッシュダウンできる可能性あり


