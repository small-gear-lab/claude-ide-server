# Neovim アダプター

Neovim 側の接続部分です。Neovim を `claude-ide-server` に登録して、Claude Code の `/ide` から見つけられるようにします。アダプターは、このリポジトリの `editors/nvim/` ディレクトリにあり、Lua で書かれています。

**いま動くこと:** プロトコルのすべて。サーバーへの接続（サーバーがいないときや、いなくなったときの再接続を含む）、ワークスペースの登録、`:cd` への追従、選択範囲と、変わった診断の送信、ファイルや行を Claude Code にメンションすること、開いているエディタ・選択範囲・診断、およびファイルを開く・確認する・保存することについての Claude Code のリクエストへの応答、そして、提案された変更を、ユーザーが承認か却下できるように表示すること。本物の Claude Code（2.1.322）で試しました。

## 必要なもの

- Neovim 0.11 以降。CI は 0.11.7 と 0.12.6 でテストを動かし、0.11.0（0.11 系の最初のリリース）は手で試しました。Neovim 0.10 では動きません（`vim.str_utfindex` の引数が違います。それより先は試していません）。
- Windows でも動きます。サーバーはそこで動き（[DESIGN](DESIGN.md) の「サーバーの実行」を参照）、アダプターは名前付きパイプで接続します。下の「Windows」を参照してください。
- `PATH` の通った `claude-ide-server`（[DESIGN](DESIGN.md) の「サーバーの実行」を参照）、または最初の接続で起動する監督プロセス。誰も待ち受けていなければ、アダプターが実行ファイルを自分で起動します。ダウンロードできるバイナリはまだありません。このリポジトリのクローンで `cargo install --path crates/server` を実行するとビルドされ、`~/.cargo/bin` に置かれます。

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
  -- The server's adapter socket. By default, where the server puts it: $XDG_RUNTIME_DIR/claude-ide-server/adapter.sock, or
  -- what $CLAUDE_IDE_SERVER_SOCKET says (the server reads it too); on Windows, the pipe that the server says.
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
  -- How long diagnostics are collected before the files that changed are reported (once, for all of them).
  diagnostics_debounce_ms = 100,
  -- How long `getDiagnostics` waits for the diagnostics of a file that was changed under Neovim. See "Diagnostics";
  -- `false` does not wait.
  diagnostics_wait = { first_ms = 750, quiet_ms = 150, cap_ms = 1500, cap_hidden_ms = 1000 },
  -- How the changes that Claude Code proposes are shown. See "Diffs"; `false` leaves them out.
  diff = { provider = 'tab', keys = {} },
  -- Called each time the registration has been accepted.
  on_registered = nil,
})
```

## メインウィンドウだけ

`setup` を呼んだ Neovim は、それぞれが自分のワークスペースを登録し、Claude Code は `/ide` にそれを並べます。ほかの Neovim の子（Git グラフや差分を、別の Neovim で開く構成でのそのウィンドウ）は、そこに現れないほうがよいはずです。どの Neovim がどれなのかはアダプターには分からず、設定側が `should_register` で伝えます。これは、アダプターが起動するときに1回だけ呼ばれます。

```lua
require('claude_ide').setup({
  should_register = function()
    return not vim.g.my_window_is_a_child -- however your configuration marks those
  end,
})
```

## Windows

アダプターは Windows で動きます。`crates/server/tests/nvim.rs` のテストを、CI の Windows ランナーで、Neovim 0.12.6 で動かしています（0.11 は、そこでは試していません）。Windows に特有のことは次のとおりです。

- **パイプ。** サーバーは、名前付きパイプ `\\.\pipe\claude-ide-server-<sid>` で待ち受けます。名前にはユーザーのセキュリティ識別子が入っています。Neovim は SID を与えてくれないので、アダプターが実行ファイルに1回尋ねます: `claude-ide-server --print-socket`（どの実行ファイルかは `server.command` が決めます。名前でも、`PATH` 上でない場所のパスでも構いません）。実行ファイルが見つからなければ、ユーザーに1回だけ知らせ、アダプターは（サーバーがいないときと同じように）あとでやり直します。`socket`、またはサーバーも読む環境変数 `CLAUDE_IDE_SERVER_SOCKET` が場所を決めていれば、何も尋ねません。
- **サーバーの起動。** ほかのシステムと同じです。`vim.system` が切り離して起動するので、この Neovim の子プロセスではなく、Neovim が終了しても動き続けます（これは試しました。アダプターが起動したサーバーは、同時に起動したほかのアダプターが見つけ、アダプターがいなくなってアイドル時間が過ぎると、自分で終了します）。
- **パス。** アダプターは、ファイルと、ワークスペースの作業ディレクトリを、実在するパスで、名前を省略せずに報告します。Neovim は、渡された名前をそのまま保つので、それが 8.3 の短い名前（`C:\Users\RUNNER~1\...`）のことがあり、Claude Code は、パスを自分の作業ディレクトリとテキストとして比べるからです。パスは Neovim が言うとおり（バックスラッシュ入り）で、URI は `file:///C:/Users/...` です。

