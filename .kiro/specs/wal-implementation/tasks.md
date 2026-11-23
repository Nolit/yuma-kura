# WAL Implementation Tasks

## 実装ステップ1: Pending ChangesとWALの導入

Pending ChangesとWALログの仕様については `.kiro/specs/wal-implementation/design.md` を参考にしてください。

- [x] `src/storage/pending.rs` を作成し、`PendingChange` を定義する。`PendingChange` は `PageWrite(page_id: u32, page_data: [u8; 4096])` と `CatalogWrite(catalog_json: Vec<u8>)` の2種とする（ページ・カタログのイメージを持つStorage固有の型）
- [x] `PageHeader` に `last_applied_lsn: u64` を追加し、`PAGE_HEADER_SIZE` を20から28バイトに変更する（`from_bytes` / `to_bytes` も更新する）。既存の `pages.dat` とはレイアウトが互換でなくなるため、既存のデータファイルは作り直す
- [x] `Storage` トレイトの戻り値を変更し、各実装を更新する。`PendingChange` はStorage内部の型であり、トレイトの署名には出さない
  - `create_table` / `insert_row`: `Result<(), String>`
  - `update_rows` / `delete_rows`: `Result<usize, String>`（`usize` は更新・削除した行数）
  - MemStorage・CsvStorage は、これまでどおり呼び出し時点で実データへ書き込む
- [x] `PagedStorage` に、1つのSQLの実行中に変更したページを保持する一時マップ（page_id → ページイメージ）を導入する。`read_page` はまずこのマップを参照し、なければディスクから読む。1回のUPDATEで複数のページを変更したり、同じSQL内で作った新ページを再び読んだりするために必要となる
- [x] `PagedStorage` に、SQLの実行中に変更するカタログを保持する作業用カタログ（`Option<Catalog>` など）を導入する
  - `create_table` / `insert_row` / `update_rows` / `delete_rows` の冒頭で、正式なカタログ（`self.catalog`）をコピーして作る。コピーのため `Catalog` / `TableMetadata` に `Clone` を derive する
  - `create_new_page`、`find_page_with_space`、`next_page_id` / `next_row_id` / `last_page_id` の採番と更新など、SQL実行中のカタログの参照と更新は、すべて作業用カタログに対して行う
  - 正式なカタログは、`commit_changes` のfsync成功後に作業用カタログで置き換えるまで変更しない
- [x] `PagedStorage::create_table` / `insert_row` / `update_rows` / `delete_rows` を、物理書き込みと `save_catalog` をせず、変更を一時マップと作業用カタログにだけ反映するように変更する。`PendingChange` は、ここでは作らない（`commit_changes` の中で作る。`design.md` の「Pending Changesの生成タイミング」参照）
  - `create_new_page` も物理書き込みをせず、新しいページのイメージを一時マップに保存する形に変更する
- [x] `PagedStorage` に、一時マップと作業用カタログから `PendingChange` のリストを作る処理を実装する（`commit_changes` から呼ぶ内部関数）
  - 一時マップの各ページから、`page_id` 順に `PageWrite` を1件ずつ作る。新規ページ、データを書いたページ、`next_page` を更新した前の末尾ページは、すべて一時マップに入っているため、漏れなく `PageWrite` になる。同じページを何回変更しても1件だけになる
  - カタログが変わる操作（CREATE TABLE、INSERT、UPDATE での新ページ作成）では、変更後のカタログ全体を `CatalogWrite` として作る（DELETE は作らない）
- [x] `src/storage/wal.rs` を作成し、WALレコードのデータ構造（ヘッダ・ボディ。`FullPageWrite` / `CatalogWrite` / `Commit`）を定義する
- [x] `src/storage/wal.rs` にWALファイルへのappendロジックを実装する
  - LSNはレコードごとに+1採番する（初期値1）
  - `Wal` は、次に採番するLSNを返す `next_lsn()` を公開する。`last_applied_lsn` の書き込みは、ページヘッダの構造を知る `PagedStorage` が、`append` の呼び出し前に行う（`design.md` の「`last_applied_lsn`」参照）。`append` は、レコードを符号化してチェックサム（CRC32）を計算し、`wal.log` に追記して、採番したLSNを返す
  - 起動時にWALを読み、最後の有効なレコードのLSN+1から採番を再開する（WALが空なら1）
  - WALを先頭から読む処理は、`dump`、LSNの再開、ステップ2のRecoveryで共通に使えるよう、1つの読み込み処理（レコードを1件ずつ返すイテレータなど）として実装する。読み込みを止める条件は、`design.md` の「WALの読み込みを止める条件」に従う
