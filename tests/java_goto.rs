use ropey::Rope;
use tower_lsp::lsp_types::{Location, Position, Url};

use jrsls::{
    indexer::Indexer,
    lang::{LanguageService, java::JavaService},
    state::GlobalIndex,
};

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

fn goto(
    service: &JavaService,
    index: &GlobalIndex,
    uri: &str,
    code: &str,
    needle: &str,
) -> Location {
    let tree = parse_and_index(code, uri, index);
    let rope = Rope::from_str(code);
    let position = pos_for(code, needle);

    service
        .goto_definition(&tree, &rope, position, index, uri)
        .expect("definition")
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
    let service = JavaService;

    let loc = goto(&service, &index, uri, code, "func(\"1\"");

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
    let service = JavaService;

    let loc = goto(&service, &index, uri, code, "println(1)");

    let expected_line = pos_for(code, "println(int");
    let def_line = loc.range.start.line;
    assert_eq!(
        def_line, expected_line.line,
        "expected int overload, got line {}",
        def_line
    );
}

#[test]
fn prefers_imported_class_over_unqualified() {
    let code = r#"
package org.cubewhy;

import java.util.HashMap;

class HashMap {} // should not be chosen

class Main {
    public static void entry() {
        HashMap map = new HashMap();
    }
}"#;
    let uri = "file:///workspace/Main.java";
    let index = GlobalIndex::new();
    let service = JavaService;

    // simulate library HashMap
    let lib_code = r#"
package java.util;
public class HashMap {
    public HashMap() {}
}
"#;
    parse_and_index(lib_code, "file:///workspace/java/util/HashMap.java", &index);

    let loc = goto(&service, &index, uri, code, "HashMap()");
    assert!(
        loc.uri.as_str().ends_with("java/util/HashMap.java"),
        "expected imported java.util.HashMap, got {}",
        loc.uri
    );
}

#[test]
fn resolves_same_file_over_imports() {
    let code = r#"
package org.cubewhy;

import java.util.HashMap;

class Main {
    static class HashMap {
        static void marker() {}
    }

    public static void entry() {
        HashMap.marker();
    }
}"#;
    let uri = "file:///workspace/Main.java";
    let index = GlobalIndex::new();
    let service = JavaService;

    let loc = goto(&service, &index, uri, code, "marker();");
    assert_eq!(
        loc.uri,
        Url::parse(uri).unwrap(),
        "expected to resolve to inner class, got {}",
        loc.uri
    );
}

#[test]
fn resolves_field_vs_method_with_same_name() {
    let code = r#"
package org.cubewhy;

class Data {
    static int value = 1;
    static int value() { return 2; }
}

class Main {
    public static void entry() {
        int a = Data.value;
        int b = Data.value();
    }
}"#;
    let uri = "file:///workspace/Main.java";
    let index = GlobalIndex::new();
    let service = JavaService;

    let field_loc = goto(&service, &index, uri, code, "value;");
    let method_line = pos_for(code, "static int value()").line;
    assert_ne!(
        field_loc.range.start.line, method_line,
        "field resolved to method"
    );

    let method_loc = goto(&service, &index, uri, code, "value();");
    assert_eq!(method_loc.range.start.line, method_line);
}

#[test]
fn prefers_java_lang_string_over_other_packages() {
    let code = r#"
package org.cubewhy;

class Main {
    String s = "";
}
"#;

    // stub out java.lang.String and another competing String class
    let java_lang_string = r#"
package java.lang;
public class String {
    public String() {}
}
"#;
    let other_string = r#"
package com.other;
public class String {}
"#;

    let index = GlobalIndex::new();
    parse_and_index(
        java_lang_string,
        "file:///workspace/java/lang/String.java",
        &index,
    );
    parse_and_index(
        other_string,
        "file:///workspace/com/other/String.java",
        &index,
    );

    let uri = "file:///workspace/Main.java";
    let service = JavaService;
    let loc = goto(&service, &index, uri, code, "String s");

    assert!(
        loc.uri.as_str().ends_with("java/lang/String.java"),
        "expected java.lang.String, got {}",
        loc.uri
    );
}

#[test]
fn member_completion_qualifier_chain() {
    let code = r#"
import java.util.ArrayList;

class Main {
    void demo() {
        ArrayList arr = new ArrayList();
        arr.si
    }
}
"#;

    let index = GlobalIndex::new();
    // stub java.util.ArrayList with size()
    let arraylist = r#"
package java.util;
public class ArrayList {
    public int size() { return 0; }
    public void clear() {}
}
"#;
    parse_and_index(arraylist, "file:///workspace/java/util/ArrayList.java", &index);

    let uri = "file:///workspace/Main.java";
    let service = JavaService;

    let caret_pos = pos_for(code, "arr.si");
    let caret = Position::new(caret_pos.line, caret_pos.character + "arr.si".len() as u32);

    let items = service
        .completion(
            &parse_and_index(code, uri, &index),
            &Rope::from_str(code),
            caret,
            &index,
            uri,
            &[],
        )
        .unwrap();

    let labels: Vec<_> = items.iter().map(|i| i.label.clone()).collect();
    assert!(
        labels.iter().any(|l| l.starts_with("size")),
        "expected size() in completions, got {:?}",
        labels
    );
}
