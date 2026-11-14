//! Behavioural tests: feed keys to the editor and compare with what Vim does.

use fevim::editor::keys::parse_keys;
use fevim::editor::{Editor, Mode};

fn ed(text: &str) -> Editor {
    let mut e = fevim::new_editor();
    e.buf_mut().lines = text.split('\n').map(|s| s.to_string()).collect();
    e.buf_mut().clear_undo();
    e
}

fn feed(e: &mut Editor, k: &str) {
    e.input(&parse_keys(k));
    e.map_timeout();
}

fn text(e: &Editor) -> String {
    e.buf().lines.join("\n")
}

fn run(start: &str, keys: &str) -> (String, (usize, usize)) {
    let mut e = ed(start);
    feed(&mut e, keys);
    let c = e.cursor();
    (text(&e), (c.line, c.col))
}

#[test]
fn word_motions() {
    assert_eq!(run("hello world\nfoo bar", "wcwXYZ<Esc>").0, "hello XYZ\nfoo bar");
    assert_eq!(run("hello world\nfoo bar", "wdw").0, "hello \nfoo bar");
    assert_eq!(run("one two three", "dw").0, "two three");
    assert_eq!(run("one two three", "2dw").0, "three");
    assert_eq!(run("one two three", "d2w").0, "three");
    assert_eq!(run("one two three", "de").0, " two three");
    assert_eq!(run("one two three", "wcw2<Esc>").0, "one 2 three");
    assert_eq!(run("a.b c", "cwX<Esc>").0, "X.b c");
    assert_eq!(run("ab cd", "lcwX<Esc>").0, "aX cd");
    assert_eq!(run("one two", "w").1, (0, 4));
    assert_eq!(run("one\n\ntwo", "w").1, (1, 0));
    assert_eq!(run("one\n\ntwo", "ww").1, (2, 0));
    assert_eq!(run("one two", "$b").1, (0, 4));
    assert_eq!(run("one two", "e").1, (0, 2));
    assert_eq!(run("one two", "ee").1, (0, 6));
    assert_eq!(run("one two", "$ge").1, (0, 2));
    assert_eq!(run("foo(bar)", "w").1, (0, 3));
    assert_eq!(run("foo(bar) x", "W").1, (0, 9));
    assert_eq!(run("last", "w").1, (0, 3));
    assert_eq!(run("a b\nc", "wdw").0, "a \nc");
    assert_eq!(run("日本語 テスト", "w").1, (0, 10));
    assert_eq!(run("日本語テスト", "w").1, (0, 9));
}

#[test]
fn operators_and_counts() {
    assert_eq!(run("a\nb\nc\nd", "2dd").0, "c\nd");
    assert_eq!(run("a\nb\nc\nd", "jdj").0, "a\nd");
    assert_eq!(run("a\nb\nc", "Gdk").0, "a");
    assert_eq!(run("abc", "x").0, "bc");
    assert_eq!(run("abc", "$x").1, (0, 1));
    assert_eq!(run("abc def", "D").0, "");
    assert_eq!(run("abc def", "wC!<Esc>").0, "abc !");
    assert_eq!(run("abc", "yyp").0, "abc\nabc");
    assert_eq!(run("abc", "yyP").0, "abc\nabc");
    assert_eq!(run("abc", "ylp").0, "aabc");
    assert_eq!(run("abc def", "yiwwviwp").0, "abc abc");
    assert_eq!(run("a b", "xp").0, " ab");
    assert_eq!(run("one\ntwo", "J").0, "one two");
    assert_eq!(run("one\n  two", "J").0, "one two");
    assert_eq!(run("one\ntwo", "gJ").0, "onetwo");
    assert_eq!(run("abc", "~~").0, "ABc");
    assert_eq!(run("Hello", "g~~").0, "hELLO");
    assert_eq!(run("hello", "gUiw").0, "HELLO");
    assert_eq!(run("x 7 y", "5<C-a>").0, "x 12 y");
    assert_eq!(run("x -1", "<C-a>").0, "x 0");
    assert_eq!(run("a\nb", ">>").0, "\ta\nb");
    assert_eq!(run("abc", "rx").0, "xbc");
    assert_eq!(run("abc", "3rx").0, "xxx");
    assert_eq!(run("one two", "dt ").0, " two");
    assert_eq!(run("one two", "df ").0, "two");
    assert_eq!(run("a,b,c", "f,;x").0, "a,bc");
    assert_eq!(run("abcabc", "$Fbx").0, "abcac");
}

