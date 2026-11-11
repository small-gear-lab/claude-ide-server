# claude-ide-server

Claude Code の `/ide` プロトコルのための、非公式の IDE 連携サーバーです。

**状況: 開発初期。** コアライブラリは本物の Claude Code と通信でき、テストは代役を使って行っています。サーバーは実行ファイルとして動き、Neovim アダプタは接続・登録・選択範囲の送信までできますが、Claude Code からのリクエストにはまだ答えません。

* [設計](DESIGN.md)
* [Neovim アダプタ](NVIM.md)
