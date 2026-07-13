> [!CAUTION]
> ## 本製品の利用は、すべて自己責任でお願いします
> **APEX//TRACE（CarLogger）は車両の安全性・故障の有無を保証する製品ではありません。** 車両、ECU、OBD-II/CAN アダプター、保存データなどに生じた損害・不具合、および本製品の表示や推定結果に基づく判断について、開発者は責任を負いません。走行中に画面を操作せず、法令・車両メーカーの指示・保証条件を確認したうえで、安全な環境で使用してください。異常を感じた場合は本製品の結果だけで判断せず、資格を持つ整備士や正規サービスへ相談してください。

<p align="center">
  <img src="docs/assets/readme-hero.png" alt="スポーツカーと車両テレメトリーを描いた APEX TRACE のヒーローイメージ" width="100%">
</p>

<h1 align="center">APEX//TRACE</h1>

<p align="center">
  <strong>愛車の声を、データで聴く。</strong><br>
  CAN / OBD-II のリアルタイム記録から、時系列分析、車両コンディションの可視化まで。<br>
  Rust × GTK4 で動く、オープンソースのデスクトップ車両ロガーです。
</p>

<p align="center">
  <a href="https://github.com/Ryokugyoku/CarLogger/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/Ryokugyoku/CarLogger?include_prereleases&style=for-the-badge&color=00b8d9"></a>
  <a href="LICENSE"><img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-5c6bc0?style=for-the-badge"></a>
  <img alt="Rust 2024" src="https://img.shields.io/badge/Rust-2024-e76f51?style=for-the-badge&logo=rust&logoColor=white">
  <img alt="GTK4" src="https://img.shields.io/badge/UI-GTK4-4a86cf?style=for-the-badge&logo=gtk&logoColor=white">
</p>

## APEX//TRACE でできること

| | 機能 | できること |
|:--:|---|---|
| ⚡ | **ライブダッシュボード** | 回転数、速度、冷却水温、スロットルなど、デコード済み CAN / OBD-II 値をリアルタイム表示 |
| 🔌 | **複数の入力方式** | ELM327/STN 系 Serial アダプターと Linux SocketCAN に対応 |
| 🧭 | **PID 探索と信号定義** | ECU が応答する Mode 01 PID を探索し、既知・未知の PID / CAN ID を整理 |
| 📈 | **テレメトリー分析** | 保存した信号を同じ時間軸へ重ね、相対比較または実値スケールでグラフ表示 |
| 🫀 | **車両ヘルススコア** | 温度・電気・空燃比・走行安定性をロバスト統計で評価し、期間ごとの傾向を可視化 |
| 🧠 | **AI コンディション** | TensorFlow ワーカーを別プロセスで実行し、統計評価と AI 推論を組み合わせて状態を表示 |
| 🗃️ | **ローカルデータ管理** | 車両や設定は SQLite、大容量の時系列ログは DuckDB に保存。車両単位で履歴を管理 |
| 🌏 | **多言語 UI** | 日本語・英語・スペイン語をアプリ内で切り替え |
| 🔄 | **安全性を意識した更新** | GitHub Releases から更新を取得し、SHA-256 を検証して安全な終了タイミングで適用 |

## 走行データが「わかる」に変わる

```text
車両
    │
    ├── ELM327 / STN (Serial)
    └── SocketCAN (Linux)
            ↓
      CAN フレーム収集 ──→ リアルタイム表示
            ↓
      PID / CAN ID デコード
            ↓
      SQLite + DuckDB
            ↓
      グラフ分析 ──→ 統計ヘルス評価 ──→ AI コンディション
```

記録処理と AI 推論は分離されています。TensorFlow 側が遅延・停止しても、CAN ロギングと GUI を巻き込まない構成です。また、AI ランタイムがない環境でも統計ベースの健康評価は利用できます。

## クイックスタート

### 1. リリース版を使う