試していないこと: Windows 上の Claude Code（テストには代役がいます）、Neovim 0.11、ASCII でないファイル名、共有のパス（`\\server\share`）。WSL は別の場合で、Issue #24 です。

## コマンド

`:ClaudeIde status` は、アダプターが接続しているか、ワークスペースのポート、ソケット、直近の問題を表示します。`:ClaudeIde start` と `:ClaudeIde stop` は、接続を始めたり止めたりします。

## 接続のしかた

サーバーを手で起動する必要はありません。アダプターはサーバーのソケットに接続し、誰も待ち受けていないときにどうなるかは、サーバーのセットアップ次第です。

- **監督プロセスがソケットを持っている**（systemd のソケットアクティベーション、または macOS の launchd。`contrib/` を参照）: 接続は成功し、最初の接続でサーバーが起動します。アダプターは、サーバーが動いていなかったことに気づきません。
- **誰も持っていない:** アダプターが `claude-ide-server` を自分で1回起動し、あとはほかのサーバーと同じように待ちます。サーバーは Neovim の子プロセスではなく、Neovim が終了しても動き続け、しばらくアダプターがいなくなると自分で終了します（`--idle-timeout`。アダプターが起動するときは5分）。同時に起動した複数の Neovim は、すべて起動を試みますが、同じソケットでサーバーは2回起動を拒否するので、1つが勝ち、ほかはそれに接続します。`server.autostart = false` でこれをやめられ、`server.command` と `server.args` で何を実行するかを決められます（実行ファイルは `PATH` から探します。切り離したプロセスの出力は行き先がないので、ログを残したいときは、自前のラッパーを使ってください）。実行ファイルが見つからなければ、ユーザーに1回だけ知らせます。起動したサーバーが、接続できるようになる前にエラーで終了した場合は、その終了コードが `:ClaudeIde status` の直近の問題に出て、コマンドラインを理解できなかったサーバー（終了コード 2 以上）はメッセージで知らせます。アダプターがサーバーを起動したあとの最初の再試行は、100 ms 後です（`reconnect.initial_ms` がそれより小さければ、その値）。サーバーは数十ミリ秒で用意できるからです。そのあとの再試行は、下の待ち時間に従います。

どちらの場合も、アダプターはやり直し続けます。`reconnect.initial_ms` 後に、その次はその2倍後に、というように `reconnect.max_ms` まで、メッセージを出さずにやり直すので、サーバーより先に起動した Neovim がうるさくなりません。サーバーが `shutdown` と言ったとき（停止中）や接続を閉じたときも、アダプターは同じようにし、ソケットアクティベーションならサーバーがまた起動します（完全に止めるには、ソケットユニットも止めてください）。`register` を拒否するサーバー（知らないプロトコルのバージョンなど）は、まだいないサーバーとは違います。やり直しても何も変わらないので、アダプターはユーザーに、1回だけ知らせます。

登録のたびに、現在の選択範囲が1回送られます。サーバーと、それに接続する Claude Code が、いまユーザーが見ているところから始められるようにするためです。

## 選択範囲

`selectionChanged` は、カーソルが `selection_debounce_ms` のあいだ止まっていて、直前に送ったものと違う選択範囲のときに送られます。選択範囲は、ビジュアル選択か、カーソル位置の空の範囲です。位置は `{ line, character }` で、`line` は 0 始まり、`character` は UTF-16 コードユニット単位（Neovim はバイトで数えます）、範囲の終端は含みません。ファイルでないバッファ（ターミナル、ヘルプ、名前のないバッファ）には、選択範囲はありません。

