# WAL Implementation Design

## WALログ

WALの実ログファイル。

### データ構造

**ヘッダ**

| フィールド | 型 | 説明 |
|---|---|---|
| `lsn` | `u64` | WALレコードに一意に割り当てられる単調増加のID。初期値は1。レコードごとに+1採番する（FullPageWrite / CatalogWrite / Commit のすべてが対象）。起動時はWALを読み、最後の有効なレコードのLSN+1から再開する（WALが空なら1）。0は「未適用」を示す値として予約する |
| `type` | `u8` | 1 = FullPageWrite、2 = CatalogWrite、3 = Commit。0は使わない |
| `len` | `u32` | WALレコード全体のサイズ。recovery時に次のrecord位置を特定するために使用 |
| `checksum` | `u32` | recordの破損やpartial writeを検知するためのチェックサム（CRC32）。対象は、`checksum` フィールドを除く、レコード全体（ヘッダとボディ）とする |

**ボディ**

```
- FullPageWrite(table_id: u32, page_id: u32, page_data: [u8; 4096])
- CatalogWrite(catalog_json: Vec<u8>)
- Commit(Empty)
```

FullPageWriteはページ全体をそのままWALに書き込む。
RecoveryはWALのページイメージをページファイルへ直接書き戻すため、
ページファイルの構造が壊れていても（torn page）正しく復旧できる。

CatalogWriteは `catalog.json` 全体のイメージ（after-image）をWALに書き込む。
長さは可変で、境界はヘッダの `len` で判別する。
詳細は「カタログの扱い」を参照。

### ライフサイクル

- Commit時、Pending Changesを元に、`page_id` 順にappendされる。1つのトランザクションの中で同じページは1回しか現れない
- トランザクションCommit時の最後にCommit recordがappendされる
- ページデータ更新前にfsyncされる
- WALログはappend専用であり、fsync後にWALレコードが更新されることはない
- 現時点ではコンパクションもせず、Commit済みのレコードが削除されることもない（将来対応予定）
- ただし、Recoveryの最後に、最後のCommitレコードより後ろ（Commitされていないトランザクションのレコード）は切り詰める（ステップ2を参照）。破損したレコードがある場合は切り詰めず、Recoveryを失敗させる（「破損したWALの扱い」を参照）

---

## Pending Changes

データ変更を伴うSQLの実行結果は、commit 前の未確定変更としてメモリ上に保持される。
この未確定変更を Pending Changes と呼ぶ。
Pending Changes は commit 時に WAL record へ変換され、永続化の対象となる。

### データ構造

Pending Changesは、Storage内部の型 `PendingChange` のリストとする。
Storageが `commit_changes` の中で、一時マップと作業用カタログから作る（「Pending Changesの生成タイミング」参照）。
Engineは保持せず、トランザクションIDによる管理も行わない（「責務分担」参照）。

PendingChangeは以下のような変更種別のenumとする。
FPW方式のため、変更後のページ全体と、変更後のカタログ全体を保持する。

```
enum PendingChange {
    PageWrite(table_id: u32, page_id: u32, page_data: [u8; 4096]),
    CatalogWrite(catalog_json: Vec<u8>),
}
```

### 責務分担

未確定の変更は、Storageが所有する。Engineは、Pending Changesの中身も、その保持も行わない。

| 層 | 持つもの | 持たないもの |
|---|---|---|
| Engine | トランザクションの境界（いつCommit / abortするか）の判断 | `PendingChange`、一時マップ、作業用カタログ |
| Storage（`PagedStorage`） | 一時マップ、作業用カタログ、そこから作る `PendingChange`、WALへの変換とappend、Commit手順 | トランザクション境界の判断 |

理由:
- `PendingChange` は、一時マップと作業用カタログから作られる派生物である。元の状態をStorageが持つ以上、派生物もStorageが所有するほうが一貫する。
- Engineが `PendingChange` を保持しても、受け取ってそのまま返すだけで、中身を参照しない。保持する理由がない。
- Engineが `PendingChange` を持つと、Storage固有の型がStorageトレイトの署名を通じてEngineに現れる。RULEBOOKの「Storage固有の構造をEngineの抽象に漏らさない」に反する。

Storageは、同時に1つの作業単位だけを持つ（マルチスレッドと複数プロセスはNon-Goals）。トランザクションを識別する番号（TxId）は、Engine・Storage・WALのどこにも持たない。同時に開けるトランザクションは1つだけで、区別する必要がないため。Engineは、SQLの成功時に `commit_changes`、Commit前の失敗時に `abort_changes` をStorageに指示する。