#[test]
fn text_objects() {
    assert_eq!(run("foo(bar, baz) x", "fbdi(").0, "foo() x");
    assert_eq!(run("foo(bar, baz) x", "fbda(").0, "foo x");
    assert_eq!(run("say \"hi there\" ok", "fhci\"X<Esc>").0, "say \"X\" ok");
    assert_eq!(run("say \"hi there\" ok", "fhda\"").0, "say ok");
    assert_eq!(run("one two three", "wdiw").0, "one  three");
    assert_eq!(run("one two three", "wdaw").0, "one three");
    assert_eq!(run("f {\n  a\n  b\n}", "jdi{").0, "f {\n}");
    assert_eq!(run("<a><b>x</b></a>", "fxdit").0, "<a><b></b></a>");
    assert_eq!(run("a\nb\n\nc", "dap").0, "c");
}

#[test]
fn insert_mode() {
    assert_eq!(run("", "ihello<Esc>").0, "hello");
    assert_eq!(run("abc", "Ax<Esc>").0, "abcx");
    assert_eq!(run("  abc", "Ix<Esc>").0, "  xabc");
    assert_eq!(run("abc", "ox<Esc>").0, "abc\nx");
    assert_eq!(run("abc", "Ox<Esc>").0, "x\nabc");
    assert_eq!(run("abc", "3ix<Esc>").0, "xxxabc");
    assert_eq!(run("abc", "A<BS><BS>x<Esc>").0, "ax");
    assert_eq!(run("one two", "A<C-w>x<Esc>").0, "one x");
    assert_eq!(run("ab", "a<CR><Esc>").0, "a\nb");
    let mut e = ed("x");
    e.exec_ex("set autoindent expandtab sw=2").unwrap();
    feed(&mut e, "S  if x {<CR>y<Esc>");
    assert_eq!(text(&e), "  if x {\n  y");
    e.exec_ex("set smartindent").unwrap();
    feed(&mut e, "ggSif x {<CR>y<Esc>");
    assert_eq!(text(&e), "  if x {\n    y\n  y");
    // insert-mode register paste
    assert_eq!(run("abc", "yiwA <C-r>\"<Esc>").0, "abc abc");
    // <C-o> runs one normal command
    assert_eq!(run("abc", "A<C-o>0X<Esc>").0, "Xabc");
}

#[test]
fn undo_redo_and_dot() {
    assert_eq!(run("abc", "xxu").0, "bc");
    assert_eq!(run("abc", "xxuu").0, "abc");
    assert_eq!(run("abc", "xxuu<C-r>").0, "bc");
    assert_eq!(run("a b c d", "dw..").0, "d");
    assert_eq!(run("a\nb\nc", "A!<Esc>j.j.").0, "a!\nb!\nc!");
    assert_eq!(run("abc abc", "cwX<Esc>w.").0, "X X");
    assert_eq!(run("hello", "ihi <Esc>u").0, "hello");
}

#[test]
fn visual_mode() {
    assert_eq!(run("abcdef", "lvlld").0, "aef");
    assert_eq!(run("a\nb\nc", "Vjd").0, "c");
    assert_eq!(run("abc\ndef\nghi", "l<C-v>jld").0, "a\nd\nghi");
    assert_eq!(run("abc\ndef", "<C-v>jIX<Esc>").0, "Xabc\nXdef");
    assert_eq!(run("ab\ncd", "<C-v>j$AX<Esc>").0, "abX\ncdX");
    assert_eq!(run("hello", "vU").0, "Hello");
    assert_eq!(run("abc", "v$y$p").0, "abcabc");
    assert_eq!(run("a b", "veyw.").1.0, 0);
}

