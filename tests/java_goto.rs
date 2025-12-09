use tower_lsp::lsp_types::Position;

use jrsls::{
    backend::LspBackend,
    indexer::Indexer,
    lang::{LanguageService, java::JavaService},
    state::GlobalIndex,
};
use ropey::Rope;
use tower_lsp::lsp_types::Url;

fn parse_and_index(code: &str, uri: &str, index: &GlobalIndex) -> tree_sitter::Tree {
    let rope = Rope::from_str(code);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("load java grammar");
    let tree = parser
        .parse_with_options(
            &mut |offset, _| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
            None,
            None,
        )
        .unwrap();
    Indexer::update_file(index, uri, &tree, &rope);
    tree
}

fn pos_for(code: &str, needle: &str) -> Position {
    for (i, l) in code.lines().enumerate() {
        if let Some(col) = l.find(needle) {
            return Position::new(i as u32, col as u32);
        }
    }
    Position::new(0, 0)
}

#[test]
fn resolves_varargs_overload() {
    let code = r#"
package org.cubewhy;

class Main {
    public static void func(double d) {}
    public static void func(String... args) {}

    public static void entry() {
        func("1", "2");
    }
}"#;
    let uri = "file:///workspace/Main.java";
    let index = GlobalIndex::new();
    let tree = parse_and_index(code, uri, &index);
    let rope = Rope::from_str(code);
    let service = JavaService;
    let position = pos_for(code, "func(\"1\"");

    let loc = service
        .goto_definition(&tree, &rope, position, &index, uri)
        .expect("definition");

    assert!(
        loc.uri == Url::parse(uri).unwrap(),
        "should resolve in same file"
    );
    // varargs overload is declared after the double overload
    assert!(
        loc.range.start.line == pos_for(code, "func(String").line,
        "expected to jump to varargs overload, got line {}",
        loc.range.start.line
    );
}

#[test]
fn resolves_println_int_overload() {
    let code = r#"
package org.cubewhy;

class PrintStream {
    public void println(int v) {}
    protected final void println(String s, Object... args) {}
}

class System {
    public static final PrintStream out = new PrintStream();
}

class Main {
    public static void entry() {
        System.out.println(1);
    }
}"#;
    let uri = "file:///workspace/Main.java";
    let index = GlobalIndex::new();
    let tree = parse_and_index(code, uri, &index);
    let rope = Rope::from_str(code);
    let service = JavaService;
    let position = pos_for(code, "println(1)");

    let loc = service
        .goto_definition(&tree, &rope, position, &index, uri)
        .expect("definition");

    let def_line = loc.range.start.line;
    // First overload (println(int)) is before the varargs one
    assert_eq!(def_line, 4, "expected int overload, got line {}", def_line);
}