| 型 | 置き場所 | 役割 |
|---|---|---|
| `PendingChange` | `src/storage/pending.rs` | ページ・カタログのイメージ。Storage内部の型であり、Storageの外へは出さない |

`PendingChange` からWALレコードへの変換はStorage層（`wal.rs`）で行う。EngineはWALの存在を知らない。

### 制約

現時点では1トランザクション = 1 SQLとする。
この制約により、insert_row / update_rows / delete_rows呼び出し時に
同一トランザクション内の未コミット変更が存在しないことが保証される。
そのため、SQLの実行開始時点では、ページファイルが最新の状態であることが保証される。

ただし、1つのSQLの中では複数のページが変更されうる（例: 複数行が溢れるUPDATEで新ページが連続して作られる、
新ページを作ったあとにそのページへ行を書き込む、など）。
物理書き込みはCommitまで行わないため、SQLの途中では変更済みのページがディスクに存在しない。
そこで、SQL実行中に変更したページは、そのSQLの間だけ保持する一時マップ（page_id → ページイメージ）で管理する。

- ページを読むときは、まず一時マップを参照し、なければページファイルから読む
- ページを変更したときは、変更後のイメージを一時マップに保存する
- Commit後（またはSQLの失敗時）に一時マップを破棄する
- キャッシュや追い出しは行わない。Buffer Poolではない

カタログも、ページと同様にCommitまで正式なカタログへ反映しない。
SQL実行中のカタログの変更は、作業用カタログ（「カタログの扱い」参照）に対して行う。

- カタログを参照するとき（テーブルの検索、空きページの判定、`next_page_id` / `next_row_id` の採番など）は、SQL実行中は作業用カタログを参照する
- Commitのfsync成功後に、作業用カタログを正式なカタログに置き換える
- SQLの失敗時に、作業用カタログを一時マップと一緒に破棄する

将来、1トランザクションに複数のSQLを許す場合は、この一時マップと作業用カタログをトランザクション単位に延ばす必要がある。
その延長がBuffer Poolの導入につながる。Buffer Poolを導入する際は、メモリ上のページキャッシュを
空き場所算出の基準とする形に再設計する。

### Pending Changesの生成タイミング

- SQL実行中は、ページの変更を一時マップにだけ反映する。`PendingChange` は、`commit_changes` の中で、一時マップと作業用カタログから作る。一時マップを、SQL実行中の最新ページの正本とする
- `commit_changes` の時点で、一時マップの各ページから `PageWrite` を1件ずつ作る。同じページを何回変更しても、`PageWrite` は最終状態の1件だけになる
- 作業用カタログが変わっていれば、`CatalogWrite` を1件作る
- このため、1つのトランザクションの中で同じページは1回しか現れず、トランザクション内のレコードの順序はRecoveryの結果に影響しない。dumpの出力を安定させるため、`PageWrite` は `page_id` 順に並べる

### ライフサイクル

- SQLの実行開始時に、正式なカタログから作業用カタログを作る
- SQLの実行中は、変更を一時マップと作業用カタログにだけ溜める
- SQLが失敗した場合は、一時マップと作業用カタログを破棄する（Commit前のため、WALには何も書かない。この時点では `PendingChange` はまだ作られていない）
- Commit時（`commit_changes` の中）に、一時マップと作業用カタログからpending changesを生成し（「Pending Changesの生成タイミング」参照）、redoログのレコードへ順番に変換
- WALのfsync成功後（Commit確定後）に、作業用カタログを正式なカタログに置き換える
- 作業用カタログへの置き換え後に、pending changesを元にページデータとカタログを実ファイルへ変更
- `commit_changes` の完了時に、一時マップと、生成したpending changesを破棄
- `commit_changes` の中で失敗した場合は、破棄せずに致命的エラーとして扱う（「Commit中・Commit後の失敗時の扱い」参照）

### 非スコープ

- ROLLBACKコマンドと、undoログによるロールバック
  - SQL失敗時の未Commit変更の破棄は対象とする。作業用カタログと一時マップを破棄するだけで元の状態に戻るため、undoログは不要
- buffer poolとの統合は将来的に対応
- MVCCは将来的に対応

### バックエンドごとの差異

SQLが途中で失敗したときの動作は、バックエンドによって異なる。