#[test]
fn search_and_substitute() {
    assert_eq!(run("foo bar foo", "/foo<CR>").1, (0, 8));
    assert_eq!(run("foo bar foo", "/foo<CR>n").1, (0, 0));
    assert_eq!(run("foo bar foo", "*").1, (0, 8));
    assert_eq!(run("foo bar foo", "$#").1, (0, 0));
    assert_eq!(run("a1 a2 a3", ":s/a\\(\\d\\)/b\\1/g<CR>").0, "b1 b2 b3");
    assert_eq!(run("x\ny\nx", ":%s/x/z/<CR>").0, "z\ny\nz");
    assert_eq!(run("aaa", ":s/a/b/<CR>").0, "baa");
    assert_eq!(run("a\nb\na", ":g/a/d<CR>").0, "b");
    assert_eq!(run("a\nb\na", ":v/a/d<CR>").0, "a\na");
    assert_eq!(run("c\na\nb", ":sort<CR>").0, "a\nb\nc");
    assert_eq!(run("one two", "d/two<CR>").0, "two");
    assert_eq!(run("a\nb\nc", ":2,3d<CR>").0, "a");
    assert_eq!(run("a\nb\nc", ":1m$<CR>").0, "b\nc\na");
    assert_eq!(run("a\nb\nc", ":1t.<CR>").0, "a\na\nb\nc");
    assert_eq!(run("a\nb", ":%norm Ax<CR>").0, "ax\nbx");
}

#[test]
fn registers_and_macros() {
    assert_eq!(run("a\nb\nc", "qaA!<Esc>jq2@a").0, "a!\nb!\nc!");
    assert_eq!(run("abc", "\"ayl\"ap").0, "aabc");
    assert_eq!(run("abc def", "dw\"1p").0, "def");
    assert_eq!(run("abc def", "yiwwdiw\"0P").0, "abcabc ");
    assert_eq!(run("x", "ma:let @/='x'<CR>`a").1, (0, 0));
}

#[test]
fn mappings() {
    let mut e = ed("one two");
    e.exec_ex("nnoremap Y y$").unwrap();
    e.exec_ex("nmap n nzz").unwrap();
    e.exec_ex("inoremap jk <Esc>").unwrap();
    feed(&mut e, "wYP");
    assert_eq!(text(&e), "one twotwo");
    feed(&mut e, "Ax jk");
    assert_eq!(text(&e), "one twotwox ");
    assert_eq!(e.mode, Mode::Normal);
    e.exec_ex("nnoremap s <Nop>").unwrap();
    e.exec_ex("nnoremap sv :<C-u>vs<CR>").unwrap();
    feed(&mut e, "sv");
    assert_eq!(e.tab_wins().len(), 2);
    feed(&mut e, "s");
    assert_eq!(text(&e), "one twotwox ");
}

#[test]
fn windows_and_tabs() {
    let mut e = ed("x");
    feed(&mut e, ":sp<CR>:vs<CR>");
    assert_eq!(e.tab_wins().len(), 3);
    feed(&mut e, "<C-w>j");
    feed(&mut e, ":q<CR>");
    assert_eq!(e.tab_wins().len(), 2);
    feed(&mut e, ":tabnew<CR>");
    assert_eq!(e.tabs.len(), 2);
    feed(&mut e, "gt");
    assert_eq!(e.cur_tab, 0);
    feed(&mut e, ":tabclose<CR>");
    assert_eq!(e.tabs.len(), 1);
}

#[test]
fn render_basic() {
    let mut e = ed("hello\tworld");
    e.resize(5, 30);
    e.exec_ex("set number list listchars=tab:>-,eol:$").unwrap();
    let g = e.render();
    assert_eq!(g.row_text(0).trim_end(), "  1 hello>--world$");
    assert_eq!(g.row_text(1).trim_end(), "~");
}

#[test]
fn files_roundtrip() {
    let dir = std::env::temp_dir().join(format!("fevim_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.txt");
    std::fs::write(&f, "line1\r\nline2\r\n").unwrap();
    let mut e = fevim::new_editor();
    e.edit_file(&f, false).unwrap();
    assert_eq!(e.opt_str("fileformat"), "dos");
    feed(&mut e, "Ax<Esc>:w<CR>");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "line1x\r\nline2\r\n");
    std::fs::remove_dir_all(&dir).ok();
}