## Claude Code が頼めること

アダプターは、サーバーの次のリクエストに答えます（`register` で宣言する `methods`。設定の `methods` オプションで、置き換えたり、足したりできます）。

- `getOpenEditors`: 一覧にあるバッファのうち、ファイルであるもの。アクティブなのは、現在のウィンドウのファイルです（ターミナルやヘルプにフォーカスがあるときは、直前にいたファイル）。名前のないバッファ、ターミナル、ヘルプは、Claude にとってのエディタではありません。ファイルの言語は `filetype` で、まだ読み込まれていないバッファは、ファイル名から決まるものです。いくつかの filetype は、Claude Code が使う言語 ID に直します（`sh` は `shellscript`、filetype なしは `plaintext`）。
- `getCurrentSelection` と `getLatestSelection`: ユーザーがいるファイルの選択範囲。ファイルのない場所にフォーカスがあるあいだは、直前にあった選択範囲。
- `openFile`: ファイルを、ファイルを表示するウィンドウで開きます（すでに表示しているウィンドウ、なければ別のファイルを表示しているウィンドウ。ターミナルにフォーカスがあって、そういうウィンドウがないときは、ターミナルで動いているものに触らないよう、新しいウィンドウ）。`makeFrontmost` が false でなければフォーカスを渡し、`startText` から `endText` までのテキストを選択します。選択は、フォーカスのあるウィンドウで行います（フォーカスのないウィンドウでは、Neovim は選択を持てません）。そのため、`makeFrontmost` が false のときは、テキストの位置にカーソルを置くだけで、何も選択しません。ディレクトリや、ないファイルは、エラーです。
- `checkDocumentDirty` と `saveDocument`: テキストがメモリにあるファイルが対象です。開いていないファイルには `success = false` と答えます。`saveDocument` は、変更のあるファイルだけを書きます。`lineCount` は、VS Code の拡張機能と同じく、最後の改行のあとの空行も数えます。
- `getDiagnostics`: `vim.diagnostic` の診断。`uri` のファイル、またはその診断を持つ開いているすべてのファイルについて。重大度は `Error`、`Warning`、`Information`、`Hint`、列は UTF-16 コードユニット単位です。

Claude Code 2.1.322 は、自分で編集する前後に `getDiagnostics` を頼み、ほかのものは使いません。[DESIGN](DESIGN.md) の「Claude Code が自分から送るもの」を参照してください。

## 診断

`getDiagnostics` は、`vim.diagnostic` が持っているものを答えます。`uri` で求められたファイルには、2つのことをします。

- **ファイルは実パスで探し、求められた `uri` のままで答えます。** Claude Code はシンボリックリンクを解決せず、求めたファイルと `uri` が合わない答えは捨てるので、リンク経由で名指されたファイルは、実際にある場所で探し、渡された名前のままで返します。一覧にあってまだ読み込まれていないファイルには診断なしで答え、Neovim の一覧にないファイルには空の一覧で答えます。
- **ほかの誰かが書いたファイルは読み直し、その診断を待ちます。** Claude Code は、ファイルを編集した直後に、その編集で生じた問題を見つけるために診断を求め、言語サーバーは変更の少しあとに答えます。その時点で Neovim が持っているのは、変更前のテキストの診断です。バッファにユーザーの変更がなく、ファイルより古ければ、読み直します（診断を作るもの（言語サーバーなど）が、ほかの変更と同じように変更を知らされるようにするためです）。そして、VS Code の拡張機能と同じように、リクエストは、最初の更新を `first_ms` まで待ち、そのあとは `quiet_ms` のあいだ更新がなくなるまで待ち、全体では `cap_ms`（どのウィンドウにも表示されていないファイルは `cap_hidden_ms`）まで待ちます。書き込まれていない変更があるバッファはユーザーのものなので、そのままにします。待つのはリクエストだけで、Neovim は待ちません。`diagnostics_wait = false` でこれをやめられ、テーブルで時間の一部を決められます。

Claude Code 2.1.322 と pyright で確認しました。ファイルを編集した直後の `getDiagnostics` は、新しいテキストの診断で答えました。ただし、これでは待ちがあるときとないときの区別はつきません。モデルは2回の呼び出しのあいだに数秒かかり、そのときには pyright の処理が終わっていたからです（`diagnostics_wait = false` で同じ実行をしても、同じ答えでした）。待ちは、アダプターのテストで、ファイルの読み込みの 150 ms 後に答える Lua 製のリンターを使って試していて、待ちがなければテストは失敗します。