- [x] `PagedStorage` に `wal: Wal` フィールドを追加する
- [x] `Storage` トレイトに `commit_changes()`（引数なし）を追加する。戻り値は、致命的エラーを通常のSQLエラーと区別できる `Result` とする（エラー型は実装時に決める）。`commit_changes` の中のエラー（WALのappend、fsync、実ファイルへの書き込み）は、すべて致命的エラーとして返す。「Commit中・Commit後の失敗時の扱い」を参照
- [x] `Storage` トレイトに `abort_changes()` を追加する。SQLが `commit_changes` を呼ぶ前に失敗したときに、そのSQLの未Commitの変更（一時マップと作業用カタログ）を破棄するために呼ぶ。メモリ上のものを捨てるだけなので、戻り値は持たない
- [x] `PagedStorage::commit_changes` で、一時マップと作業用カタログから `PendingChange` を作り、作った順に（`PageWrite` は `page_id` 順）WALにappendし、Commitレコードをappendしてfsyncする。fsync成功後（Commit確定後）に、作業用カタログを正式なカタログに置き換え、その後で `PageWrite` を `pages.dat` に、`CatalogWrite` を `catalog.json` に書き込む。`PageWrite` ごとに `page_data` を複製してLSNを設定し、WALへの書き込みと `pages.dat` への書き込みには、このLSN設定済みページイメージの同じバイト列を使う（`PendingChange` 自体は変更しない）。書き込み後は、SQL実行中に使った一時マップを破棄する
- [x] `MemStorage::commit_changes` / `CsvStorage::commit_changes` は no-op として実装する（insert_row / update_rows / delete_rows 時点で既に書き込み済みのため）
- [x] `PagedStorage::abort_changes` で、一時マップと作業用カタログを破棄する。正式なカタログとページファイルは変更しない。`MemStorage::abort_changes` / `CsvStorage::abort_changes` は no-op として実装する（呼び出し時点で実データへ書き込み済みのため、破棄できない。「バックエンドごとの差異」参照）
- [x] `executor.rs` で CREATE TABLE / INSERT / UPDATE / DELETE 実行後、`storage.commit_changes()` を呼ぶ（auto commit）
- [x] `executor.rs` の CREATE TABLE / INSERT / UPDATE / DELETE 処理で、`commit_changes` を呼ぶ前にエラーが発生した場合は、`storage_mut().abort_changes()` を呼んでからエラーを返す。`commit_changes` が返したエラー（致命的エラー）では `abort_changes` を呼ばず、そのまま伝える
- [x] 致命的エラーを `main` まで伝え、プロセスを終了する
  - `Engine::execute` の戻り値のエラー型を、致命的エラーと通常のSQLエラーを区別できる型にする
  - `executor.rs` は、`commit_changes` が返した致命的エラーを変換せずにそのまま伝える
  - `src/main.rs` は、致命的エラーを受け取ったら、エラー内容を標準エラーに出力し、0以外の終了コードでプロセスを終了する。通常のSQLエラーは、今までどおりエラーを表示してループを続ける
- [x] `src/storage/wal.rs` にWALファイルを読み込んで内容を表示する `dump` メソッドを実装する
- [x] `src/main.rs` に `dump-wal <path>` サブコマンドを追加し、`cargo run -- dump-wal <path>` でWALの内容をコンソールに表示できるようにする

### ステップ1 完了条件

- 通常のCRUD操作がステップ1以前と同じ結果を返すこと（既存テストが通ること）
- INSERT / UPDATE / DELETE 後にWALファイルが生成されていること
- 複数ページにまたがるINSERT / UPDATE / DELETE の後でも、全行が正しく読めること
- 複数ページにまたがる操作が、1つのトランザクションとして `FullPageWrite` 複数件と `Commit` 1件でWALに記録されること
- カタログが変わる操作では、`CatalogWrite` がWALに記録されること
- Commit後、`pages.dat` の各ページの `last_applied_lsn` が、WALの対応する `FullPageWrite` のLSNと一致すること。あわせて、そのページのバイト列がWALの `page_data` と完全に一致すること
- `cargo run -- dump-wal <path>` でWALレコードの内容（LSN・Type・checksum）が表示されること
- チェックサムが正しく計算されていること（dump時にOK/NGが表示されること）
- SQLが `commit_changes` の前に失敗した場合（例: 複数ページにまたがるUPDATEの途中でエラー）、`abort_changes` により、正式なカタログ、`pages.dat`、`catalog.json`、WALのいずれも変更されていないこと。失敗の直後に、同じテーブルへのINSERTとSELECTが成功すること
- `commit_changes` が致命的エラーを返した場合、通常のSQLエラーと区別できること（実ファイルへの書き込みの失敗を模擬して確認する）
- 致命的エラーの後にストレージを開き直すと、Recoveryは未実装のため、ステップ1の時点では起動時のRecoveryによる復旧は確認しない（ステップ2で確認する）

## 実装ステップ2: Crash Recovery

Recoveryの仕様は、`.kiro/specs/wal-implementation/design.md` の「カタログの扱い」の「Recovery」、「Recoveryの適用ルール」、「WALの読み込みを止める条件」、「破損したWALの扱い」を参照してください。
WALを先頭から順に読む処理は、ステップ1の `dump` とLSNの再開で作るものを、Recoveryでも使います。

