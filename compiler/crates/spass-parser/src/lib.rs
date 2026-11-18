//! Defines the Spass AST and recursive-descent parser.

pub mod ast;
pub mod error;
pub mod parser;

pub use ast::{
    Annotation, ApplyStmt, AssertKind, BinOp, Block, CallArg, ConvKind, DispatchBind, DispatchDecl,
    Expr, ForIter, FuncDef, Item, Module, OrderKind, Param, RangeBound, RangeDecl, RationalLiteral,
    SpasioMode, Stmt, StructDef, StructField, TagItem, TagSelector, TagValue, TestDef, TestParam,
    Type, UnaryOp, Visibility,
};
pub use error::ParseError;
pub use parser::{FsFileLoader, FileLoader, ParsedProject, Parser};

#[cfg(test)]
mod tests {
    //! Parser tests for the test system.
    //!
    //! These tests exercise the new surface area added for the test
    //! driver: `test "name" { body }`, `test "name" with {... } { body }`,
    //! `assert(ident in RANGE)` with all four bracket combinations, and
    //! `assert(approx(value, target, tolerance))`. We parse small
    //! fragments and assert on the resulting AST shape - no codegen
    //! or runtime involved, so these run fast and pin the grammar.

    use super::ast::{AssertKind, Expr, Item, Stmt, Type};
    use super::Parser;

    fn parse_module(src: &str) -> super::ast::Module {
        Parser::parse_source(src, "<test>")
            .unwrap_or_else(|e| panic!("parse failed: {}", e))
    }

    fn first_test(src: &str) -> super::ast::TestDef {
        let m = parse_module(src);
        let Item::Test(t) = m.items.into_iter().next().expect("no items") else {
            panic!("first item is not a test");
        };
        t
    }

    fn first_stmt_of_test(src: &str) -> Stmt {
        let t = first_test(src);
        t.body.stmts.into_iter().next().expect("empty test body")
    }

    #[test]
    fn parses_test_item_without_with_block() {
        // the bare form, no parameters.
        let t = first_test(
            r#"
            test "simple" {
                let x: float = 1.0
            }
            "#,
        );
        assert_eq!(t.name, "simple");
        assert!(t.params.is_empty());
        assert_eq!(t.body.stmts.len(), 1);
    }

    #[test]
    fn parses_test_item_with_with_block() {
        // parameterised form. Each entry is `name: T = default`.
        let t = first_test(
            r#"
            test "parameterised" with {
                x: float = 1.0,
                y: float = 2.0
            } {
                let h: float = x + y
            }
            "#,
        );
        assert_eq!(t.name, "parameterised");
        assert_eq!(t.params.len(), 2);
        assert_eq!(t.params[0].name, "x");
        assert!(matches!(t.params[0].ty, Type::Float(_)));
        assert_eq!(t.params[1].name, "y");
        assert!(matches!(t.params[1].ty, Type::Float(_)));
    }

    #[test]
    fn test_param_default_is_expression() {
        // The default value is a regular expression; we record it as
        // an `Expr::FloatLit` here. A future test could use a more
        // complex default.
        let t = first_test(
            r#"
            test "defaults" with {
                x: float = 1.5 + 2.0
            } {
                return x
            }
            "#,
        );
        assert!(matches!(t.params[0].default, Expr::Binary { .. }));
    }

