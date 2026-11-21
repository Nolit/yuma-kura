# Requirements Document

## Introduction

WALを導入し、コミット済みの変更をクラッシュ後も確実にページデータへ反映できるようにする。
WALはredo専用とし、ロールバックのためのundo情報は扱わない。

## Requirements

### REQ-1: WAL Append on Commit

As a database engine,
I want to append all pending changes to the WAL before writing to data pages,
so that committed changes can be recovered after a crash.

Acceptance Criteria:
- commit時にPending ChangesをFIFO順でWALにappendする
- Commit recordをappendしてからfsyncを行う
- fsync完了後に実データページへ反映する

### REQ-2: Crash Recovery

As a database engine,
I want to replay unapplied WAL records after a crash,
so that committed changes are not lost.

Acceptance Criteria:
- 起動時にWALを読み込み、未適用のレコードをページに反映する
- Commit recordのないトランザクションのレコードは適用しない
- チェックサムが不正なレコードはpartial writeとみなし、それ以降を無視する

### REQ-3: Partial Write Detection

As a database engine,
I want to detect corrupted or partially written WAL records via checksum,
so that recovery does not apply incomplete data.

Acceptance Criteria:
- 各WALレコードはチェックサムを持つ
- チェックサム不正のレコードをrecovery時にスキップする

## Non-Goals

- マルチスレッド対応
- ロールバック
- MVCC
- WALのコンパクション・チェックポイント
- 1トランザクション内での複数SQL（現時点では1トランザクション = 1 SQLとする）

## Glossary

- **WAL**: Write-Ahead Log。データページ変更前にredo情報を書くログファイル
- **LSN**: Log Sequence Number。WALレコードに割り当てる単調増加のID
- **Pending Changes**: commit前の未確定変更をメモリ上に保持したもの
- **TxId**: トランザクションID
