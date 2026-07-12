# 自動更新・リリース運用

## 実装方式

APEX//TRACE は standalone GTK アプリの構成を維持し、`self_update 0.44` の GitHub Releases取得、ZIP展開、`self-replace` によるOS別の安全な実行ファイル置換を利用する。独自に実行中バイナリを上書きしない。Tauri updater は署名必須・各OSのインストーラー対応に優れるがTauriランタイムへの移行が必要で不採用、Velopackは包括的だが現時点でGTK Rustから使う安定した直接APIがないため不採用とした。

更新確認は起動時、24時間ごと、設定画面のボタンで行う。正式版はGitHubの `releases/latest` から正式版だけを取得する。Preview版はRelease一覧から現在より新しい`-preview.N`だけをSemVer順で選び、正式版やbetaなど別チャネルへは移動しない。自動確認の失敗・更新なしは操作を妨げず、手動確認だけ結果を表示する。公開Releaseには対象ZIPと同名の `.sha256` が必須で、不一致なら適用しない。

記録接続中は「更新待機中」とし、切断後に画面名とウィンドウサイズを原子的に保存する。5秒の取消不能表示後、`self-replace`で更新し新版を起動する。入力フォームの機密的な一時内容は永続化しない。保存・復元失敗は起動を妨げない。

## OS別配布

| OS | 成果物 | インストール・更新 | runner | 署名 |
|---|---|---|---|---|
| Windows x64 | portable `.exe`、更新ZIP | 展開またはexe配置。終了時にself-replace | `windows-2022` | Authenticode（任意Secrets） |
| macOS Intel | `.tar.gz`、更新ZIP | 展開して配置。bundle内実行ファイルを置換 | `macos-15-intel` | Developer ID + hardened runtime、notarytool（任意Secrets） |
| macOS ARM64 | `.tar.gz`、更新ZIP | Intelと同じ | `macos-14` | 同上 |
| Linux x64 | AppImage、更新ZIP | 実行権限を付与。AppImage配布、実行ファイル更新 | `ubuntu-22.04` | SHA-256（OSコード署名なし） |

macOSは依存ランタイムと署名対象がアーキテクチャ別であり、障害の切り分けと成果物選択を明確にするためUniversal Binaryに統合しない。

WindowsとmacOSで未署名の成果物は検証用に生成できるが、SmartScreenまたはGatekeeperの警告が出る。一般配布ではWindows Authenticode署名、macOS Developer ID署名とnotarizationが実質必須である。

## Previewリリース

現在のリリースはPreview扱いとし、`staging`ブランチへ配布対象の変更が反映された場合だけ実行する。手動実行も`staging`を選択した場合に限る。タグとアプリバージョンは `vX.Y.Z-preview.<GitHub run番号>`、GitHub Releaseはprereleaseとして公開する。Preview版アプリには新しいPreview版を配信し、正式版アプリには配信しない。

Environment `preview-release` を作り、Required reviewersを設定する。ビルド4件と品質検査がすべて成功した場合だけEnvironment承認へ進み、承認後にタグとPreview Releaseを同時に作る。拒否時はタグもReleaseも作られない。Environment承認が利用できないプランでは、`approve-and-publish-preview` jobを `workflow_dispatch` 専用の別workflowへ移し、対象run IDを入力させる。

## GitHub設定とSecrets

任意Secrets:

- `WINDOWS_CERTIFICATE_BASE64`, `WINDOWS_CERTIFICATE_PASSWORD`
- `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
- notarization用 `APPLE_ID`, `APPLE_APP_PASSWORD`, `APPLE_TEAM_ID`

Secretsが空ならワークフローは署名無効を明示して続行する。秘密鍵はリポジトリへ入れない。Apple証明書はbase64化した`.p12`、Windows証明書はbase64化した`.pfx`を登録する。

## リリース手順

1. 初回のみ `preview-release` Environmentとreviewer、必要なら上記Secretsを登録する。
2. 配布物に影響する変更を`staging`へmergeする。文書だけの変更はpaths filterにより起動しない。
3. CIは最新の正式Release（存在しなければworkspaceの`0.1.0`）の次patchをPreview基準版とし、run番号を付けてrunner内だけでCargo versionを注入する。
4. ActionsのEnvironment承認画面で成果を確認し承認する。
5. 公開jobが日本語ノート、`vX.Y.Z-preview.N`タグ、全成果物、SHA-256をprereleaseへ公開する。

`concurrency`、既存タグ検査、既存Release検査により並行・再実行時の二重公開を防ぐ。リリース専用コミットは`staging`へ作らない。

## 制約と復旧

`self-replace`はWindowsで置換時の旧実行ファイルを扱うが、全OS共通の起動ヘルスチェック付き自動ロールバックは提供しない。危険な独自ロールバックは実装せず、更新失敗時は現プロセスを継続する。新版が起動不能の場合はGitHub Releaseに保持される旧版を再配置する。macOS app bundle全体やPython runtimeの変更はZIPによる実行ファイル更新だけでは反映できないため、その種の更新では配布パッケージの再インストールが必要である。

現状の「重要処理」はリアルタイム記録接続である。将来import/export/syncを追加する場合は同じbusyフラグへ処理スコープを登録すること。ダウンロード進捗はContent-Lengthを公開しないAPIにも対応するため、UIの数値は取得済みMiBを上限99として示す概算である。

## Preview開始時のデータベース基準

SQLiteとDuckDBはPreview開始時点の完成スキーマを新規作成する。旧開発版向けの起動時`ALTER TABLE`、データ補正`UPDATE`、テーブル再構築は持たない。Preview以前の開発用DBは互換対象外のため、必要なデータを退避したうえで`car-logger.db`と`car-logger.duckdb`を削除し、アプリに再作成させる。Preview公開後にスキーマ変更が必要になった場合は、この基準スキーマを変更せず、バージョン管理された新規マイグレーションとして追加する。