- [ ] `PagedStorage` に、Recoveryを行う関連関数（`recover(base_path)` など、`self` を取らないもの）を実装する。起動時に、`catalog.json` のロードと `pages.dat` のオープンより前に呼ぶため。以下の手順とする
  - WALファイルがない、または空の場合は、何も適用せずに終了する（`catalog.json` と `pages.dat` には触れない）
  - 書き戻しの前に、WAL全体を検査する。WALを先頭から順に読み、「WALの読み込みを止める条件」に当たったら、何も書き戻さず、WALも切り詰めずにエラーを返す（`design.md` の「破損したWALの扱い」参照）
  - 最後まで正常に読めた（正常なEOFに達した）場合だけ、書き戻しに進む。WALを先頭から読み、Commitレコードの位置でトランザクションを区切る（`tx_id` は使わない）
  - 読んでいるトランザクションのレコードはメモリに溜める。Commitレコードを読んだ時点で、溜めた `FullPageWrite` をLSN順に `pages.dat` へ無条件で書き戻す。`page_id` が `pages.dat` の末尾より先なら、ファイルを延ばして書く
  - `CatalogWrite` は、Commit済みのものの最後の1件だけを覚えておき、読み終えた後に `catalog.json` へ書き戻す（トランザクションごとには書かない）。Commit済みの `CatalogWrite` が1件もない場合は、`catalog.json` に触れない
  - Commitレコードに達しなかった末尾のレコード（Commitされていないトランザクションのレコード）は、適用せずに捨てる
  - `pages.dat` と `catalog.json` への書き込みの後、それぞれfsyncする
  - 読み書きのエラーが起きた場合は、エラーを返す（「Recoveryが失敗した場合」は、起動を失敗させる）
- [ ] Recoveryの最後に、WALファイルを最後のCommitレコードの終端で切り詰める（`File::set_len`）。切り捨てるのは、Commitされていないトランザクションの正しいレコードだけである（破損したレコードがある場合は、この手前でエラーになり、切り詰めない）。残すと、起動後に追記された新しいトランザクションのレコードと連続し、次回のRecoveryが「前のCommitから次のCommitまで」を1つのトランザクションとみなして、Commitされていないレコードまで適用してしまうため。LSNは、切り詰め後の最後のレコードのLSN+1から再開する（WALが空なら1）
- [ ] `PagedStorage::new` の起動順序を、ディレクトリ作成 → Recovery → `catalog.json` のロード → `pages.dat` のオープン → WALのオープン、に変更する
  - Recoveryは `catalog.json` を読まない。ロードは、Recoveryが書き戻した後に行う
  - `catalog.json` がない場合は、これまでどおり空のカタログから始める
  - WALのオープンは、切り詰めの後に行う。追記位置は切り詰め後のファイル末尾とし、LSNは最後のレコードのLSN+1から採番する
- [ ] `Engine::new` の `StorageType::Paged` の分岐で、`PagedStorage::new` が失敗しても、MemStorageにフォールバックせずにエラーを返す（CsvStorageのフォールバックは変えない）。`main.rs` は、起動の失敗を、エラーを表示した上で、0以外の終了コードで終了する

### ステップ2 完了条件

クラッシュは、`PagedStorage` を閉じた後に、WAL・`pages.dat`・`catalog.json` を加工することで再現する。

- Commit後、`pages.dat` と `catalog.json` への書き込み前の状態（WALだけが最新）から起動すると、全行が正しく読めること
- 新しいページを作るINSERTの後、`pages.dat` が短い状態から起動すると、Recoveryでファイルが延び、全行が正しく読めること
- `pages.dat` のページの一部が壊れている（torn page）状態でも、Recoveryで復旧し、全行が正しく読めること
- Recoveryの後、書き戻されたページの `last_applied_lsn` が、WALの対応する `FullPageWrite` のLSNと一致し、ページのバイト列がWALの `page_data` と完全に一致すること
- `catalog.json` が壊れていても、WALに `CatalogWrite` があれば、Recoveryで復旧して起動できること
- `catalog.json` が壊れていて、WALに `CatalogWrite` がない場合は、起動が失敗し、MemStorageにフォールバックしないこと
- WALがない、または空の場合は、`catalog.json` と `pages.dat` を変更せずに起動できること
- WAL末尾に壊れたレコード（書きかけのレコード）がある状態から起動すると、起動が失敗し、`pages.dat`・`catalog.json`・WALのいずれも変更されていないこと
- WALの途中のレコードのチェックサムが一致しない状態から起動すると、起動が失敗し、`pages.dat`・`catalog.json`・WALのいずれも変更されていないこと
- Commitレコードのないトランザクションのレコードが、適用されず、WALから切り詰められること
- Commitなしのレコードが切り詰められた後に、新しいトランザクションをCommitして再起動しても、直前のCommitなしのレコードは適用されず、新しいトランザクションは正しく復旧されること
- Recoveryを2回続けて実行しても、結果が同じになること
- ステップ1で作った、`commit_changes` の致命的エラーの後に、ストレージを開き直すと、Recoveryで復旧すること
