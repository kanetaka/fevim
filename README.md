# FeVim

gvim を Rust で書き直したエディタです。UI は [GPUI](https://crates.io/crates/gpui)、
設定・拡張は Lua（Lua 5.4 / mlua）で書けます。既存の `_vimrc` / `_gvimrc` はそのまま
読み込めるほか、`:ImportVimrc` で編集可能な `init.lua` / `ginit.lua` に変換できます。

## ビルドと起動

```sh
cargo build --release
./target/release/fevim [ファイル...]
```

| OS | 必要なもの |
|----|-----------|
| Windows | MSVC ツールチェーン（動作確認済み） |
| macOS | Xcode（Metal）。GPUI の要件に準じます |
| Linux | `pkg-config` と X11/Wayland・Vulkan・fontconfig・xkbcommon の開発パッケージ（GPUI の要件） |

主なオプション（`fevim --help`）:

```
-u {vimrc}          使う vimrc / init.lua（NONE で設定を読まない）
-U {gvimrc}         使う gvimrc / ginit.lua
-c {cmd}, +{cmd}    最初のファイルを開いた後に実行
--cmd {cmd}         vimrc より前に実行
--clean             設定なしで起動
--headless          ウィンドウを開かずにコマンドを実行して終了（スクリプト・テスト用）
--transpile {file}  Vim script の Lua 変換結果を表示
```

`cargo build --no-default-features` で GPUI なし（headless のみ）のビルドもできます。

## 設定ファイル

設定ディレクトリ（`$FEVIM_CONFIG` で変更可）:

- Windows: `%APPDATA%\fevim`
- macOS / Linux: `$XDG_CONFIG_HOME/fevim`（既定 `~/.config/fevim`）

起動時の読み込み順:

1. `init.lua`。無ければ Vim と同じ場所の `_vimrc` / `.vimrc` / `vimfiles/vimrc` / `$VIM/_vimrc`
2. `ginit.lua`。無ければ `_gvimrc` / `.gvimrc` …（GUI 起動時のみ）
3. コマンドラインのファイルを開き、`GUIEnter` → `VimEnter`

起動時のエラーは gvim と同様に「Press ENTER」形式でまとめて表示します。

### vimrc のインポート

```vim
:ImportVimrc ~/_vimrc            " → <設定ディレクトリ>/init.lua
:ImportVimrc ~/_gvimrc           " → <設定ディレクトリ>/ginit.lua
:ImportVimrc! ~/_vimrc out.lua   " 出力先指定・上書き
```

変換結果は元のコメントと構造を保った Lua になります。

```lua
vim.opt.tabstop = 4
vim.opt.listchars = 'eol:$,tab:<-,extends:<,trail:_'
vim.keymap.set('n', 'Y', 'y$')
vim.api.nvim_create_augroup('InsertHook', { clear = false })
vim.api.nvim_create_autocmd('InsertEnter', { group = 'InsertHook', pattern = '*',
  command = 'highlight StatusLine guifg=#ccdc90 guibg=#2E4340' })
V.func('MySetSearch', { 'cmd' }, { varargs = true }, function(a, l)
  l.saved_reg = vim.fn.getreg('"')
  ...
end)
```

`vim.opt` / `vim.keymap.set` などで直接表せない Vim script の意味論（数値と文字列の
自動変換、0 始まりのリスト、`=~` など）はヘルパー `vim._vs`（変換結果中の `V`）で
再現します。失敗しうる文は `at(行番号, function() ... end)` で包まれ、エラーが出ても
Vim と同じく次の行から実行を続けます。

インポートせずに vimrc を直接読み込んだ場合も、内部で同じ変換を行ってから実行します。

### Lua API（Neovim 互換のサブセット）

- `vim.o` `vim.go` `vim.bo` `vim.wo` `vim.opt`（`:append` `:remove` `:prepend` `:get`）`vim.opt_local` `vim.opt_global`
- `vim.g` `vim.b` `vim.w` `vim.t` `vim.v` `vim.env`
- `vim.cmd`（`vim.cmd.colorscheme('hybrid')` 形式も可）、`vim.fn`（Vim の組み込み関数とユーザー関数）
- `vim.keymap.set` / `del`
- `vim.api.nvim_create_autocmd` / `nvim_create_augroup` / `nvim_clear_autocmds` / `nvim_create_user_command` /
  `nvim_set_hl` / `nvim_buf_get_lines` / `nvim_buf_set_lines` / `nvim_win_get_cursor` / `nvim_feedkeys` /
  `nvim_set_option_value` ほか
- `vim.print` `vim.inspect` `vim.notify` `vim.keycode` `vim.split` `vim.trim` `vim.tbl_*` `vim.schedule` `vim.defer_fn` `vim.json`
- `require()` は `<設定ディレクトリ>/lua/` を検索します

`:lua {code}` / `:luafile` / `:source file.lua` で Lua を、`:source file.vim` で Vim script を実行できます。

## 実装状況

**エディタ**: Normal / Insert / Replace / Visual（文字・行・矩形）/ コマンドライン、オペレータ（`d c y < > = g~ gu gU g? gq !`）、
モーション（単語・文・段落・`f t ; ,`・`%`・`*` `#`・マーク・`H M L`・`gj gk` など）、テキストオブジェクト
（`iw aw ip a( i" it` など）、レジスタ（名前付き・番号・`"+` `"*` とシステムクリップボード）、マクロ、`.` リピート、
undo/redo、カウント、マッピング（`noremap` `<silent>` `<expr>` `<buffer>` `<Plug>` `<leader>`、タイムアウト）、
ウィンドウ分割・タブページ、バッファ一覧、検索（`incsearch` `hlsearch` `smartcase`、オフセット）、`:s` `:g` `:sort`
`:normal` `:!` フィルタ、補完（`<C-n>` `<C-p>` `<C-x><C-l>`）、自動インデント（`autoindent` `smartindent` `cindent` の簡易版）、
文字コード判定（UTF-8 / BOM / CP932 / EUC-JP など）と改行コード、ファイル名補完・`wildmenu`。

**Vim script**: `let` `unlet` `if` `while` `for` `function`（可変長引数・`s:` 関数・`<SID>`）、`try/catch/finally` `throw`、
`execute` `call` `echo` 系、ラムダ、リスト・辞書、`autoload#関数`（runtimepath の `autoload/` から自動読み込み）、
約 200 個の組み込み関数。

**GUI**: GPUI によるセル描画、`guifont` / `guifontwide` / `linespace`、カラースキーム（組み込みの `default` / `hybrid`、
または runtimepath の `colors/*.vim|lua`）、シンタックス強調（主要言語の簡易ハイライタ）、`listchars` `number`
`cursorline` `cursorcolumn` `colorcolumn` `:match`、ステータスライン（`%{expr}` を含む `statusline` 書式）、
タブライン、IME（日本語入力のプリエディット表示）、マウス（クリック・ダブル/トリプルクリック選択・ドラッグ・
ホイール・タブクリック・分割境界のドラッグ）、ファイルのドラッグ&ドロップ、`winsize` `lines` `columns`。

### 主な制限

- Vim の正規表現は Rust の `regex` に変換して実行します。後方参照（`\1`）と先読み・後読み（`\@=` など）は未対応です。
- シンタックスは Vim の `syntax/*.vim` ではなく内蔵の簡易ハイライタです。`:syntax keyword/match/region` は無視されます。
- 折りたたみ、quickfix（`:make` `:grep` `:copen`）、スワップ・バックアップ・undo ファイル、`:terminal`、メニュー、
  スペルチェック、Vim9 script は未実装です（該当コマンドは受け付けて無視するか、エラーを表示します）。
- `plugin/` ディレクトリのプラグインは既定では読み込みません。`let g:fevim_load_plugins = 1` で有効になりますが、
  対応しているのは Vim script のサブセットです。
- `winpos` による起動後のウィンドウ移動は GPUI に API が無いため起動時の位置にのみ反映します。

## 構成

```
src/
  main.rs, lib.rs        起動処理（設定の読み込み順、CLI）
  editor/                エディタコア（GUI 非依存）
    buffer.rs            テキスト・undo・ファイル入出力
    window.rs            ウィンドウ分割レイアウト・タブ・バッファ管理・スクロール
    normal.rs insert.rs cmdline.rs   各モード
    motion.rs            モーション・テキストオブジェクト・検索（Vim の textobject.c 準拠）
    ex.rs excmds.rs      Ex コマンドの解析と実装
    options.rs           オプション（options_table.rs は vim/src/optiondefs.h から生成）
    highlight.rs syntax.rs vregex.rs statusline.rs screen.rs mouse.rs
  script/
    vimscript.rs         Vim script → Lua 変換器
    prelude.lua          vim.* API と Vim script ランタイム（vim._vs）
    builtins.rs          Rust 実装の組み込み関数
    mod.rs               Lua ホスト（mlua）
  gui/                   GPUI フロントエンド（描画・キー/IME・マウス・フォント）
tools/gen_options.py     オプション表の生成スクリプト
tests/editing.rs         キー入力による挙動テスト
```

オプション表の再生成: `python tools/gen_options.py ../vim/src`

## テスト

```sh
cargo test                       # ユニットテストと挙動テスト
fevim --headless -u NONE -c "echo 1 + 1"
fevim --transpile ~/_vimrc       # 変換結果の確認
```

キー入力のデバッグ表示: 環境変数 `FEVIM_DEBUG_KEYS=1`。vimrc 読み込み時の変換結果表示: `FEVIM_DEBUG_TRANSPILE=1`。