    #[test]
    fn parses_assert_in_range_closed() {
        // `[a, b]` - both inclusive.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(x in [0.0, 1.0])
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert, got {:?}", stmt);
        };
        let AssertKind::InRange { value, range, .. } = kind else {
            panic!("expected in-range, got {:?}", kind);
        };
        assert_eq!(value, "x");
        assert!(range.lo_inclusive);
        assert!(range.hi_inclusive);
    }

    #[test]
    fn parses_assert_in_range_open() {
        // `{a, b}` - both exclusive.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(x in {0.0, 1.0})
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert");
        };
        let AssertKind::InRange { range, .. } = kind else {
            panic!("expected in-range");
        };
        assert!(!range.lo_inclusive);
        assert!(!range.hi_inclusive);
    }

    #[test]
    fn parses_assert_in_range_left_closed_right_open() {
        // `[a, b}` - left-closed, right-open.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(x in [0.0, 1.0})
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert");
        };
        let AssertKind::InRange { range, .. } = kind else {
            panic!("expected in-range");
        };
        assert!(range.lo_inclusive);
        assert!(!range.hi_inclusive);
    }

    #[test]
    fn parses_assert_in_range_left_open_right_closed() {
        // `{a, b]` - left-open, right-closed.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(x in {0.0, 1.0])
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert");
        };
        let AssertKind::InRange { range, .. } = kind else {
            panic!("expected in-range");
        };
        assert!(!range.lo_inclusive);
        assert!(range.hi_inclusive);
    }

    #[test]
    fn parses_assert_in_range_with_identifier_bounds() {
        // range bounds can reference local names. The
        // parser records the identifier as `Expr::Ident`; the test
        // driver will resolve it at test time.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(h in [sea, max_h])
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert");
        };
        let AssertKind::InRange { range, .. } = kind else {
            panic!("expected in-range");
        };
        assert!(matches!(range.lo, Expr::Ident { .. }));
        assert!(matches!(range.hi, Expr::Ident { .. }));
    }

    #[test]
    fn parses_assert_approx() {
        // `assert(approx(value, target, tolerance))` form.
        let stmt = first_stmt_of_test(
            r#"
            test "r" {
                assert(approx(h, 1.0, 0.01))
            }
            "#,
        );
        let Stmt::Assert { kind, .. } = stmt else {
            panic!("expected assert");
        };
        let AssertKind::Approx { value, target, tolerance, .. } = kind else {
            panic!("expected approx");
        };
        assert_eq!(value, "h");
        assert!(matches!(target, Expr::FloatLit { .. }));
        assert!(matches!(tolerance, Expr::FloatLit { .. }));
    }

    #[test]
    fn assert_must_use_identifier() {
        // The language writes `assert(variable in...)`, where the LHS is a
        // local variable name. An expression LHS should fail.
        let res = Parser::parse_source(
            r#"
            test "r" {
                assert(1.0 in [0.0, 1.0])
            }
            "#,
            "<test>",
        );
        assert!(res.is_err(), "integer literal in assert LHS should fail");
    }

    #[test]
    fn parses_test_alongside_func() {
        // `test` and `func` are both top-level items; they can
        // appear in the same module. The assert LHS must be an
        // identifier, so we bind the call result to a local first.
        let m = parse_module(
            r#"
            func add(a: float, b: float): float { return a + b }

            test "smoke" {
                let s: float = add(1.0, 2.0)
                assert(s in [2.5, 3.5])
            }
            "#,
        );
        assert_eq!(m.items.len(), 2);
        assert!(matches!(m.items[0], Item::Func(_)));
        assert!(matches!(m.items[1], Item::Test(_)));
    }

    #[test]
    fn parses_named_test_item() {
        // Smoke test: just check the name round-trips.
        let t = first_test(r#"test "hello world" { return 0 }"#);
        assert_eq!(t.name, "hello world");
    }

    #[test]
    fn empty_test_body_parses() {
        // A test with no asserts is technically useless, but the
        // grammar allows it. The driver will report "no asserts"
        // separately.
        let t = first_test(r#"test "empty" {}"#);
        assert!(t.body.stmts.is_empty());
    }

    #[test]
    fn rejects_unclosed_range_bound() {
        // Missing `]` after a range bound should fail to parse.
        let res = Parser::parse_source(
            r#"
            test "r" {
                assert(x in [0.0, 1.0)
            }
            "#,
            "<test>",
        );
        // Note: `[0.0, 1.0)` mixes `[` with `)` which is not in the
        // bracket alphabet. The parser should fail.
        assert!(res.is_err(), "mismatched bracket should fail");
    }

    #[test]
    fn range_bound_with_no_brackets_is_rejected() {
        // The language uses bracket pairs; bare `(0.0, 1.0)` is not a
        // range. The parser should fail rather than accept it as a
        // tuple-shaped range.
        let res = Parser::parse_source(
            r#"
            test "r" {
                assert(x in (0.0, 1.0))
            }
            "#,
            "<test>",
        );
        assert!(res.is_err(), "paren range bound should fail");
    }

    #[test]
    fn test_can_be_compound_with_other_items() {
        // Mixing `test`, `struct`, and `func` in one module is allowed.
        // We bind the struct field to a local first because `assert`
        // takes an identifier on the LHS, not a field-access expression.
        let m = parse_module(
            r#"
            struct Point { x: float, y: float }
            func origin(): Point { return Point{x: 0.0, y: 0.0} }
            test "origin is zero" {
                let p: Point = origin()
                let px: float = p.x
                assert(px in [-0.01, 0.01])
            }
            "#,
        );
        assert_eq!(m.items.len(), 3);
    }

    /// Integration check: the on-disk `examples/terrain/test/terrain_test.spass`
    /// file must parse without errors. The file is small and lives at
    /// a known relative path from the workspace root; the test is
    /// skipped if the file is missing (e.g. when the test binary is
    /// moved to a different layout).
    #[test]
    fn parses_terrain_test_example() {
        // The path is relative to the workspace root (`compiler/`),
        // not the crate (`compiler/crates/spass-parser/`). Adjust
        // accordingly.
        let path = std::path::Path::new("../../examples/terrain/test/terrain_test.spass");
        if !path.exists() {
            eprintln!("skipping: {} not found", path.display());
            return;
        }
        let m = super::Parser::parse_file_with_includes(path, &super::FsFileLoader)
            .expect("terrain_test.spass should parse");
        // Expect at least one test item. The file currently has 4 tests.
        let test_count = m
            .items
            .iter()
            .filter(|i| matches!(i, super::Item::Test(_)))
            .count();
        assert!(test_count >= 1, "expected at least one test item, got {}", test_count);
    }

    // ====================================================================
    // module system (include / fileclude / ownclude)
    // ====================================================================

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// In-memory file system for include tests. The parser keys the
    /// include guard on canonical paths, but the test files do not
    /// exist on disk - `canonicalize` fails and falls back to the
    /// literal path, so a flat name map matches exactly what the
    /// parser looks up (single-segment names keep this true on both
    /// Windows and Unix path separators).
    struct MapLoader {
        files: HashMap<String, String>,
    }

    impl MapLoader {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                files: entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }
    }

    impl super::FileLoader for MapLoader {
        fn load(&self, path: &Path) -> Result<String, String> {
            self.files
                .get(&path.display().to_string())
                .cloned()
                .ok_or_else(|| format!("no such file: {}", path.display()))
        }
    }

    const MATH_SRC: &str = "func add(a: float, b: float): float { return a + b }";

    fn project_files(files: &[(&str, &str)], tops: &[&str]) -> super::ast::Module {
        let loader = MapLoader::new(files);
        let top_paths: Vec<PathBuf> = tops.iter().map(PathBuf::from).collect();
        Parser::parse_project_with_includes(&top_paths, &loader)
            .unwrap_or_else(|e| panic!("project parse failed: {}", e))
    }

    fn count_funcs(m: &super::ast::Module, name: &str) -> usize {
        m.items
            .iter()
            .filter(|i| matches!(i, super::Item::Func(f) if f.name == name))
            .count()
    }

    #[test]
    fn project_include_guard_expands_shared_header_once() {
        // two top-level files both `include` the same
        // header. Without the project-wide guard the second expansion
        // would duplicate every public function (E012).
        let m = project_files(
            &[
                ("math.spass", MATH_SRC),
                (
                    "a.spass",
                    "include \"math.spass\"\nfunc fa(x: float): float { return add(x, x) }",
                ),
                (
                    "b.spass",
                    "include \"math.spass\"\nfunc fb(x: float): float { return add(x, 1.0) }",
                ),
            ],
            &["a.spass", "b.spass"],
        );
        assert_eq!(count_funcs(&m, "add"), 1, "shared header must expand once");
        assert_eq!(count_funcs(&m, "fa"), 1);
        assert_eq!(count_funcs(&m, "fb"), 1);
    }

    #[test]
    fn reinclude_in_same_file_is_silent_noop() {
        // The same header included twice from one file is a no-op
        // (auto include-guard), not an E003/E012 error.
        let m = project_files(
            &[
                ("math.spass", MATH_SRC),
                (
                    "main.spass",
                    "include \"math.spass\"\ninclude \"math.spass\"\nfunc main_f(x: float): float { return add(x, x) }",
                ),
            ],
            &["main.spass"],
        );
        assert_eq!(count_funcs(&m, "add"), 1);
        assert_eq!(count_funcs(&m, "main_f"), 1);
    }

    #[test]
    fn cyclic_include_raises_e003() {
        // a includes b, b includes a - the include_stack detects the
        // recursion while the first expansion is still running.
        let loader = MapLoader::new(&[
            ("a.spass", "include \"b.spass\"\nfunc fa(): float { return 1.0 }"),
            ("b.spass", "include \"a.spass\"\nfunc fb(): float { return 2.0 }"),
        ]);
        let tops = [PathBuf::from("a.spass")];
        let res = Parser::parse_project_with_includes(&tops, &loader);
        match res {
            Err(super::ParseError::CyclicInclude { .. }) => {}
            other => panic!("expected CyclicInclude, got {:?}", other.map(|m| m.items.len())),
        }
    }

    #[test]
    fn ownclude_at_top_level_is_rejected() {
        // `ownclude` is a statement-level construct; a top-level
        // occurrence is almost certainly a missing enclosing body.
        let res = Parser::parse_source(
            "ownclude func f(x: float): float { return x }",
            "<test>",
        );
        assert!(res.is_err(), "top-level ownclude must be a parse error");
    }

    #[test]
    fn ownclude_statement_parses_with_enclosing_func() {
        // `ownclude func` inside a body parses into `Stmt::Ownclude`
        // with `enclosing_func` stamped to the outer function name.
        let m = parse_module(
            r#"
            func outer(x: float): float {
                let base = x + 1.0
                ownclude func inner(t: float): float {
                    return t * base
                }
                return inner(x)
            }
            "#,
        );
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        let super::Stmt::Ownclude(inner) = &f.body.stmts[1] else {
            panic!("expected ownclude stmt, got {:?}", f.body.stmts[1]);
        };
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.enclosing_func.as_deref(), Some("outer"));
        assert_eq!(inner.visibility, super::Visibility::OwnPrivate);
    }

    // ------------------------------------------------------------------
    // Struct-literal lookahead vs. namespace calls
    //
    // A control-flow body `{... }` and a struct literal `T { f: v }`
    // share the `Ident {` shape. The lookahead disambiguates on
    // `Ident:` inside the braces -- but a namespace call
    // (`spasio:output[...]`) has exactly that shape, so a body whose
    // first token is a namespace call used to be misread as a struct
    // literal. The namespace names are reserved, so they can never
    // introduce a field.
    // ------------------------------------------------------------------

    fn while_with_body(body: &str) -> super::ast::Module {
        parse_module(&format!(
            "func f() {{ let i = 0.0 let n = 1.0 while i < n {{ {} }} }}",
            body
        ))
    }

    #[test]
    fn namespace_call_at_head_of_while_body_parses() {
        for body in [
            "spasio:output[file](1.0, \"c\")",
            "s2d:window(1.0, 2.0, 3.0, 4.0)",
            "s3d:box(1.0, 2.0, 3.0)",
            "spio:read_text(\"f\")",
        ] {
            let m = while_with_body(body);
            let super::Item::Func(f) = &m.items[0] else {
                panic!("expected func item");
            };
            let super::Stmt::While { body: blk, .. } = &f.body.stmts[2] else {
                panic!("expected while stmt, got {:?}", f.body.stmts[2]);
            };
            assert!(!blk.stmts.is_empty(), "while body should not be empty");
        }
    }

    #[test]
    fn namespace_call_at_head_of_for_body_parses() {
        let m = parse_module(
            r#"
            func f() {
                for i in 0..10 {
                    spasio:output[file](1.0, "c")
                }
            }
            "#,
        );
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        assert!(matches!(f.body.stmts[0], super::Stmt::For { .. }));
    }

    #[test]
    fn struct_literal_still_parses() {
        // The exclusion must not disturb the shape the lookahead exists
        // for: `Ident { Ident: value }` is still a struct literal.
        let m = parse_module("func f() { let p = Point { x: 1.0, y: 2.0 } }");
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        let super::Stmt::Let { value, .. } = &f.body.stmts[0] else {
            panic!("expected let stmt");
        };
        assert!(matches!(value, super::Expr::StructLit { .. }));
    }

    #[test]
    fn plain_call_at_head_of_while_body_parses() {
        // The condition-ending-in-identifier shape with a non-namespace
        // call body was already valid and must stay valid.
        let m = while_with_body("g(1.0)");
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        assert!(matches!(f.body.stmts[2], super::Stmt::While { .. }));
    }

    /// A source file that starts with a UTF-8 byte order mark is still a
    /// source file: Windows editors write one by default, and the mark is
    /// not part of the program.
    #[test]
    fn a_byte_order_mark_does_not_break_the_source() {
        let src = "\u{FEFF}func height(x: float): float {\n    return x\n}\n";
        let m = parse_module(src);
        assert_eq!(m.items.len(), 1, "the BOM must not become an item");
    }

    /// `for x in xs` is a list walk, not a range. The
    /// iterated expression is parsed first; only a following `..`
    /// makes it a range.
    #[test]
    fn for_in_identifier_is_a_list_walk() {
        let m = parse_module(
            "func f() { let xs = [1, 2] for x in xs { let y = x } }",
        );
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        let super::Stmt::For { iter, .. } = &f.body.stmts[1] else {
            panic!("expected for stmt, got {:?}", f.body.stmts[1]);
        };
        assert!(matches!(iter, super::ast::ForIter::List(_)));
    }

    /// `random(a, b)` is the value form; the second
    /// argument list in `random(f, g)(x)` makes it the function form,
    /// and it must touch the closing `)`.
    #[test]
    fn random_value_and_function_forms_are_distinct() {
        let m = parse_module(
            r#"
            func f() {
                let a = random(1.0, 2.0)
                let b = random(g, h)(1.0)
            }
            "#,
        );
        let super::Item::Func(f) = &m.items[0] else {
            panic!("expected func item");
        };
        let super::Stmt::Let { value: a, .. } = &f.body.stmts[0] else {
            panic!("expected let stmt");
        };
        let super::Stmt::Let { value: b, .. } = &f.body.stmts[1] else {
            panic!("expected let stmt");
        };
        assert!(matches!(a, Expr::Random { args: None, .. }));
        assert!(matches!(b, Expr::Random { args: Some(_), .. }));
    }
}