| バックエンド | SQL失敗時の動作 |
|---|---|
| PagedStorage | 作業用カタログと一時マップを破棄する。実データは変更されていない |
| MemStorage / CsvStorage | 呼び出し時点で実データへ書き込むため、失敗した時点までの変更が残る（WALの対象外であり、本specでは変更しない） |

RULEBOOKの「バックエンド間で論理動作を揃える。違う場合はspecに明記する」に従い、この差異を明記する。

---

## カタログの扱い

`catalog.json` には `schema`、テーブルと `first_page_id` の対応、`next_page_id`、`last_page_id`、`next_row_id` が入っている。
`schema` などはページから復元できず、また `fs::write` による上書きはtorn writeで壊れる可能性がある。
そのため、カタログもページと同様にWALの対象とする。

### 記録方式

- カタログ全体のイメージを `CatalogWrite` として記録する（差分は管理しない）
- SQL成功時点の作業用カタログをシリアライズしたものを after-image とする

### 作業用カタログ

SQLの実行中は、Commit済みの正式なカタログを直接変更せず、そのコピー（作業用カタログ）を変更する。
Commit前に正式なカタログを変更すると、SQLが途中で失敗したときに `last_page_id` や `next_page_id` が、
ディスクに存在しない新ページを指したまま残るため。

| データ | 役割 |
|---|---|
| 正式なカタログ | Commit済みの状態。WALのfsyncが成功するまで変更しない |
| 作業用カタログ | SQL実行開始時に正式なカタログからコピーして作る。SQL実行中の未確定状態 |
| `PendingChange::CatalogWrite` | 作業用カタログのafter-image。Commit時にWALへ記録する |

流れ:

1. SQLの実行開始時に、正式なカタログから作業用カタログを作る
2. 作業用カタログと、ページの一時マップを変更する
3. SQLが失敗した場合は、作業用カタログと一時マップを破棄する。正式なカタログは変更されていないため、これだけで元の状態に戻る（undoログは不要）
4. SQLが成功した場合は、`commit_changes` の中で、一時マップと作業用カタログから `PendingChange`（`PageWrite` / `CatalogWrite`）を作り、WALにappendしてfsyncする（Commit確定）
5. fsync成功後に、作業用カタログを正式なカタログに置き換える
6. `PageWrite` と `CatalogWrite` を実ファイルに書き込む

- 作業用カタログは `PagedStorage` の内部で持つ。EngineはカタログJSONの中身も作業用カタログの存在も知らない
- 作業用カタログのコピーは、DELETEなどカタログが変わらない操作でも同じ手順で作る（操作ごとに分岐させない）
- 現時点では1トランザクション = 1 SQL のため、作業用カタログの寿命もSQL単位とする。複数SQLのトランザクションを許す場合は、一時マップと同様にトランザクション単位へ延ばす

### CatalogWriteを積むタイミング

カタログが変わる操作では、同じトランザクションのPendingChangeとして `CatalogWrite` を積む。

| 操作 | カタログの変化 | CatalogWrite |
|---|---|---|
| CREATE TABLE | `tables` に追加、`next_page_id` | 積む |
| INSERT | `next_row_id` が毎回増える。新ページ作成時は `next_page_id` / `last_page_id` | 毎回積む |
| UPDATE | 行の移動で新ページが作られたときのみ `next_page_id` / `last_page_id` | 新ページ作成時のみ積む |
| DELETE | 変化なし | 積まない |

新ページの作成と、チェーンの `next_page` の更新も、物理書き込みはせずPendingChange（`PageWrite`）として扱う。

### Recovery

- Recoveryは、起動時に `catalog.json` をロードするより前に実行する
  - `FullPageWrite` はページ全体、`CatalogWrite` はカタログ全体のイメージを持つため、WALだけでRecoveryできる。Recoveryは `catalog.json` を読まない
  - これにより、`catalog.json` がtorn writeなどで破損していても、Recoveryで復旧できる
  - 起動の順序: ディレクトリ作成 → WALのRecovery → `catalog.json` のロード → `pages.dat` のオープン
- Commit済みのトランザクションに含まれる最後の `CatalogWrite` を `catalog.json` に書き戻す
- カタログ変更とページ変更は同じCommitに属するため、どちらも適用されるか、どちらも適用されない
- WALは削除しない前提のため、最後の `CatalogWrite` が最新のカタログとなる
- WALファイルがない、または空の場合は、何も適用しない。`catalog.json` と `pages.dat` には触れない（新規のデータベースの場合など）
- WALにCommit済みの `CatalogWrite` が1件もない場合は、`catalog.json` を書き戻さない。既存の `catalog.json` をそのまま使い、ファイルがなければ空のカタログから始める
  - `catalog.json` が壊れていて、WALにも `CatalogWrite` がない場合は、復旧できない。この場合は、起動を失敗させる（メモリにフォールバックしない）