## 差分

Claude Code がファイルへの変更を提案すると、アダプターは、専用のタブページに表示します。左は、いまのファイル（開いているバッファがあれば、書いていない変更を含むそのバッファ）、右は、Claude Code の提案で、編集できます。2つは Neovim の差分モードになります。

- **右側での `:write` が承認です。** そのときの内容で承認されます（別のものを承認したいときは、先に編集してください）。アダプターは、ファイルには何も書きません。ファイルを書くのは Claude Code で、その内容を使います。
- **`:ClaudeIdeDiffReject` が却下です。** タブページや右のウィンドウを閉じても却下になります。`:ClaudeIdeDiffAccept` は `:write` と同じです。コマンドは、現在のバッファの差分、待っている差分が1つだけならそれ、名前を指定したもの（`:ClaudeIdeDiffReject edit-1`）に対して働きます。
- Claude Code が閉じる差分（ユーザーがターミナルで決めたときに、そう頼みます）は、画面から消えます。Claude Code がいなくなった差分も同じです。
- 表示中の差分と同じ名前の新しい差分は、それを置き換えます。

キーを指定でき、差分の2つのバッファの両方に割り当てられます（`diff.keys`）。

```lua
require('claude_ide').setup({
  diff = { keys = { accept = '<leader>da', reject = '<leader>dr' } },
})
```

**差分をどこに表示するかは、提供元（provider）の仕事で、設定から置き換えられます。** アダプターが流れ（待っているリクエスト、名前、置き換え、閉じること）を持ち、提供元は、差分を表示することと、画面から外すことだけをします。

```lua
require('claude_ide').setup({
  diff = {
    provider = {
      -- Show `diff` ({ tab_name, old_path, new_path, new_contents }). Call finish(true, contents) when the user accepts
      -- (with the contents of the file as they are then), finish(false) when they reject. Return a table: a handle for `close`
      -- (put in it whatever you need), in which `accept()` and `reject()` serve the two commands.
      open = function(diff, finish) ... return { } end,
      -- Take the diff that `open` gave this handle off the screen.
      close = function(handle) ... end,
    },
  },
})
```

`diff = false` にすると、差分を省きます。アダプターは差分を宣言せず、Claude Code は、変更をターミナルに表示します。

差分は、本物の Claude Code（2.1.322）を、ターミナルで動かし、アダプターを headless の Neovim に入れて試しました。Claude Code が差分を求めるのは、ファイルを編集してよいかをユーザーに尋ねるときだけです（許可モードが、尋ねるもので、`auto` や `accept edits` ではないとき）。そのとき `Opened changes in Neovim` と表示され、ユーザーは、Neovim でもターミナルでも決められます。確認できたこと: 右側での `:write` で、編集は先へ進み、Claude Code は、右側のそのときの内容でファイルを書きます（ユーザーが内容を編集していれば、モデルには、提案をユーザーが変更してから承認したことが伝わります）。`:ClaudeIdeDiffReject` では `User rejected update` となり、ファイルは変わりません。ターミナルで決めたときは、差分が Neovim の画面から消えます。

## Claude Code へのメンション

`:ClaudeIdeMention` は、現在のファイルを Claude Code にメンションし、`:'<,'>ClaudeIdeMention`（`:2,4ClaudeIdeMention` のような任意の範囲でも）は、その範囲の行をメンションします。Claude Code は、それをプロンプトに `@file#L2-4` として入れます。キーは割り当てていないので、好きなように割り当ててください。Lua からは `require('claude_ide').mention(first, last)` で同じことができ、行（1 始まり）を渡すか、渡さなければファイル全体になります。Claude Code に伝えられたかどうかを返します。

## テスト

```default
nvim --headless -u NONE -l editors/nvim/tests/unit.lua       # what needs no server
cargo test -p claude-ide-server --test nvim                  # a headless Neovim against the real executable
```

2つ目には、パスの通った `nvim` が要ります。なければ、テストはそう表示して成功します。`CLAUDE_IDE_REQUIRE_NVIM` が設定されている場合は除きます（CI は設定します）。