[GitHub Releases](https://github.com/Ryokugyoku/CarLogger/releases) からお使いの OS / CPU に合うアーカイブを取得し、展開して `car-logger-gui`（Windows は `car-logger-gui.exe`）を起動してください。

配布対象は次のとおりです。

- Windows x64
- macOS Intel / Apple Silicon
- Linux x64

> [!NOTE]
> リリースの有無や対象プラットフォームは開発状況により変わります。プレビュー版には未完成の機能が含まれる場合があります。

### 2. ソースから起動する

必要なもの：安定版 Rust、GTK4、gettext。Linux では Serial 利用のため `libudev` も必要です。

<details>
<summary><strong>macOS</strong></summary>

```bash
brew install gtk4 gettext
cargo run --release
```

</details>

<details>
<summary><strong>Ubuntu / Debian</strong></summary>

```bash
sudo apt update
sudo apt install libgtk-4-dev libudev-dev gettext
cargo run --release
```

</details>

<details>
<summary><strong>Windows（MSYS2 / MinGW）</strong></summary>

```bash
pacman -S mingw-w64-x86_64-gtk4 mingw-w64-x86_64-gettext mingw-w64-x86_64-pkgconf
rustup target add x86_64-pc-windows-gnu
cargo run --release --target x86_64-pc-windows-gnu
```

MSYS2 の `mingw64/bin` を `PATH` に追加し、必要に応じて `PKG_CONFIG` と `PKG_CONFIG_PATH` を設定してください。

</details>

## 車両へ接続する

1. 停車し、安全を確保した状態で OBD-II / CAN アダプターを接続します。
2. APEX//TRACE を起動し、検出されたインターフェースを選択します。
3. 車両を登録または選択し、接続を開始します。
4. ライブダッシュボードで値を確認し、ログを蓄積します。
5. 「Data Charts」「Vehicle health」で履歴と傾向を確認します。

ELM327/STN 系アダプターでは一般的なボーレートを順に試し、対応 PID を探索します。SocketCAN は Linux でのみ利用できます。車両やアダプターによって取得できる信号、更新頻度、応答形式は異なります。

> [!WARNING]
> 低品質な互換アダプター、車種固有 CAN、誤った配線や設定は通信不良や車両側の不具合につながる可能性があります。接続前に機器の仕様を確認し、運転操作を妨げないよう配線してください。

## AI 機能を有効にする（任意）

AI 機能のローカル開発には **Python 3.11** が必要です。TensorFlow は容量が大きいため、専用の仮想環境を推奨します。

```bash
python3.11 -m venv python/ai_worker/.venv
source python/ai_worker/.venv/bin/activate
python -m pip install -e './python/ai_worker'
cargo run --release
```

Windows では有効化コマンドを `python/ai_worker/.venv/Scripts/activate` に読み替えてください。自動検出できない場合は以下を設定できます。

```bash
export CAR_LOGGER_AI_PYTHON=/path/to/python
export CAR_LOGGER_AI_WORKER_SCRIPT=/path/to/run_worker.py
```

AI のスコアは診断結果ではありません。学習データの量・質・走行条件によって結果が変わるため、統計スコアや実車の状態と合わせて参考情報として扱ってください。詳しい受け入れ条件は [AI Release & Acceptance](docs/AI_RELEASE_AND_ACCEPTANCE.md) を参照してください。

## データ保存

デバッグビルドでは、ワークスペース直下に次のファイルが作成されます。

| ファイル | 内容 |
|---|---|
| `car-logger.db` | 車両、設定、接続履歴、信号定義などの SQLite データ |
| `car-logger.duckdb` | CAN フレーム、PID サンプル、集計元などの時系列ログ |

保存先は `CAR_LOGGER_DB_PATH` で変更できます。大切なログはアプリ停止後に両ファイルをセットでバックアップしてください。

```bash
export CAR_LOGGER_DB_PATH="$HOME/CarLogger/car-logger.db"
```

## 開発

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Python AI worker
python -m pip install -e './python/ai_worker[dev]'
ruff check python/ai_worker
ruff format --check python/ai_worker
pytest python/ai_worker
```

ワークスペースは責務ごとに分割されています。

| パッケージ | 役割 |
|---|---|
| `apps/car-logger-gui` | GTK4 デスクトップ UI とバックグラウンド処理の組み立て |
| `crates/domain` | 車両、CAN フレーム、信号定義などの共通データ型 |
| `crates/application` | 接続、PID 探索、リアルタイム処理などのユースケース |
| `crates/transport` | Serial / SocketCAN / Replay 入力 |
| `crates/storage` | SQLite / DuckDB 永続化 |
| `crates/health` | 統計ヘルス評価と AI 特徴量 |
| `crates/ai-worker` | Rust と Python/TensorFlow ワーカー間のプロセス管理 |

Issue や Pull Request を歓迎します。不具合報告には OS、アダプター、接続方式、再現手順を含め、VIN や位置情報などの個人情報・車両固有情報を公開しないようご注意ください。

## ライセンス

このプロジェクトは [GNU Affero General Public License v3.0 only](LICENSE) の下で公開されています。配布物に含まれる第三者コンポーネントについては [Third-party licenses](distribution/THIRD_PARTY_LICENSES.md) を参照してください。

---

<p align="center">
  <strong>LOG THE DRIVE. UNDERSTAND THE MACHINE.</strong><br>
  APEX//TRACE — built for drivers who love data.
</p>
