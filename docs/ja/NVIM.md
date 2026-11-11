# Neovim アダプタ

Neovim 側の接続部分です。Neovim を `claude-ide-server` に登録して、Claude Code の `/ide` から見つけられるようにします。アダプタは、このリポジトリの `editors/nvim/` ディレクトリにあり、Lua で書かれています。

**いま動くこと:** サーバーへの接続（サーバーがいないときや、いなくなったときの再接続を含む）、ワークスペースの登録、`:cd` への追従、選択範囲の送信、ファイルや行を Claude Code にメンションすること、そして開いているエディタ・選択範囲・診断についての Claude Code のリクエストへの応答。本物の Claude Code（2.1.322）で試しました。**まだ動かないこと:** `openFile`、`checkDocumentDirty`、`saveDocument`（Issue #11）、診断の通知（#12）、差分（#13、#14）。

## 必要なもの

- Neovim 0.11 以降。CI は 0.11.7 と 0.12.6 でテストを動かし、0.11.0（0.11 系の最初のリリース）は手で試しました。Neovim 0.10 では動きません（`vim.str_utfindex` の引数が違います。それより先は試していません）。
- `PATH` の通った `claude-ide-server`（[DESIGN](DESIGN.md) の「サーバーの実行」を参照）、または最初の接続で起動する監督プロセス。誰も待ち受けていなければ、アダプタが実行ファイルを自分で起動します。

## インストール

このリポジトリのクローンにある `editors/nvim` を、お使いのプラグインマネージャのやり方で Neovim のランタイムパスに加えてから、`setup` を呼びます。

```lua
vim.opt.runtimepath:append('/path/to/claude-ide-server/editors/nvim')
require('claude_ide').setup()
```

`setup` は、Neovim が起動し終わってから接続します。`setup({ autostart = false })` なら接続せず、`:ClaudeIde start` で接続します。

## オプション

```lua
require('claude_ide').setup({
  -- The server's adapter socket. By default, where the server puts it: $XDG_RUNTIME_DIR/claude-ide-server/adapter.sock.
  socket = nil,
  -- The name that /ide shows.
  ide_name = 'Neovim',
  -- Whether this Neovim is to register. See "Main windows only".
  should_register = function() return true end,
  -- The folders of the workspace. By default, the working directory. It is asked at registration, and when :cd changes it.
  workspace_folders = function() return { vim.uv.cwd() } end,
  -- The wait between attempts to connect: it doubles, up to the maximum.
  reconnect = { initial_ms = 500, max_ms = 10000 },
  -- What to do when nobody is listening on the socket. See "How it connects".
  server = { autostart = true, command = 'claude-ide-server', args = { '--idle-timeout', '300' } },
  -- How long the server has to answer `register`.
  register_timeout_ms = 5000,
  -- How long the cursor must stay still before the selection is sent.
  selection_debounce_ms = 100,
  -- Called each time the registration has been accepted.
  on_registered = nil,
})
```

## メインウィンドウだけ

`setup` を呼んだ Neovim は、それぞれが自分のワークスペースを登録し、Claude Code は `/ide` にそれを並べます。ほかの Neovim の子（Git グラフや差分を、別の Neovim で開く構成でのそのウィンドウ）は、そこに現れないほうがよいはずです。どの Neovim がどれなのかはアダプタには分からず、設定側が `should_register` で伝えます。これは、アダプタが起動するときに1回だけ呼ばれます。

```lua
require('claude_ide').setup({
  should_register = function()
    return not vim.g.my_window_is_a_child -- however your configuration marks those
  end,
})
```

## コマンド

`:ClaudeIde status` は、アダプタが接続しているか、ワークスペースのポート、ソケット、直近の問題を表示します。`:ClaudeIde start` と `:ClaudeIde stop` は、接続を始めたり止めたりします。

## 接続のしかた

サーバーを手で起動する必要はありません。アダプタはサーバーのソケットに接続し、誰も待ち受けていないときにどうなるかは、サーバーのセットアップ次第です。

