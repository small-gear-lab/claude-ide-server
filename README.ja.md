# claude-ide-server

<a href="README.md">English</a>

[Claude Code](https://code.claude.com/docs/en/overview) の `/ide` プロトコルのための、**非公式の** IDE 連携サーバーです。Anthropic とは無関係で、Anthropic が認めたものでもありません。

このプロトコルは、Anthropic によって文書化されていません。このプロジェクトが頼っているのは、既存の連携（VS Code の拡張と JetBrains のプラグイン）の挙動の観察と、[coder/claudecode.nvim](https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md) の公開されている解説です。Claude Code が変わると、壊れることがあります。

> **状態: 開発の初期段階です。** サーバーはプロトコルの全体を実装しています。ハンドシェイク、VS Code 拡張が持つツールのうち端末のエディタに意味のあるすべて（ファイルを開く、選択範囲、診断、承認と拒否のある差分）、そして通知です。実際の Claude Code（2.1.322）と Neovim アダプターで、Linux 上で試しました。実行ファイル（`claude-ide-server`）として、Linux（systemd のソケットアクティベーション）、macOS（launchd）、Windows（名前付きパイプと、タスクスケジューラのタスク）で動きます。macOS と Windows は CI で試していて、使っているのは Claude Code の代役で、本物の Claude Code ではありません。未対応は、WSL 上の Claude Code と Windows 側のエディタの組み合わせ（#24）と、crates.io への公開（#16）です。実行ファイルとアダプターは [Releases](https://github.com/small-gear-lab/claude-ide-server/releases) のページにあります。

## これは何か

Claude Code は、MCP の変種（JSON-RPC 2.0）を話す WebSocket で、IDE につながります。IDE は `~/.claude/ide/` にロックファイルを書き、Claude Code が、それを見つけてつなぎます。このプロジェクトは、その接続の IDE 側で、2 つの部分に分かれます。

```default
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix ソケット、または名前付きパイプ)── エディタのアダプター
```

- **サーバー** は、Claude Code のプロトコルを話します。WebSocket、ロックファイルとそのトークン、MCP のツールと通知です。特定のエディタについては、何も知りません。
- **エディタのアダプター** は、サーバーにつないで、自分がどのワークスペースを持っているかを伝え、サーバーの要求（ファイルを開く、選択範囲を返す、差分を出す）に、JSON-RPC で答えます。アダプターが切れると、サーバーは、そのワークスペースとロックファイルを手放すので、エディタが落ちても、何も残りません。

最初のアダプターは Neovim 用で、Lua で書かれています（`editors/nvim`）。

## インストール

1. [Releases](https://github.com/small-gear-lab/claude-ide-server/releases) のページから、使うシステム向けのサーバーのアーカイブ（`claude-ide-server-<version>-<target>`）を取り、`claude-ide-server` を `PATH` の通った場所に置きます。Linux の `-musl` は、どのディストリビューションでも動く静的な 1 つのファイルで、`-gnu` は動的リンクで、glibc 2.34 以降が要ります。`SHA256SUMS` と照らし合わせて確かめてください（`gh attestation verify <file> --repo small-gear-lab/claude-ide-server` でも確かめられます。`gh` 2.49 以降が必要です）。
2. `claude-ide-nvim-<version>.zip` を取って展開し、そのディレクトリを Neovim のランタイムパスに加えて、`setup` を呼びます。[docs/NVIM.md](docs/ja/NVIM.md) を参照してください。
3. Neovim が開いているディレクトリで `claude` を起動し、`/ide` で Neovim を選びます。

自分でビルドするときは、このリポジトリのクローンで `cargo install --path crates/server --locked` を実行すると、実行ファイルが `~/.cargo/bin` に置かれます。アダプターは、クローンの `editors/nvim` ディレクトリです。

## ドキュメント

- [docs/DESIGN.md](docs/ja/DESIGN.md): 設計、サーバーとエディタのアダプターの間のプロトコル、マイルストーン。
- [docs/NVIM.md](docs/ja/NVIM.md): Neovim アダプター。インストール、オプション、できること。

ドキュメントは英語で書いてあり、日本語版（`README.ja.md`、`docs/ja/`）があります。日本語版は、`docs/locale/ja/` の gettext のカタログから、`tools/build-docs.sh` で作ります。

## 構成

| 場所                                  | 内容                                                   |
|-------------------------------------|------------------------------------------------------|
| `crates/core/`                      | サーバーのライブラリ: WebSocket、ロックファイル、認証、MCP の処理、アダプターのプロトコル |
| `crates/server/`                    | 実行ファイル `claude-ide-server`                           |
| `crates/launchd/`、`crates/windows/` | `unsafe` が必要な macOS と Windows の呼び出し。ほかから切り離してある      |
| `crates/test-support/`              | ほかの crate のテストが共有するもの: Claude Code とアダプターの代役、フィクスチャ  |
| `editors/nvim/`                     | Neovim のアダプター                                        |
| `contrib/`                          | systemd、launchd、タスクスケジューラ用のファイル                      |
| `docs/`                             | 設計、アダプターのプロトコル、Claude Code の挙動のメモ                    |
| `tools/`                            | 日本語版ドキュメントのビルド                                       |

## コントリビュート

[CONTRIBUTING.md](CONTRIBUTING.md) を見てください。コミットと日本語版ドキュメントの扱い、チェックの走らせ方、そして Claude Code についての記述に「どこで知ったか」が添えられている理由が書いてあります。

## ライセンス

次のどちらかのライセンスで使えます。

- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE)）
- MIT ライセンス（[LICENSE-MIT](LICENSE-MIT)）

どちらにするかは、使う人が選べます。
