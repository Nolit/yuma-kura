# Requirements Document

## Introduction

WALを導入し、コミット済みの変更をクラッシュ後も確実にページデータへ反映できるようにする。
WALはredo専用とし、undo情報は扱わない。SQLが失敗した場合は、Commit前の未確定変更（Pending Changesと作業用カタログ）を破棄することで、元の状態に戻す。

## Requirements

### REQ-1: WAL Append on Commit

As a database engine,
I want to append all pending changes to the WAL before writing to data pages,
so that committed changes can be recovered after a crash.

Acceptance Criteria:
- commit時にPending ChangesをWALにappendする。1つのトランザクションの中で、同じページの `FullPageWrite` は1件だけとする
- Commit recordをappendしてからfsyncを行う
- fsync完了後に実データページへ反映する

### REQ-2: Crash Recovery

As a database engine,
I want to replay unapplied WAL records after a crash,
so that committed changes are not lost.

Acceptance Criteria:
- 起動時にWALを読み込み、Commit済みトランザクションのレコードをページとカタログに反映する
- Commit recordのないトランザクションのレコードは適用しない
- 最後のCommit recordより後ろに残った、正しいがCommitされていないレコードは、適用せずにWALから切り詰める
- 破損したレコード（不完全なレコード、不正なヘッダ、チェックサム不一致）を検出した場合は、WALの途中か末尾かを区別せず、ページとカタログに何も書き戻さず、WALも切り詰めずに、起動を失敗させる

### REQ-3: Partial Write Detection

As a database engine,
I want to detect corrupted or partially written WAL records via checksum,
so that recovery does not apply incomplete data.

Acceptance Criteria:
- 各WALレコードはチェックサムを持つ
- チェックサム不正のレコードを検出した場合は、REQ-2の破損したレコードの基準に従い、起動を失敗させる

### REQ-4: Discard Uncommitted Changes on Failure

As a database engine,
I want to discard all uncommitted changes when a SQL statement fails before commit,
so that a failed statement never affects the committed state of the database.

Acceptance Criteria:
- Commitが確定するまで、正式なカタログとページファイルを変更しない
- commit前にSQLが失敗した場合、Pending Changesと作業用カタログを破棄する。WALには何も書かない
- 失敗の直後に、同じテーブルへのINSERTとSELECTが成功する

### REQ-5: Stop on Commit Failure

As a database engine,
I want to stop the process when a failure occurs while committing,
so that the next startup recovers a consistent state from the WAL instead of continuing with an unknown on-disk state.

Acceptance Criteria:
- Commit処理中のエラー（WALのappend、fsync、実ファイルへの書き込み）を致命的エラーとして扱い、通常のSQLエラーと区別する
- 致命的エラーが発生したら、エラー内容を出力してプロセスを終了し、以降のSQLを処理しない
- 通常のSQLエラーでは、プロセスを終了しない
- 終了後の次回起動時に、REQ-2のRecoveryで復旧する。ただし、WALの末尾に書きかけのレコードが残った場合は、REQ-2の破損したレコードの基準に従い、起動を失敗させる

## Correctness Properties

実装がどの状態でも常に成り立つべき不変条件。テストはこれらを確認する。

- **CP-1 Write-Ahead**: Commitレコードのfsyncが完了する前に、`pages.dat` と `catalog.json` は変更されない（REQ-1, REQ-4）
- **CP-2 Durability**: Commitレコードのfsyncが完了したトランザクションの変更は、その後クラッシュしても、Recovery後にすべてページとカタログに反映されている（REQ-1, REQ-2）
- **CP-3 Atomicity**: Commitレコードのないトランザクションのレコードは、Recoveryで一度も適用されない。1つのトランザクションの変更は、すべて反映されるか、まったく反映されないかのどちらかである（REQ-2, REQ-4）
- **CP-4 Idempotency**: Recoveryを何度続けて実行しても、`pages.dat`・`catalog.json`・WALの結果は同じになる（REQ-2）
- **CP-5 LSN Monotonicity**: WAL内のレコードのLSNは、先頭から順に1ずつ増える。LSN 0はどのレコードにも割り当てられない（REQ-1）
- **CP-6 One Image per Page**: 1つのトランザクションの中で、同じページの `FullPageWrite` は1件だけである（REQ-1）

## Non-Goals

- マルチスレッド対応
- 複数プロセスからの同時アクセス（同じデータベースを開けるのは1プロセスだけとする。ロックによる排他は行わない）
- ROLLBACKコマンドと、undoログによるロールバック（SQL失敗時の未Commit変更の破棄は対象とする）
- MVCC
- WALのコンパクション・チェックポイント
- 1トランザクション内での複数SQL（現時点では1トランザクション = 1 SQLとする）

## Glossary

- **WAL**: Write-Ahead Log。データページ変更前にredo情報を書くログファイル
- **LSN**: Log Sequence Number。WALレコードに割り当てる単調増加のID
- **Pending Changes**: commit前の未確定変更をメモリ上に保持したもの
- **TxId**: トランザクションID
- **作業用カタログ**: SQL実行中の未確定のカタログ。SQLの実行開始時に正式なカタログからコピーして作る。Commit確定（WALのfsync成功）後に正式なカタログと置き換え、SQLが失敗した場合は破棄する
