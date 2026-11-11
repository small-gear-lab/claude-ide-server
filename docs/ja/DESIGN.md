# 設計

状態: 草稿。ここに書いてあることは、まだ何も実装されていません。Claude Code の挙動についての記述は、既存の連携の観察と、公開されている解説に基づいています。Claude Code 本体で確かめていないものには **（仮定）** と付けてあります。

## 目標

- Claude Code のプロトコルを話す部分と、エディタを操作する部分を分けて、Claude Code の `/ide` をどのエディタにもつなげられるようにします。
- サーバーは、特定のエディタについて何も知らないままにします。エディタ側のアダプターは、サーバーと JSON-RPC で話す小さなプログラム（かプラグイン）です。
- 1 つのサーバープロセスで、複数のワークスペースを扱います。ワークスペースごとに、ロックファイルと WebSocket のポートを持ちます。
- 後始末をします。エディタがいなくなったら、そのロックファイルとポートも消えます。

## やらないこと

- Claude Code の CLI や、その端末を起動・管理すること。CLI は、利用者が動かした場所で動きます。`/ide` は、ロックファイルを通してサーバーを見つけます。
- Windows。アダプターのソケットは Unix ドメインソケットです。名前付きパイプの経路は、あとから足せます。
- 公式の連携の置き換え。公式の連携がないエディタのためのものです。
- すべてのワークスペースを代表する、ハブの入口。検討して、見送りました。CLI は、ふつう、エディタで開いているプロジェクトの中で動きます。同じディレクトリに合う入口が 2 つになると、`/ide` が迷うからです。

## 概要

```default
Claude Code ──WebSocket (MCP)──▶ claude-ide-server ◀──JSON-RPC (Unix socket)── editor adapter
   (1 per CLI)                    (1 process)                                    (1 per workspace)
```

サーバーには、2 つの顔があります。

- **Claude 側。** 登録されたワークスペースごとに、ループバックの WebSocket ポートで待ち受け、ロックファイルを書き、トークンで CLI を認証して、MCP（JSON-RPC 2.0）を話します。`initialize`、`tools/list`、`tools/call` に答え、通知を送ります。
- **アダプター側。** 1 つの Unix ソケットで待ち受けます。アダプターが接続して、ワークスペースを登録し、通知（選択範囲の変更、診断の変更）を送り、サーバーの要求（ファイルを開く、開いているエディタを返す、差分を出す）に答えます。

## Claude 側

既存の連携を観察して分かっていること:

- IDE は、10000〜65535 の乱数のポートで待ち受けます。バインドするのは `127.0.0.1` だけです。
- `~/.claude/ide/`（か `$CLAUDE_CONFIG_DIR/ide/`）に `<port>.lock` を書きます。中身は `pid`、`workspaceFolders`、`ideName`、`transport`（`"ws"`）、`authToken`、（JetBrains では）`runningInWindows` です。ファイルのモードは 0600、ディレクトリは 0700 です。
- CLI は、ヘッダー `x-claude-code-ide-authorization: <authToken>` を付けて、WebSocket でつなぎます。一致しなければ、閉じます（コード 1008）。
- メッセージは JSON-RPC 2.0 で、MCP 2025-03-26 に基づいています。
- CLI は、`initialize` を送り、続けて、自分の pid を付けた通知 `ide_connected` を送ります。

**（仮定）** `/ide` は、`workspaceFolders` に CLI の作業ディレクトリを含むロックファイルを一覧にして、`pid` が生きているかを確かめます。サーバーがアダプターの pid をロックファイルに書くのは、そのためです。

### Claude に公開するツール

ツールの集合は、VS Code の拡張が登録しているものです。サーバーが公開するのは、アダプターが登録のときに宣言したメソッド（下の `register` を参照）に裏付けられたツールだけです。JetBrains のプラグインが、VS Code の拡張より小さい集合を公開しているのと同じ考え方です。

| MCP のツール              | アダプターのメソッド            | 備考                                     |
|-----------------------|-----------------------|----------------------------------------|
| `openFile`            | `openFile`            | `startText` / `endText` で、範囲を選ぶこともできます |
| `openDiff`            | `openDiff`            | 利用者が承認か拒否をするまで待ちます                     |
| `close_tab`           | `closeDiff`           | `tab_name`（`openDiff` に渡したもの）で指定します    |
| `closeAllDiffTabs`    | `closeAllDiffs`       | `CLOSED_<n>_DIFF_TABS` と答えます           |
| `getOpenEditors`      | `getOpenEditors`      |                                        |
| `getCurrentSelection` | `getCurrentSelection` |                                        |
| `getLatestSelection`  | `getLatestSelection`  | 最後の選択範囲です。エディタのフォーカスが外れていても返します        |
| `getWorkspaceFolders` | （サーバーが答えます）           | 登録の内容から返します                            |
| `getDiagnostics`      | `getDiagnostics`      | 1 つの URI か、すべて                         |
| `checkDocumentDirty`  | `checkDocumentDirty`  |                                        |
| `saveDocument`        | `saveDocument`        |                                        |