- **監督プロセスがソケットを持っている**（systemd のソケットアクティベーション。`contrib/` を参照）: 接続は成功し、最初の接続でサーバーが起動します。アダプタは、サーバーが動いていなかったことに気づきません。
- **誰も持っていない:** アダプタが `claude-ide-server` を自分で1回起動し、あとはほかのサーバーと同じように待ちます。サーバーは Neovim の子プロセスではなく、Neovim が終了しても動き続け、しばらくアダプタがいなくなると自分で終了します（`--idle-timeout`。アダプタが起動するときは5分）。同時に起動した複数の Neovim は、すべて起動を試みますが、同じソケットでサーバーは2回起動を拒否するので、1つが勝ち、ほかはそれに接続します。`server.autostart = false` でこれをやめられ、`server.command` と `server.args` で何を実行するかを決められます（実行ファイルは `PATH` から探します。切り離したプロセスの出力は行き先がないので、ログを残したいときは、自前のラッパーを使ってください）。実行ファイルが見つからなければ、ユーザーに1回だけ知らせます。起動したサーバーが、接続できるようになる前にエラーで終了した場合は、その終了コードが `:ClaudeIde status` の直近の問題に出て、コマンドラインを理解できなかったサーバー（終了コード 2 以上）はメッセージで知らせます。

どちらの場合も、アダプタはやり直し続けます。`reconnect.initial_ms` 後に、その次はその2倍後に、というように `reconnect.max_ms` まで、メッセージを出さずにやり直すので、サーバーより先に起動した Neovim がうるさくなりません。サーバーが `shutdown` と言ったとき（停止中）や接続を閉じたときも、アダプタは同じようにし、ソケットアクティベーションならサーバーがまた起動します（完全に止めるには、ソケットユニットも止めてください）。`register` を拒否するサーバー（知らないプロトコルのバージョンなど）は、まだいないサーバーとは違います。やり直しても何も変わらないので、アダプタはユーザーに、1回だけ知らせます。

登録のたびに、現在の選択範囲が1回送られます。サーバーと、それに接続する Claude Code が、いまユーザーが見ているところから始められるようにするためです。

## 選択範囲

`selectionChanged` は、カーソルが `selection_debounce_ms` のあいだ止まっていて、直前に送ったものと違う選択範囲のときに送られます。選択範囲は、ビジュアル選択か、カーソル位置の空の範囲です。位置は `{ line, character }` で、`line` は 0 始まり、`character` は UTF-16 コードユニット単位（Neovim はバイトで数えます）、範囲の終端は含みません。ファイルでないバッファ（ターミナル、ヘルプ、名前のないバッファ）には、選択範囲はありません。

## Claude Code が頼めること

アダプタは、サーバーの次のリクエストに答えます（`register` で宣言する `methods`。設定の `methods` オプションで、置き換えたり、足したりできます）。

- `getOpenEditors`: 一覧にあるバッファのうち、ファイルであるもの。アクティブなのは、現在のウィンドウのファイルです（ターミナルやヘルプにフォーカスがあるときは、直前にいたファイル）。名前のないバッファ、ターミナル、ヘルプは、Claude にとってのエディタではありません。ファイルの言語は `filetype` で、まだ読み込まれていないバッファは、ファイル名から決まるものです。いくつかの filetype は、Claude Code が使う言語 ID に直します（`sh` は `shellscript`、filetype なしは `plaintext`）。
- `getCurrentSelection` と `getLatestSelection`: ユーザーがいるファイルの選択範囲。ファイルのない場所にフォーカスがあるあいだは、直前にあった選択範囲。
- `getDiagnostics`: `vim.diagnostic` の診断。`uri` のファイル、またはその診断を持つ開いているすべてのファイルについて。重大度は `Error`、`Warning`、`Information`、`Hint`、列は UTF-16 コードユニット単位です。

Claude Code 2.1.322 は、自分で編集する前後に `getDiagnostics` を頼み、ほかの3つは使いません。[DESIGN](DESIGN.md) の「Claude Code が自分から送るもの」を参照してください。

## Claude Code へのメンション

`:ClaudeIdeMention` は、現在のファイルを Claude Code にメンションし、`:'<,'>ClaudeIdeMention`（`:2,4ClaudeIdeMention` のような任意の範囲でも）は、その範囲の行をメンションします。Claude Code は、それをプロンプトに `@file#L2-4` として入れます。キーは割り当てていないので、好きなように割り当ててください。Lua からは `require('claude_ide').mention(first, last)` で同じことができ、行（1 始まり）を渡すか、渡さなければファイル全体になります。Claude Code に伝えられたかどうかを返します。

## テスト

```default
nvim --headless -u NONE -l editors/nvim/tests/unit.lua       # what needs no server
cargo test -p claude-ide-server --test nvim                  # a headless Neovim against the real executable
```

2つ目には、パスの通った `nvim` が要ります。なければ、テストはそう表示して成功します。`CLAUDE_IDE_REQUIRE_NVIM` が設定されている場合は除きます（CI は設定します）。