- Recoveryが失敗した場合（WALやページファイルの読み書きのエラーなど）は、起動を失敗させる。`Engine::new` は、PagedStorageの初期化に失敗しても、MemStorageにフォールバックしない。フォールバックすると、ユーザーが壊れたデータの存在に気づかないまま、メモリ上で作業を続けてしまうため
  - 「WALの読み込みを止める条件」に当たった場合（WALの破損）も、Recoveryを失敗させる。扱いは「破損したWALの扱い」に従う

---

## Commit中・Commit後の失敗時の扱い

失敗が起きた段階によって、扱いを分ける。

| 失敗が起きる段階 | 扱い |
|---|---|
| SQL実行中（`commit_changes` を呼ぶ前） | 作業用カタログと一時マップを破棄してエラーを返す。プロセスは継続する |
| `commit_changes` の中（WALのappend、fsync、実ファイルへの書き込みのいずれか） | 致命的エラーとして扱う。プロセスを終了し、次回起動時のRecoveryに委ねる |

### `commit_changes` の失敗を致命的エラーとする理由

- **WALのfsyncが失敗した場合:** Commitレコードがディスクに到達したか分からない。作業用カタログを破棄して処理を続けると、再起動後のRecoveryがそのトランザクションを適用し、メモリ上の状態と食い違う可能性がある
- **WALのappendが失敗した場合:** 書きかけのレコードがWAL末尾に残る可能性がある
- **fsync成功後に実ファイルへの書き込みが失敗した場合:** `pages.dat` のどのページが新旧どちらか分からない（torn pageの可能性がある）。メモリ上のカタログはすでに作業用カタログへ差し替え済みであり、そのまま次のSQLを受けると、壊れたページを読んだり、その上に書いたりする

Commitの確定はWALのfsync成功であり、WALがあればRecoveryで復旧できる。
そのため、プロセス内でやり直す処理は書かず、再起動してRecoveryを実行する。

### 終了の実装方法

- `commit_changes` は、致命的エラーを通常のSQLエラーと区別できる形で返す（型は実装時に決める）
- `PagedStorage` の内部で `process::exit` は呼ばない。致命的エラーをEngine経由で `main` まで伝え、`main` がエラー内容を標準エラーに出力して、0以外の終了コードでプロセスを終了する
  - Storageがプロセスの終了を決めると、層の責務が混ざる
  - 終了するコードはユニットテストで検証しにくい。致命的エラーを返す形にすれば、「致命的エラーが返る → ストレージを開き直す → Recoveryで復旧する」というテストが書ける
- WAL末尾に壊れたレコードが残った場合は、次回起動時のRecoveryが失敗する（「破損したWALの扱い」を参照）

---

## ページヘッダの拡張とRecoveryの適用ルール

### Recoveryの適用ルール

- Recoveryは、Commit済みトランザクションの `FullPageWrite` を、WALの記録順（LSN順）に無条件でページファイルへ書き戻す。ページの `last_applied_lsn` との比較によるスキップは行わない。
- 理由: ページが壊れていると、ヘッダの `last_applied_lsn` も信用できない。たとえばtorn pageでヘッダだけが新しい場合、スキップすると壊れたページが残る。
- `FullPageWrite` はページ全体のイメージなので、同じWALを何回replayしても結果は変わらない（冪等）。同じページが複数回現れても、後のレコードが上書きして最新のイメージが残る。
- 代償として、起動のたびにWAL全体をreplayする。WALを削除しない現状では許容する。
- `FullPageWrite` の `page_id` が `pages.dat` の末尾より先にある場合は、ファイルを延ばして書き込む。新しいページはCommitまでディスクに書かないため、クラッシュ後に `pages.dat` が短いのは通常の状態である
  - 例: `pages.dat` にページ0〜4があり、INSERTが新しいページ5を作る。Commit時にWALへ `FullPageWrite`（ページ5）、`FullPageWrite`（ページ4。`next_page` を5にしたもの）、`CatalogWrite`、`Commit` をappendしてfsyncした後、`pages.dat` へ書く前にクラッシュすると、WALにはページ5があるが、`pages.dat` にはページ0〜4しかない。Recoveryは、ページ5のイメージをファイルの末尾より先の位置に書き込み、ファイルを延ばす
  - 書き戻しはLSN順に行うので、`page_id` が大きいページが先に書かれ、小さいページが後で書かれる場合がある。その間は一時的にゼロ埋めの領域ができるが、同じCommitの後続のレコードが埋める