`executeCode`（Jupyter だけ）は、対象外です。

### Claude に送る通知

| 通知                    | いつ                                                           |
|-----------------------|--------------------------------------------------------------|
| `selection_changed`   | アダプターが、新しい選択範囲を知らせたとき                                        |
| `at_mentioned`        | 利用者が、選択範囲かファイルを、参照として送ったとき（`filePath`、`lineStart`、`lineEnd`） |
| `diagnostics_changed` | アダプターが、診断の変更を知らせたとき（`{ "uris": [...] }`）                     |

`diagnostics_changed` を送る連携はいくつかありますが、送らない第三者の実装もあります。広く使えるものに、このプロジェクトが最初に足す機能です。

### 1 つのワークスペースに、複数の CLI

WebSocket の接続は、1 本ごとに別のクライアントです。サーバーは、1 つのワークスペースで、複数を受け付けます（VS Code の拡張は、最新の 1 本だけを残します）。差分は、それを開いた接続のものです。その接続が閉じたとき、または、いなくなったときに、閉じます。

## アダプター側

### 通信の経路

- `$XDG_RUNTIME_DIR/claude-ide-server/adapter.sock` の Unix ドメインソケットです。置き場所のディレクトリはモード 0700、ソケットのファイルはモード 0600 です。
- LSP と同じ、`Content-Length` による区切りの JSON-RPC 2.0 です。LSP のクライアントを持つエディタは、その通信の部分を使い回せます。Neovim 0.12 で確かめました。`vim.lsp.rpc.connect(path)` は Unix ソケットにつなげます。アダプターは、要求と通知を送り、サーバーの要求に答えられます（`server_request` のディスパッチャー）。サーバーが接続を閉じると、`on_exit` が呼ばれ、`is_closing()` が true になります。ソケットがないときは、エラーにならず、Neovim がメッセージを出すだけで、`on_exit` も来ません。そのため、アダプターは、つなぐ前に、ソケットがあるかを確かめます。
- どちらの側からも、要求と通知を送ります。接続がそのままワークスペースです。接続が閉じたら、サーバーは、そのワークスペースの WebSocket のクライアントとポートを閉じて、ロックファイルを消します。

### メッセージ

アダプター → サーバー:

| メッセージ                     | 種類   | 引数                                                                                   |
|---------------------------|------|--------------------------------------------------------------------------------------|
| `register`                | 要求   | `ideName`、`pid`、`workspaceFolders`、`methods`（実装しているサーバー→アダプターのメソッド）→ `{ "port": n }` |
| `workspaceFoldersChanged` | 通知   | `workspaceFolders`（ロックファイルを書き直します）                                                   |
| `selectionChanged`        | 通知   | `filePath`、`text`、`selection`                                                        |
| `diagnosticsChanged`      | 通知   | `uris`                                                                               |
| `atMention`               | 通知   | `filePath`、`lineStart`、`lineEnd`                                                     |

サーバー → アダプター:

| メッセージ                 | 種類   | 引数 → 結果                                                                                                                |
|-----------------------|------|------------------------------------------------------------------------------------------------------------------------|
| `openFile`            | 要求   | `filePath`、`preview`、`startText`、`endText`、`selectToEndOfLine`、`makeFrontmost` → `{ filePath, languageId, lineCount }` |
| `getOpenEditors`      | 要求   | → `{ tabs: [{ uri, isActive, label, languageId, isDirty }] }`                                                          |
| `getCurrentSelection` | 要求   | → 選択範囲、または `{ success: false }`                                                                                        |
| `getLatestSelection`  | 要求   | → 選択範囲、または `{ success: false }`                                                                                        |
| `getDiagnostics`      | 要求   | `uri?` → `[{ uri, diagnostics: [{ message, severity, range, source }] }]`                                              |
| `checkDocumentDirty`  | 要求   | `filePath` → `{ isDirty, isUntitled }`                                                                                 |
| `saveDocument`        | 要求   | `filePath` → `{ saved }`                                                                                               |
| `openDiff`            | 要求   | `oldFilePath`、`newFilePath`、`newFileContents`、`tabName` → `"saved"` か `"rejected"`。利用者が決めたときに答えます                      |
| `closeDiff`           | 要求   | `tabName`                                                                                                              |
| `closeAllDiffs`       | 要求   | → `{ closed: n }`                                                                                                      |
| `shutdown`            | 通知   | サーバーが止まるとき                                                                                                             |

