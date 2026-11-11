# claude-ide-server

[English](README.md)

[Claude Code](https://code.claude.com/docs/en/overview) の `/ide` プロトコルのための、**非公式の** IDE 連携サーバーです。Anthropic とは無関係で、Anthropic が認めたものでもありません。

このプロトコルは、Anthropic によって文書化されていません。このプロジェクトが頼っているのは、既存の連携（VS Code の拡張と JetBrains のプラグイン）の挙動の観察と、[coder/claudecode.nvim](https://github.com/coder/claudecode.nvim/blob/main/PROTOCOL.md) の公開されている解説です。Claude Code が変わると、壊れることがあります。

> **状況: 開発初期。** コアライブラリは本物の Claude Code と通信できます（ハンドシェイク、ツール一覧、ツール呼び出し、通知）。テストは、Claude Code とエディタの代役を使って行っています。サーバーは実行ファイル（`claude-ide-server`。`contrib/` に systemd のソケットユニットの例があります）として動きます。Neovim アダプタは接続し、ワークスペースを登録し、選択範囲を送りますが、Claude Code からのリクエストにはまだ答えません（#20）。

## 何になるか

Claude Code は、MCP の変種（JSON-RPC 2.0）を話す WebSocket で、IDE につながります。IDE は `~/.claude/ide/` にロックファイルを書き、Claude Code が、それを見つけてつなぎます。このプロジェクトは、その接続の IDE 側で、2 つの部分に分かれます。

```default
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix ソケット)── エディタのアダプター
```

- **サーバー** は、Claude Code のプロトコルを話します。WebSocket、ロックファイルとそのトークン、MCP のツールと通知です。特定のエディタについては、何も知りません。
- **エディタのアダプター** は、サーバーにつないで、自分がどのワークスペースを持っているかを伝え、サーバーの要求（ファイルを開く、選択範囲を返す、差分を出す）に、JSON-RPC で答えます。アダプターが切れると、サーバーは、そのワークスペースとロックファイルを手放すので、エディタが落ちても、何も残りません。

最初のアダプターは、Neovim 用で、Lua で書きます。

## ドキュメント

- [docs/DESIGN.md](docs/ja/DESIGN.md): 設計、サーバーとエディタのアダプターの間のプロトコル、マイルストーン。
- [docs/NVIM.md](docs/ja/NVIM.md): Neovim アダプタ。インストール、オプション、できること。

ドキュメントは英語で書いてあり、日本語版（`README.ja.md`、`docs/ja/`）があります。日本語版は、`docs/locale/ja/` の gettext のカタログから、`tools/build-docs.sh` で作ります。

## 構成（予定）

| 場所              | 内容                                 |
|-----------------|------------------------------------|
| `crates/core/`  | サーバー: WebSocket、ロックファイル、認証、MCP の処理 |
| `editors/nvim/` | Neovim のアダプター                      |
| `docs/`         | プロトコルのメモと、設計                       |

## ライセンス

次のどちらかのライセンスで使えます。

- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE)）
- MIT ライセンス（[LICENSE-MIT](LICENSE-MIT)）

どちらにするかは、使う人が選べます。