- トランザクションの区切りは、Commitレコードの位置で決める。前のCommitレコードの直後から、次のCommitレコードまでのレコードを、1つのトランザクションとする
  - WALレコードはトランザクションIDを持たない。トランザクションIDは、どこにも持たない
  - 前提: レコードはCommit時にまとめてappendされるため、1つのトランザクションのレコードはWAL上で連続して並ぶ。複数のトランザクションのレコードを交互にappendする設計にする場合は、このルールを見直す
  - 注意: Commitレコードを書けずに、Commitなしのレコードが残ったまま追記を続けると、次のトランザクションのレコードと連続して、1つのトランザクションとして扱われる。これを防ぐため、`commit_changes` の中のエラーは致命的エラーとして処理を止め、Recoveryの最後にCommitなしのレコードを切り詰める
- 最後のCommitレコードより後ろのレコードは、どのトランザクションにも属さないため、適用しない（Recoveryの最後に切り詰める）

### WALの読み込みを止める条件

Recoveryは、WALを先頭から順に読む。次のいずれかに当たったら、そのレコードを破損したレコードとみなし、Recoveryを失敗させる。扱いは「破損したWALの扱い」に従う。

- 残りのバイト数が、ヘッダのサイズより小さい（ヘッダの途中で切れている）
- `len` がヘッダのサイズより小さい、またはファイルの残りのバイト数より大きい（レコードの途中で切れている）
- `type` が `FullPageWrite` / `CatalogWrite` / `Commit` のいずれでもない
- `type` に対して `len` が正しくない（`FullPageWrite` は4121バイト、`Commit` は17バイトでない）
- チェックサムが一致しない

- 判定は上から順に行う。`len` が信用できないと、チェックサムを計算する範囲が決まらないため

### 破損したWALの扱い

- WALの途中の破損と末尾の破損は区別しない。どちらの場合も、Recoveryを失敗させ、起動を失敗させる
- 破損したWALは切り詰めない。`pages.dat` と `catalog.json` への書き戻しも行わない。調査と手作業での復旧のため、そのままの状態で残す
- WALを最後のレコードの終端まで読めた（正常なEOFに達した）場合だけ、Recoveryを成功とする
- 最後の `Commit` レコードより後ろに、正しいが `Commit` されていないレコードが残っている場合は、適用せずに切り詰める（すべてのレコードを追記した後、`Commit` レコードを追記する前にクラッシュした場合など）

制限:
- 追記の途中でクラッシュし、WALの末尾に書きかけのレコードが残ると、正常なクラッシュでも起動できなくなる。この場合は、WALの末尾を手作業で取り除く必要がある

### `last_applied_lsn`

| フィールド | 型 | 説明 |
|---|---|---|
| `last_applied_lsn` | `u64` | このページに最後に適用されたWALレコードのLSN。0はWAL未適用を示す |

- 現時点ではRecoveryでは参照しない。将来のスキップ判定用に予約する。
- Commit時、`PageWrite` ごとに `page_data` を複製し、そのレコードに採番したLSNを複製の `last_applied_lsn` に書き込む。これを「LSN設定済みページイメージ」と呼ぶ。
- チェックサムの計算、WALへの `FullPageWrite` の書き込み、`pages.dat` への書き込みには、このLSN設定済みページイメージと同じバイト列を使う。WAL用と実データ用でイメージを別々に作らない。
- 結果として、Commit後に `pages.dat` のページの `last_applied_lsn` は、WALの対応する `FullPageWrite` のLSNと一致する。
- `PendingChange` 自体は変更しない。LSNは採番時にしか決まらないため、複製への書き込みはCommit時に行う。
- `PageHeader` は現在20バイト（`_reserved` 2バイトを含む）。`last_applied_lsn`（8バイト）の追加で `PAGE_HEADER_SIZE` は28バイトになる。

### 将来対応（時間があれば）

- ページにもCRC32チェックサムを持たせ、読み込み時に検証する。
- チェックサムが正しいページに限り、`last_applied_lsn` によるスキップ判定を行う。不一致のページは無条件で適用する。
- WALレコードのLSNは、現状どおり全レコードに採番する（変更なし）。