選択範囲は `{ start: { line, character }, end: { line, character }, isEmpty }` です。`line` は 0 始まり、`character` は UTF-16 のコード単位（LSP の決まり）、`end` は含みません。エディタ自身の単位からの変換は、アダプターが行います。**（仮定）** CLI は、この値から行数を数えるだけなので、`character` を間違えても、Claude に見えるものは変わりません。

`openDiff` が `"saved"` を返したとき、アダプターは、`newFileContents` を `newFilePath` に書き終えています。サーバーは、Claude に `FILE_SAVED` というテキストで答えます。`"rejected"` のときは、`DIFF_REJECTED` です。

### バージョン

`register` は、`protocolVersion` を持ちます。サーバーは、知らないメジャーバージョンのアダプターを断ります。メソッドは、メジャーバージョンを上げずに足せます。アダプターは、実装しているものを `methods` に並べます。

## 起動から終了までの流れ

1. サーバーが起動して、アダプターのソケットで待ち受けます。
2. アダプターが接続して、`register` を送ります。
3. サーバーは、`127.0.0.1` の空いているポートを選び、OS の暗号論的乱数から 128 ビットのトークンを作り、ロックファイルを原子的に書き（一時的な名前で書いてから、名前を変える）、ポートを返します。
4. CLI が、ロックファイルを見つけて、つないで、認証します。選択範囲と診断は、CLI へ流れます。CLI のツール呼び出しは、アダプターへの要求になります。
5. アダプターが切れる（か、落ちる）と、サーバーは、そのワークスペースの接続を閉じ、ロックファイルを消し、保留中の要求を失敗にします。
6. 終了するとき、サーバーは、アダプターに `shutdown` を送り、自分が書いたロックファイルをすべて消します。

サーバー自身が強制終了されると、ロックファイルが残ります。サーバーは、自分が書いたロックファイルを、実行時のディレクトリに記録しておき、次の起動で、まだ残っているものを消します。

## セキュリティ

- 待ち受けは、すべて `127.0.0.1` です。アダプターのソケットは、モード 0700 のディレクトリの中にあり、ソケット自身も 0600 です。
- トークンは、WebSocket へのアップグレードのたびに確かめます。Web ページは、そのヘッダーを付けられないので、ブラウザーは、そのポートと話せません。
- フレームとメッセージの大きさには、上限があります。壊れたメッセージは、その接続だけを閉じます。
- Claude からのパスは、アダプターが渡したとおりに使います。サーバー自身は、プロジェクトのファイルを読み書きしません。差分が承認されたあとの、ファイルの書き込みは、アダプターの仕事です。

## テスト

- 偽の Claude クライアント（WebSocket）と、偽のアダプター（Unix ソケット）を、同じテストのプロセスで動かします。テストは、握手、認証、すべてのツール、通知、差分、切断、後始末を扱います。
- ツールの結果の期待する形は、上に書いた、観察した挙動から取り、フィクスチャとして書き残します。
- アダプターごとに、自分のテストを持ちます。Neovim のものは、ヘッドレスの Neovim を、本物のサーバーにつないで動かします。

## マイルストーン

1. **M0、サーバーの骨組み。** WebSocket サーバー、ロックファイル、トークン、`initialize`、`tools/list`。偽のアダプター。
2. **M1、読み取りだけのツール。** `getOpenEditors`、`getWorkspaceFolders`、`getCurrentSelection`、`getLatestSelection`、`getDiagnostics`。`selection_changed`、`at_mentioned`。
3. **M2、Neovim のアダプター。** Lua で、`vim.lsp.rpc` の上に作ります。登録するのは、メインのウィンドウだけです。
4. **M3、操作。** `openFile`、`checkDocumentDirty`、`saveDocument`、`diagnostics_changed`。
5. **M4、差分。** `openDiff`、`close_tab`、`closeAllDiffTabs`。それと、Neovim のアダプターの、承認と拒否の流れ。

## 未解決の問い

- CLI が、複数のロックファイルからどれを選ぶか。`pid` を何に使うか（上で仮定したもの）。
- サーバーを、必要になったときに起動するか（systemd のソケットアクティベーション）、動かしたままにするか。
- サーバーがいないときに、アダプターがどうするか。再試行するか、どのくらいの間か。
- シンボリックリンクを通るパスのワークスペースの、ロックファイルを受け付けるか。パスをどう比べるか。
- Windows 向けの、名前付きパイプの経路。
