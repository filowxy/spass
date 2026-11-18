//! Parses Spass source with a recursive-descent parser.

use super::ast::*;
use super::error::ParseError;
use num_rational::Rational64;
use spass_lexer::{lex, Span, Token, TokenKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Hand-rolled recursive-descent parser.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Tracks the directory of the file currently being parsed. Used to
    /// resolve relative `include` paths.
    base_dir: PathBuf,
    /// the file path of the source this parser is
    /// currently reading. Stamped onto every [`FuncDef`] it
    /// produces so the semantic pass can enforce `fileclude`
    /// visibility across the project. Set on construction; never
    /// mutates during parsing (the recursive-descent sub-parser
    /// used by include resolution gets its own value).
    current_file: String,
    /// Files currently on the include-resolution stack - used to detect
    /// cycles (E003). A file that re-appears *while* it is being
    /// expanded is a cycle; a file that has already finished expanding
    /// is in [`Self::expanded`] and is silently skipped instead.
    include_stack: HashSet<PathBuf>,
    /// Files that have already been fully expanded in this resolution
    /// pass. Re-include of any file in this set is a silent no-op
    /// The set is shared across the recursive descent
    /// so the same header merged from two top-level files is included
    /// exactly once.
    expanded: HashSet<PathBuf>,
    /// Function names defined in the current module (E012).
    function_names: HashSet<String>,
    /// stack of currently-open public-function names.
    /// Pushed on entry to a top-level `func` (or `fileclude func`),
    /// popped on exit. An `ownclude func` encountered at a
    /// statement position pops the top of this stack to learn which
    /// public function owns it. Empty at module level and outside
    /// any function body.
    enclosing_func_stack: Vec<String>,
}

impl Parser {
    /// Create a parser from a pre-lexed token stream. Use this when the
    /// caller already owns the tokens (e.g. for testing).
    pub fn from_tokens(tokens: Vec<Token>, base_dir: impl Into<PathBuf>) -> Self {
        // The file path is taken from the first token's span, which
        // is what the lexer was told to use for source location. The
        // caller (CLI / project entry point) passes the same value
        // to `lex`, so the two stay in sync.
        let current_file = tokens
            .first()
            .map(|t| t.span.file.clone())
            .unwrap_or_default();
        Self {
            tokens,
            pos: 0,
            base_dir: base_dir.into(),
            current_file,
            include_stack: HashSet::new(),
            // `expanded` is the cross-resolution set of files whose
            // contents have already been merged in. Re-including any
            // file in this set is a silent no-op ("the
            // compiler expands only the first occurrence; no manual
            // include guard is required"). Without this set, a
            // translation unit that `include`s the same header twice
            // would raise E003 on the second occurrence, contradicting
            // the language.
            expanded: HashSet::new(),
            function_names: HashSet::new(),
            enclosing_func_stack: Vec::new(),
        }
    }

    /// Lex + parse a single source string, no include resolution.
    pub fn parse_source(source: &str, file: &str) -> Result<Module, ParseError> {
        let tokens = lex(source, file).map_err(|e| ParseError::Syntax {
            message: format!("{}", e),
            span: e.span(),
        })?;
        let mut parser = Self::from_tokens(tokens, std::path::Path::new(file));
        parser.parse_module()
    }

    /// Lex + parse a source file path, with include resolution using the
    /// supplied [`FileLoader`].
    pub fn parse_file_with_includes(
        path: &Path,
        loader: &dyn FileLoader,
    ) -> Result<Module, ParseError> {
        let src = loader.load(path).map_err(|e| ParseError::Syntax {
            message: format!("cannot read `{}`: {}", path.display(), e),
            span: Span::new(path.display().to_string(), 0, 0, 0, 0),
        })?;
        let tokens = lex(&src, &path.display().to_string()).map_err(|e| ParseError::Syntax {
            message: format!("{}", e),
            span: e.span(),
        })?;
        let mut parser = Self::from_tokens(tokens, path.parent().unwrap_or(Path::new(".")));
        let module = parser.parse_module()?;
        // Top-level call: each top-level file is its own resolution
        // root. We pass a fresh `expanded` set so re-include of the
        // same header from two top-level files in the same project
        // is still a no-op.
        let mut expanded = HashSet::new();
        parser.resolve_includes(module, loader, &mut expanded)
    }

    /// Lex + parse a list of source files as one project, with a
    /// shared include-guard set. This is the right entry point for
    /// `spass build` / `spass run`: a header that two top-level
    /// files both `include` is expanded exactly once across the
    /// whole project ("the compiler expands only the
    /// first occurrence; no manual include guard is required").
    ///
    /// `ponytail`: the per-file entry point
    /// [`Self::parse_file_with_includes`] uses a fresh
    /// `expanded` set on every call, which is the right behaviour
    /// for single-file tests but wrong for a multi-file project.
    /// The CLI uses this function instead.
    pub fn parse_project_with_includes(
        files: &[PathBuf],
        loader: &dyn FileLoader,
    ) -> Result<Module, ParseError> {
        Ok(Self::parse_project_with_dependencies(files, loader)?.module)
    }

    /// Lex + parse a list of source files as one project and report
    /// the source files the parse actually read.
    ///
    /// The dependency list is what a build cache must key on. A
    /// directory listing of `src/` misses every file pulled in through
    /// `include` from outside `src/`, and a cache keyed on the
    /// listing alone serves stale code that no amount of touching the
    /// `src/` files can fix.
    pub fn parse_project_with_dependencies(
        files: &[PathBuf],
        loader: &dyn FileLoader,
    ) -> Result<ParsedProject, ParseError> {
        let mut all_items: Vec<Item> = Vec::new();
        let mut expanded: HashSet<PathBuf> = HashSet::new();
        let mut source_file = String::new();
        for path in files {
            // A top-level file that another top-level file already
            // `include`d has its items merged into the including file
            // during the including file's expansion. Re-parsing it
            // here would duplicate every public/fileclude function
            // (E012). The check uses the canonical path so relative
            // paths and `.`/`..` segments all compare equal.
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            if expanded.contains(&canonical) {
                continue;
            }
            let src = loader.load(path).map_err(|e| ParseError::Syntax {
                message: format!("cannot read `{}`: {}", path.display(), e),
                span: Span::new(path.display().to_string(), 0, 0, 0, 0),
            })?;
            let tokens = lex(&src, &path.display().to_string())
                .map_err(|e| ParseError::Syntax { message: format!("{}", e), span: e.span() })?;
            let mut parser =
                Self::from_tokens(tokens, path.parent().unwrap_or(Path::new(".")));
            let module = parser.parse_module()?;
            // Mark this top-level file as expanded BEFORE recursing
            // so any sibling `include` of the same file is a no-op
            // (the project's `expanded` set is shared via `&mut`).
            //
            // The file is ALSO pushed onto the include stack while its
            // own expansion runs: a file that `include`s a top-level
            // file that includes it back (a -> b -> a) is still a cycle
            // (E003), not a harmless re-include - the pre-seeded
            // `expanded` entry alone would silently cut the recursion
            // and mask the cycle. The stack is per-`Parser`, so each
            // top-level file's chain starts clean.
            expanded.insert(canonical.clone());
            parser.include_stack.insert(canonical);
            let resolved = parser.resolve_includes(module, loader, &mut expanded)?;
            source_file = resolved.source_file.clone();
            all_items.extend(resolved.items);
        }
        // `expanded` holds the canonical path of every file the parse
        // read: the top-level files and every file reached through
        // `include`.
        let mut dependencies: Vec<PathBuf> = expanded.into_iter().collect();
        dependencies.sort();
        Ok(ParsedProject {
            module: Module {
                items: all_items,
                source_file,
            },
            dependencies,
        })
    }

    /// Parse a single source string into a `Module` without resolving
    /// `include` directives. The caller is responsible for providing the
    /// already-lexed tokens.
    pub fn parse_module(&mut self) -> Result<Module, ParseError> {
        let start_span = self.peek().span.clone();
        let mut items = Vec::new();
        while !self.is_eof() {
            items.push(self.parse_item()?);
        }
        Ok(Module { items, source_file: start_span.file })
    }

    // ========================================================================
    // Items
    // ========================================================================

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let leading_annots = self.collect_leading_annotations()?;

        if let TokenKind::Ident(s) = self.peek_kind().clone() {
            match s.as_str() {
                "fileclude" => {
                    self.advance();
                    let func = self.parse_func_def_with_visibility_and_annots(
                        Visibility::FilePrivate,
                        leading_annots,
                    )?;
                    return Ok(Item::Func(func));
                }
                "ownclude" => {
                    // `ownclude` is a statement-level
                    // construct, not a top-level one. A user typing
                    // it at module level almost certainly forgot the
                    // enclosing function body. Surface a clear error
                    // instead of silently parsing it as a top-level
                    // function (which would also lose the
                    // enclosing_func field and break scope).
                    return Err(self.error(
                        "`ownclude` is only allowed inside a function body; \
                         define it as `ownclude func name(...) { ... }` within \
                         a top-level `func` body"
                    ));
                }
                "sequence" => {
                    // `sequence` does not carry annotations.
                    self.advance();
                    return self.parse_sequence_def();
                }
                "include" => {
                    // `include` does not carry annotations.
                    self.advance();
                    let path_tok = self.expect(TokenKind::StringLit(String::new()))?;
                    let path = if let TokenKind::StringLit(s) = &path_tok.kind {
                        s.clone()
                    } else {
                        unreachable!()
                    };
                    return Ok(Item::Include { path, span: path_tok.span });
                }
                _ => {}
            }
        }

        match self.peek_kind() {
            TokenKind::KwFunc => {
                let func = self.parse_func_def(leading_annots)?;
                Ok(Item::Func(func))
            }
            TokenKind::KwStruct => {
                // Structs do not carry annotations; drop them on the floor.
                let s = self.parse_struct_def()?;
                Ok(Item::Struct(s))
            }
            TokenKind::KwTest => Ok(Item::Test(self.parse_test_item()?)),
            TokenKind::KwLet => Err(self.error("top-level `let` is not allowed; use `func` or `struct`")),
            _ => Err(self.error(&format!(
                "expected `func`, `struct`, `include`, or `test`, found {}",
                self.peek_kind().debug_name()
            ))),
        }
    }

    // ========================================================================
    // Test declarations
    // ========================================================================
    //
    // Two accepted forms:
    // test "name" { body }
    // test "name" with { p1: T = default,... } { body }
    //
    // The `with` block is a flat comma-separated list of (name, type,
    // default-expression) triples. Each triple's `default` is parsed as
    // a regular expression; a test is just a function-shaped body that
    // runs under the `spass test` driver.

    fn parse_test_item(&mut self) -> Result<TestDef, ParseError> {
        let kw = self.expect(TokenKind::KwTest)?;
        let name = self.expect_string()?;
        let mut params: Vec<TestParam> = Vec::new();
        if matches!(self.peek_kind(), TokenKind::KwWith) {
            self.advance();
            params = self.parse_with_block()?;
        }
        let body = self.parse_block()?;
        let span = kw.span.join(body.span.clone());
        Ok(TestDef { name, params, body, span })
    }

    /// Parse `sequence name { body }`. The caller has
    /// already consumed the `sequence` identifier. A sequence takes no
    /// parameters - it is an entry point, not a callable.
    fn parse_sequence_def(&mut self) -> Result<Item, ParseError> {
        let name_tok = self.expect_ident()?;
        let body = self.parse_block()?;
        let span = name_tok.span.join(body.span.clone());
        Ok(Item::Sequence(SequenceDef {
            name: name_tok.name,
            body,
            span,
        }))
    }

    /// Parse `{ name: type = default,... }` for a `with` block.
    /// Empty `{}` is accepted but useless; a missing type or default is
    /// a syntax error.
    fn parse_with_block(&mut self) -> Result<Vec<TestParam>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut params = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RBrace) {
            loop {
                let name = self.expect_ident()?;
                self.expect(TokenKind::Colon)?;
                let ty = self.parse_type()?;
                self.expect(TokenKind::Eq)?;
                let default = self.parse_expr()?;
                let span = name.span.join(default.span());
                params.push(TestParam { name: name.name, ty, default, span });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(params)
    }

    fn parse_func_def(&mut self, annotations: Vec<Annotation>) -> Result<FuncDef, ParseError> {
        self.parse_func_def_with_visibility_and_annots(Visibility::Public, annotations)
    }

    fn parse_func_def_with_visibility_and_annots(
        &mut self,
        visibility: Visibility,
        mut annotations: Vec<Annotation>,
    ) -> Result<FuncDef, ParseError> {
        self.expect(TokenKind::KwFunc)?;
        let name_tok = self.expect_ident()?;
        let name = name_tok.name.clone();
        let name_span = name_tok.span.clone();

        // `ownclude` functions live in a per-enclosing-function
        // scope, not the top-level module scope. A nested function with the
        // same name as a top-level function (e.g. `smooth`, which is also
        // a builtin) is legal - the duplicate-name check only applies to
        // the top-level (Public / FilePrivate) case.
        if !matches!(visibility, Visibility::OwnPrivate) {
            if !self.function_names.insert(name.clone()) {
                return Err(ParseError::DuplicateFunction {
                    name: name.clone(),
                    span: name_span.clone(),
                });
            }
        }

        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                params.push(self.parse_param()?);
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen)?;

        let mut return_type = None;
        let mut multi_returns = None;
        if matches!(self.peek_kind(), TokenKind::Colon) {
            self.advance();
            if matches!(self.peek_kind(), TokenKind::LParen) {
                self.advance();
                let mut types = Vec::new();
                types.push(self.parse_type()?);
                while matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                    types.push(self.parse_type()?);
                }
                self.expect(TokenKind::RParen)?;
                if types.is_empty() {
                    return Err(self.error("multi-return tuple must have at least one type"));
                }
                multi_returns = Some(types);
            } else {
                return_type = Some(self.parse_type()?);
            }
        }

        Self::validate_annotations(&annotations, &name_span)?;

        // Push this function's name onto the enclosing-func stack
        // for the duration of the body parse. The stack is empty
        // for top-level `func` calls; nested `ownclude` statements
        // (parsed by `parse_stmt`) read the top of the stack to
        // stamp their `enclosing_func` field. The pop is paired
        // with this push in the `Ok(FuncDef {... })` arm below.
        self.enclosing_func_stack.push(name.clone());
        let body = self.parse_block()?;
        let popped = self.enclosing_func_stack.pop();
        debug_assert_eq!(popped.as_ref(), Some(&name));

        Ok(FuncDef {
            name,
            params,
            return_type,
            multi_returns,
            body,
            annotations: std::mem::take(&mut annotations),
            visibility,
            // stamp the source file at parse time so
            // the semantic pass can enforce `fileclude` visibility
            // without re-reading the file path. The value is the
            // same `current_file` used by the lexer for spans, so
            // error messages and visibility checks reference the
            // exact same string.
            source_file: self.current_file.clone(),
            // Only `ownclude` (parsed separately as a statement) sets
            // this to a non-`None` value. Top-level `func` and
            // `fileclude` are always `None` - the visibility field
            // already differentiates them.
            enclosing_func: None,
            span: name_span,
        })
    }

    fn validate_annotations(annots: &[Annotation], fn_span: &Span) -> Result<(), ParseError> {
        let mut has_gpu = false;
        let mut has_checkpoint = false;
        let mut has_resume = false;
        let mut tag_keys: HashSet<String> = HashSet::new();

        for a in annots {
            match a {
                Annotation::Gpu { .. } => has_gpu = true,
                Annotation::Checkpoint { .. } => has_checkpoint = true,
                Annotation::Resume { .. } => has_resume = true,
                Annotation::Tag { items, .. } => {
                    for item in items {
                        if let TagItem::Valued(key, _, s) = item {
                            if !tag_keys.insert(key.clone()) {
                                return Err(ParseError::DuplicateTagKey {
                                    key: key.clone(),
                                    span: s.clone(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if has_gpu && (has_checkpoint || has_resume) {
            return Err(ParseError::Syntax {
                message: "E009: @gpu function cannot use @checkpoint or @resume".to_string(),
                span: fn_span.clone(),
            });
        }
        // E013 (`@gpu` + `@await`) is enforced in the semantic pass
        // because `@await` is now a body-level statement
        // (`Stmt::Await`), not a function-level annotation. The
        // function annotation `@gpu` and the body walker see
        // different things; the semantic layer is the only one
        // that can connect the two.
        let _ = has_gpu;
        Ok(())
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        let is_out = if matches!(self.peek_kind(), TokenKind::KwOut) {
            self.advance();
            true
        } else {
            false
        };
        let name_tok = self.expect_ident()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.parse_type()?;
        Ok(Param {
            name: name_tok.name,
            ty,
            is_out,
            span: name_tok.span,
        })
    }

    fn parse_struct_def(&mut self) -> Result<StructDef, ParseError> {
        self.expect(TokenKind::KwStruct)?;
        let name_tok = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof) {
            let fname = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let fty = self.parse_type()?;
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            }
            fields.push(StructField { name: fname.name, ty: fty, span: fname.span });
        }
        self.expect(TokenKind::RBrace)?;
        Ok(StructDef { name: name_tok.name, fields, span: name_tok.span })
    }

    // ========================================================================
    // Types
    // ========================================================================

    fn parse_type(&mut self) -> Result<Type, ParseError> {
        let base = match self.peek_kind().clone() {
            TokenKind::KwFloat => { self.advance(); Type::Float(None) }
            TokenKind::KwInt => { self.advance(); Type::Int(None) }
            TokenKind::KwUint => { self.advance(); Type::Uint(None) }
            TokenKind::KwBool => { self.advance(); Type::Bool }
            TokenKind::KwString => { self.advance(); Type::String }
            TokenKind::KwList => {
                self.advance();
                self.expect(TokenKind::Lt)?;
                let inner = self.parse_type()?;
                self.expect(TokenKind::Gt)?;
                Type::List(Box::new(inner))
            }
            // column vectors. Per-component value-domain
            // declarations are parsed via the uniform form `vecN[min, max, step]`
            // (the same range is applied to all components). Per-component
            // ranges (semicolon-separated) are a later revision.
            TokenKind::KwVec2 => { self.advance(); Type::Vec2(None, None) }
            TokenKind::KwVec3 => { self.advance(); Type::Vec3(None, None, None) }
            TokenKind::KwVec4 => { self.advance(); Type::Vec4(None, None, None, None) }
            // Matrices do not carry value-domain declarations in the MVP.
            TokenKind::KwMat3 => { self.advance(); Type::Mat3 }
            TokenKind::KwMat4 => { self.advance(); Type::Mat4 }
            TokenKind::Ident(name) => {
                self.advance();
                Type::Struct(name, self.previous_span())
            }
            _ => {
                return Err(self.error(&format!(
                    "expected a type, found {}",
                    self.peek_kind().debug_name()
                )))
            }
        };
        if matches!(self.peek_kind(), TokenKind::LBracket) {
            let range = self.parse_range_decl()?;
            Ok(apply_range(base, range)?)
        } else {
            Ok(base)
        }
    }

    fn parse_range_decl(&mut self) -> Result<RangeDecl, ParseError> {
        let lbr = self.expect(TokenKind::LBracket)?;
        let mut r = RangeDecl { min: None, max: None, step: None, span: lbr.span.clone() };
        if matches!(self.peek_kind(), TokenKind::RBracket) {
            let span = self.advance().span;
            r.span = lbr.span.join(span);
            return Ok(r);
        }
        r.min = Some(self.parse_rational_literal()?);
        if matches!(self.peek_kind(), TokenKind::Comma) {
            self.advance();
        }
        if !matches!(self.peek_kind(), TokenKind::RBracket) {
            r.max = Some(self.parse_rational_literal()?);
        }
        if matches!(self.peek_kind(), TokenKind::Comma) {
            self.advance();
        }
        if !matches!(self.peek_kind(), TokenKind::RBracket) {
            r.step = Some(self.parse_rational_literal()?);
        }
        let rbr = self.expect(TokenKind::RBracket)?;
        r.span = lbr.span.join(rbr.span);
        Ok(r)
    }

    fn parse_rational_literal(&mut self) -> Result<RationalLiteral, ParseError> {
        let tok = self.peek().clone();
        let val = match &tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Rational64::from_integer(*n)
            }
            TokenKind::FloatLit(s) => {
                self.advance();
                parse_rational(&s).map_err(|e: String| self.error(&e))?
            }
            _ => return Err(self.error("expected a numeric literal")),
        };
        Ok(RationalLiteral { value: val, span: tok.span })
    }

    // ========================================================================
    // Statements
    // ========================================================================

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        let lb = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        let rb = self.expect(TokenKind::RBrace)?;
        Ok(Block { stmts, span: lb.span.join(rb.span) })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        // `@dispatch(...)` is a 2-line construct: the header line plus
        // the immediately-following `apply(...)` statement. We need to
        // peek past the `@` to decide which parser to dispatch to.
        if matches!(self.peek_kind(), TokenKind::At) {
            if self.is_dispatch_header() {
                return self.parse_dispatch_stmt();
            }
        }
        // `if_present(name) {... }` looks like a regular call but is
        // a statement-level compile-time branch. We
        // peek for the identifier form so we don't have to make
        // `if_present` a reserved keyword.
        if self.is_if_present_stmt() {
            return self.parse_if_present_stmt();
        }
        // `ownclude func name(...)... {... }` is a
        // statement-level construct that introduces a function-local
        // nested function. Detect the leading `ownclude` identifier
        // here so the expression parser doesn't try to interpret it
        // as a bare identifier call.
        if self.is_ownclude_stmt() {
            return self.parse_ownclude_stmt();
        }
        match self.peek_kind() {
            TokenKind::KwLet => self.parse_let(),
            TokenKind::KwReturn => self.parse_return(),
            TokenKind::KwIf => self.parse_if(),
            TokenKind::KwFor => self.parse_for(),
            TokenKind::KwWhile => self.parse_while(),
            TokenKind::KwBreak => {
                let span = self.advance().span;
                self.consume_optional_semicolon();
                Ok(Stmt::Break { span })
            }
            TokenKind::KwContinue => {
                let span = self.advance().span;
                self.consume_optional_semicolon();
                Ok(Stmt::Continue { span })
            }
            TokenKind::At => self.parse_annotation_stmt(),
            TokenKind::KwAssert => self.parse_assert_stmt(),
            _ => self.parse_expr_or_assign_stmt(),
        }
    }

    /// True if the next tokens are `ownclude func` (without consuming
    /// them). Used to route the nested-function form to its dedicated
    /// parser. We require the `func` keyword to follow so a call
    /// named `ownclude(...)` somewhere would still parse as an
    /// expression - the `func` lookahead is what makes `ownclude`
    /// unambiguous.
    fn is_ownclude_stmt(&self) -> bool {
        if let TokenKind::Ident(name) = &self.peek_kind() {
            if name == "ownclude" {
                if let Some(next) = self.tokens.get(self.pos + 1) {
                    if let TokenKind::KwFunc = &next.kind {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Parse `ownclude func name(params) [-> ret] { body }` as a
    /// statement inside the enclosing function body. The
    /// `enclosing_func_stack` must have a top entry - the
    /// enclosing public function's name - which we read here to
    /// stamp the nested function's `enclosing_func` field. The
    /// nested function's own body parsing pushes this name onto
    /// the stack for any further nested ownclude (one level of
    /// nesting is the language's use case, but the parser doesn't
    /// enforce a depth limit).
    fn parse_ownclude_stmt(&mut self) -> Result<Stmt, ParseError> {
        let ownclude_tok = self.expect_ident()?;
        debug_assert_eq!(ownclude_tok.name, "ownclude");
        let enclosing = match self.enclosing_func_stack.last() {
            Some(name) => name.clone(),
            None => {
                return Err(ParseError::Syntax {
                    message:
                        "`ownclude` must be inside a top-level `func` body, but no enclosing \
                         function was found on the parser stack"
                            .to_string(),
                    span: ownclude_tok.span,
                });
            }
        };
        // The visibility is fixed for the `ownclude` keyword. We do
        // not consult `parse_func_def_with_visibility_and_annots` for
        // the visibility parameter (it would default to Public) -
        // pass OwnPrivate explicitly.
        let mut func =
            self.parse_func_def_with_visibility_and_annots(Visibility::OwnPrivate, vec![])?;
        // Stamp the enclosing public function name. The function
        // body parser already pushed/popped the stack around the
        // inner body, so the stack is in its pre-`ownclude` state
        // by the time we get here.
        func.enclosing_func = Some(enclosing);
        Ok(Stmt::Ownclude(Box::new(func)))
    }

    /// True if the next tokens are `if_present (`, without consuming
    /// them. Used to route `if_present(name) {... }` to its dedicated
    /// parser before the expression parser sees it as a call.
    fn is_if_present_stmt(&self) -> bool {
        if let TokenKind::Ident(name) = &self.peek_kind() {
            if name == "if_present" {
                if let Some(next) = self.tokens.get(self.pos + 1) {
                    return matches!(next.kind, TokenKind::LParen);
                }
            }
        }
        false
    }

    /// Parse `if_present(name) {... } @absent {... }`.
    /// The `@absent` block is optional; if missing, the function
    /// continues normally when the param is absent.
    fn parse_if_present_stmt(&mut self) -> Result<Stmt, ParseError> {
        let ip = self.expect_ident()?; // `if_present`
        debug_assert_eq!(ip.name, "if_present");
        self.expect(TokenKind::LParen)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::RParen)?;
        let present_branch = self.parse_block()?;
        let absent_branch = if self.is_absent_block() {
            self.parse_absent_block()?
        } else {
            None
        };
        let span = ip.span.join(
            absent_branch
                .as_ref()
                .map(|b| b.span.clone())
                .unwrap_or_else(|| present_branch.span.clone()),
        );
        self.consume_optional_semicolon();
        Ok(Stmt::IfPresent {
            param: name.name,
            present_branch,
            absent_branch,
            span,
        })
    }

    /// True if the next tokens are `@absent {` (without consuming).
    fn is_absent_block(&self) -> bool {
        if !matches!(self.peek_kind(), TokenKind::At) {
            return false;
        }
        if let Some(next) = self.tokens.get(self.pos + 1) {
            if let TokenKind::Ident(name) = &next.kind {
                if name == "absent" {
                    if let Some(third) = self.tokens.get(self.pos + 2) {
                        return matches!(third.kind, TokenKind::LBrace);
                    }
                }
            }
        }
        false
    }

    /// Parse `@absent {... }` and return the inner block.
    fn parse_absent_block(&mut self) -> Result<Option<Block>, ParseError> {
        let at = self.expect(TokenKind::At)?;
        let name = self.expect_ident()?;
        debug_assert_eq!(name.name, "absent");
        let block = self.parse_block()?;
        let _ = at; // span is part of the block's own span
        Ok(Some(block))
    }

    /// True if the next tokens are `@dispatch` (without consuming them).
    /// Used to route `@dispatch(...)` to its dedicated parser before
    /// `parse_annotation_stmt` sees the `@` and complains about the
    /// missing `apply` companion.
    fn is_dispatch_header(&self) -> bool {
        if !matches!(self.peek_kind(), TokenKind::At) {
            return false;
        }
        // Look one token ahead without consuming.
        if let Some(next) = self.tokens.get(self.pos + 1) {
            if let TokenKind::Ident(name) = &next.kind {
                return name == "dispatch";
            }
        }
        false
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let lt = self.expect(TokenKind::KwLet)?;
        // `let name = expr;` or `let (a, b, c) = expr;`.
        // The destructuring form lets a multi-return call site name each
        // element of the returned tuple.
        let (names, mut ty) = if matches!(self.peek_kind(), TokenKind::LParen) {
            let lp = self.advance();
            let mut names = Vec::new();
            if !matches!(self.peek_kind(), TokenKind::RParen) {
                loop {
                    let n = self.expect_ident()?;
                    names.push(n.name);
                    if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            let rp = self.expect(TokenKind::RParen)?.span;
            if names.is_empty() {
                return Err(ParseError::Syntax {
                    message: "destructuring `let ()` is not allowed".into(),
                    span: lp.span.join(rp),
                });
            }
            (names, None)
        } else {
            let n = self.expect_ident()?;
            (vec![n.name], None)
        };
        if matches!(self.peek_kind(), TokenKind::Colon) {
            if names.len() != 1 {
                return Err(self.error(
                    "type annotation is only valid for single-binding `let`; \
                     destructured names get their types from the right-hand side",
                ));
            }
            self.advance();
            ty = Some(self.parse_type()?);
        }
        self.expect(TokenKind::Eq)?;
        let value = self.parse_expr()?;
        let span = lt.span.join(value.span());
        self.consume_optional_semicolon();
        Ok(Stmt::Let { names, ty, value, span })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let r = self.expect(TokenKind::KwReturn)?;
        let mut value = None;
        if !matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Semi | TokenKind::Eof) {
            value = Some(self.parse_expr()?);
        }
        self.consume_optional_semicolon();
        Ok(Stmt::Return { value, span: r.span.clone() })
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let i = self.expect(TokenKind::KwIf)?;
        let cond = self.parse_expr()?;
        let then_branch = self.parse_block()?;
        let else_branch = if matches!(self.peek_kind(), TokenKind::KwElse) {
            self.advance();
            if matches!(self.peek_kind(), TokenKind::KwIf) {
                let nested = self.parse_if()?;
                let nested_span = nested.span();
                Block { stmts: vec![nested], span: nested_span }
            } else {
                self.parse_block()?
            }
        } else {
            Block { stmts: vec![], span: cond.span() }
        };
        Ok(Stmt::If { cond, then_branch, else_branch, span: i.span.clone() })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let f = self.expect(TokenKind::KwFor)?;
        let var = self.expect_ident()?;
        self.expect(TokenKind::KwIn)?;
        // The iterated expression is parsed first either way: a range
        // continues with `..`, a list is simply what follows `in`
        // The old shape only recognised a list
        // when it started with `[`, which rejected `for x in xs`.
        let first = self.parse_expr()?;
        let iter = if matches!(self.peek_kind(), TokenKind::DotDot) {
            self.advance();
            let end = self.parse_expr()?;
            let step = if matches!(self.peek_kind(), TokenKind::KwStep) {
                self.advance();
                Some(self.parse_expr()?)
            } else {
                None
            };
            ForIter::Range { start: first, end, step }
        } else {
            ForIter::List(first)
        };
        let body = self.parse_block()?;
        Ok(Stmt::For { var: var.name, iter, body, span: f.span.clone() })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let w = self.expect(TokenKind::KwWhile)?;
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body, span: w.span.clone() })
    }

    fn parse_annotation_stmt(&mut self) -> Result<Stmt, ParseError> {
        let at_tok = self.expect(TokenKind::At)?;
        let name_tok = self.expect_ident()?;
        let name = name_tok.name.clone();
        let span = at_tok.span.join(name_tok.span.clone());
        match name.as_str() {
            "checkpoint" => {
                let n = self.expect_ident()?;
                self.consume_optional_semicolon();
                Ok(Stmt::Checkpoint { name: n.name, span })
            }
            "resume" => {
                let n = self.expect_ident()?;
                self.consume_optional_semicolon();
                Ok(Stmt::Resume { name: n.name, span })
            }
            "await" => {
                // `@await` is a marker; it takes one line and the
                // next statement must be a `spasio:input[...]("name")`
                // binding. The next-stmt shape is
                // checked in the semantic pass because the parser
                // can't see across statement boundaries cleanly.
                self.consume_optional_semicolon();
                Ok(Stmt::Await { span })
            }
            "before" => {
                // Two accepted forms per  // `@before F require;` (whitespace-separated)
                // `@before(F [, require]);` (paren / comma form, language reference)
                // Both produce the same `Stmt::OrderDecl` node.
                let (n, require) = if matches!(self.peek_kind(), TokenKind::LParen) {
                    self.advance();
                    let ident = self.expect_ident()?;
                    let require = if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                        self.consume_ident_if("require")
                    } else {
                        false
                    };
                    self.expect(TokenKind::RParen)?;
                    (ident, require)
                } else {
                    let ident = self.expect_ident()?;
                    let require = self.consume_ident_if("require");
                    (ident, require)
                };
                self.consume_optional_semicolon();
                Ok(Stmt::OrderDecl { kind: OrderKind::Before, target: n.name, require, span })
            }
            "after" => {
                let (n, require) = if matches!(self.peek_kind(), TokenKind::LParen) {
                    self.advance();
                    let ident = self.expect_ident()?;
                    let require = if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                        self.consume_ident_if("require")
                    } else {
                        false
                    };
                    self.expect(TokenKind::RParen)?;
                    (ident, require)
                } else {
                    let ident = self.expect_ident()?;
                    let require = self.consume_ident_if("require");
                    (ident, require)
                };
                self.consume_optional_semicolon();
                Ok(Stmt::OrderDecl { kind: OrderKind::After, target: n.name, require, span })
            }
            _ => Err(ParseError::Syntax {
                message: format!("unknown `@{}` statement", name),
                span,
            }),
        }
    }

    fn parse_expr_or_assign_stmt(&mut self) -> Result<Stmt, ParseError> {
        let expr = self.parse_expr()?;
        if matches!(self.peek_kind(), TokenKind::Eq) {
            if let Expr::Ident { name, span } = expr {
                self.advance();
                let value = self.parse_expr()?;
                let span = span.join(value.span());
                self.consume_optional_semicolon();
                return Ok(Stmt::Assign { name, value, span });
            }
            return Err(self.error("assignment target must be an identifier"));
        }
        self.consume_optional_semicolon();
        Ok(Stmt::Expr(expr.clone(), expr.span()))
    }

    // ========================================================================
    // @dispatch + apply
    // ========================================================================
    //
    // `@dispatch(tags = <sel>, bind = {...})` is followed by a single
    // statement that must be an `apply(...)` call (optionally with an
    // assignment target). Example:
    //
    // @dispatch(tags = {erosion}, bind = {strength: @tag(strength_factor)})
    // h = apply(pos, h)
    //
    // The parser consumes both lines and returns a single
    // `Stmt::Dispatch { decl, apply, span }`.

    fn parse_dispatch_stmt(&mut self) -> Result<Stmt, ParseError> {
        let at_tok = self.expect(TokenKind::At)?;
        let name_tok = self.expect_ident()?;
        if name_tok.name != "dispatch" {
            return Err(self.error("expected `@dispatch`"));
        }
        let header_span = at_tok.span.join(name_tok.span.clone());

        // Parse the parenthesised argument list: `tags =..., bind =...`.
        self.expect(TokenKind::LParen)?;
        let mut tags: Option<TagSelector> = None;
        let mut bind: Vec<DispatchBind> = Vec::new();
        loop {
            let key = self.expect_ident()?;
            self.expect(TokenKind::Eq)?;
            match key.name.as_str() {
                "tags" => tags = Some(self.parse_tag_selector()?),
                "bind" => bind = self.parse_dispatch_bind_list()?,
                other => {
                    return Err(ParseError::Syntax {
                        message: format!(
                            "unknown `@dispatch` argument `{}`; expected `tags` or `bind`",
                            other
                        ),
                        span: key.span,
                    });
                }
            }
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        let tags = tags.ok_or_else(|| {
            self.error("`@dispatch` requires a `tags = ...` argument")
        })?;

        // Consume the optional trailing semicolon on the header line.
        self.consume_optional_semicolon();

        let decl = DispatchDecl { tags, bind, span: header_span.clone() };

        // The following statement must be `name = apply(...)` or just
        // `apply(...)`. We parse it inline rather than re-calling
        // `parse_stmt` so we can return the assignment target on the
        // ApplyStmt.
        let apply = self.parse_apply_stmt()?;

        let span = header_span.join(apply.span.clone());
        Ok(Stmt::Dispatch { decl, apply, span })
    }

    /// Parse `{A, B, key=val,...}` (OR) or `[A, B,...]` (AND).
    /// The closing bracket is consumed; caller has already seen the
    /// opening one (because we got here via `tags = `).
    fn parse_tag_selector(&mut self) -> Result<TagSelector, ParseError> {
        match self.peek_kind() {
            TokenKind::LBrace => {
                self.advance();
                let items = self.parse_tag_item_list(TokenKind::RBrace)?;
                self.expect(TokenKind::RBrace)?;
                Ok(TagSelector::Or(items))
            }
            TokenKind::LBracket => {
                self.advance();
                let items = self.parse_tag_item_list(TokenKind::RBracket)?;
                self.expect(TokenKind::RBracket)?;
                Ok(TagSelector::And(items))
            }
            _ => Err(self.error(
                "expected `{...}` (OR) or `[...]` (AND) after `tags =`",
            )),
        }
    }

    /// Parse a comma-separated list of `TagItem`s until the close
    /// token. Empty list is allowed.
    fn parse_tag_item_list(&mut self, close: TokenKind) -> Result<Vec<TagItem>, ParseError> {
        let mut items = Vec::new();
        if matches!(
            self.peek_kind(),
            TokenKind::RBrace | TokenKind::RBracket
        ) {
            // empty selector - likely a user error but tolerated
            return Ok(items);
        }
        loop {
            items.push(self.parse_tag_item()?);
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        let _ = close; // close token is consumed by the caller
        Ok(items)
    }

    /// Parse one tag item: `name` (bare) or `name = literal` (valued).
    fn parse_tag_item(&mut self) -> Result<TagItem, ParseError> {
        let key = self.expect_ident()?;
        if matches!(self.peek_kind(), TokenKind::Eq) {
            self.advance();
            let val = self.parse_tag_value()?;
            Ok(TagItem::Valued(key.name, val, key.span))
        } else {
            Ok(TagItem::Bare(key.name))
        }
    }

    /// Parse `{param: value,...}` where value is either an arbitrary
    /// expression or `@tag(key)`.
    fn parse_dispatch_bind_list(&mut self) -> Result<Vec<DispatchBind>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut items = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RBrace) {
            loop {
                let name = self.expect_ident()?;
                self.expect(TokenKind::Colon)?;
                let source = self.parse_dispatch_bind_source()?;
                let span = name.span.join(source.span());
                items.push(DispatchBind { param: name.name, source, span });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(items)
    }

    fn parse_dispatch_bind_source(&mut self) -> Result<DispatchBindSource, ParseError> {
        if matches!(self.peek_kind(), TokenKind::At) {
            self.advance();
            let ident = self.expect_ident()?;
            if ident.name != "tag" {
                return Err(self.error("only `@tag(key)` references are allowed in `bind =`"));
            }
            self.expect(TokenKind::LParen)?;
            let key = self.expect_ident()?;
            self.expect(TokenKind::RParen)?;
            return Ok(DispatchBindSource::TagRef(key.name, ident.span.join(key.span)));
        }
        // Otherwise, a literal expression (a number, string, or
        // identifier constant). For MVP we only allow direct literals
        // and bare identifiers (forwarded as-is to the matched fn).
        let expr = self.parse_expr()?;
        Ok(DispatchBindSource::Literal(expr))
    }

    /// Parse the apply companion statement: `name = apply(args)` or
    /// `apply(args)`. The expression must be a call to `apply`.
    fn parse_apply_stmt(&mut self) -> Result<ApplyStmt, ParseError> {
        // Look-ahead one token to detect the assignment form: if the
        // current token is an `Ident` and the very next token is `=`,
        // the apply is the rhs of an assignment.
        if let TokenKind::Ident(_) = self.peek_kind().clone() {
            if matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::Eq)
            ) {
                let name_tok = self.expect_ident()?;
                self.advance(); // consume the `=`
                let call = self.parse_expr()?;
                self.consume_optional_semicolon();
                let span = name_tok.span.join(call.span());
                return Ok(ApplyStmt { target: Some(name_tok.name), call, span });
            }
        }
        // Plain `apply(args)` form - the callee is parsed as part of
        // the call expression.
        let call = self.parse_expr()?;
        let span = call.span();
        self.consume_optional_semicolon();
        Ok(ApplyStmt { target: None, call, span })
    }

    // ========================================================================
    // `assert` statement
    // ========================================================================
    //
    // Two accepted forms (callers always wrap them in `assert(...)`):
    //
    // assert(ident in RANGE) // range check
    // assert(approx(value, target, tolerance)) // approximation
    //
    // `RANGE` is a half-open or closed interval written with `[`/`{`
    // brackets: `[a, b]`, `{a, b}`, `[a, b}`, `{a, b]`. The opening and
    // closing brackets can be different - e.g. `[a, b}` is left-closed,
    // right-open - so we track inclusivity independently of which
    // bracket opened the range. `ponytail`: we do not allow a bare
    // expression or a `+`/`-` boundary; the language lists only the four
    // bracket forms with literal / identifier boundaries.

    fn parse_assert_stmt(&mut self) -> Result<Stmt, ParseError> {
        let kw = self.expect(TokenKind::KwAssert)?;
        let kw_span = kw.span.clone();
        self.expect(TokenKind::LParen)?;
        let kind = if matches!(self.peek_kind(), TokenKind::KwApprox) {
            // `assert(approx(value, target, tolerance))` form.
            self.advance();
            self.expect(TokenKind::LParen)?;
            let value = self.expect_ident()?.name;
            self.expect(TokenKind::Comma)?;
            let target = self.parse_expr()?;
            self.expect(TokenKind::Comma)?;
            let tolerance = self.parse_expr()?;
            let inner_rp = self.expect(TokenKind::RParen)?.span;
            let span = kw.span.join(inner_rp);
            AssertKind::Approx { value, target, tolerance, span }
        } else {
            // `assert(ident in RANGE)` form.
            let value_tok = self.expect_ident()?;
            self.expect(TokenKind::KwIn)?;
            let range = self.parse_range_bound()?;
            let span = kw.span.join(range.span.clone());
            AssertKind::InRange { value: value_tok.name, range, span }
        };
        let rp = self.expect(TokenKind::RParen)?;
        let span = kw_span.join(rp.span);
        self.consume_optional_semicolon();
        Ok(Stmt::Assert { kind, span })
    }

    /// Parse a half-open or closed interval used inside `assert(... in...)`.
    /// The opening bracket determines `lo_inclusive` (`[` = closed, `{` = open);
    /// the closing bracket determines `hi_inclusive` (mirrored). The four
    /// combinations in are:
    ///
    /// `[a, b]` closed-closed (both inclusive)
    /// `{a, b}` open-open (both exclusive)
    /// `[a, b}` left-closed, right-open
    /// `{a, b]` left-open, right-closed
    fn parse_range_bound(&mut self) -> Result<RangeBound, ParseError> {
        let (lo_open, lo_span) = match self.peek_kind() {
            TokenKind::LBracket => {
                let t = self.advance();
                (false, t.span)
            }
            TokenKind::LBrace => {
                let t = self.advance();
                (true, t.span)
            }
            _ => {
                return Err(self.error(
                    "expected `[` or `{` to start a range bound in `assert(... in ...)`",
                ))
            }
        };
        let lo = self.parse_expr()?;
        self.expect(TokenKind::Comma)?;
        let hi = self.parse_expr()?;
        let (hi_open, close_span) = match self.peek_kind() {
            TokenKind::RBracket => (false, self.advance().span),
            TokenKind::RBrace => (true, self.advance().span),
            _ => {
                return Err(self.error(
                    "expected `]` or `}` to close a range bound in `assert(... in ...)`",
                ))
            }
        };
        let span = lo_span.join(close_span);
        Ok(RangeBound {
            lo,
            hi,
            lo_inclusive: !lo_open,
            hi_inclusive: !hi_open,
            span,
        })
    }

    // ========================================================================
    // Expressions
    // ========================================================================

    pub fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek_kind(), TokenKind::PipePipe) {
            self.advance();
            let rhs = self.parse_and()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op: BinOp::Or, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_equality()?;
        while matches!(self.peek_kind(), TokenKind::AmpAmp) {
            self.advance();
            let rhs = self.parse_equality()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op: BinOp::And, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_comparison()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_additive()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_multiplicative()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_power()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_power()?;
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_power(&mut self) -> Result<Expr, ParseError> {
        let base = self.parse_unary()?;
        if matches!(self.peek_kind(), TokenKind::StarStar) {
            self.advance();
            let exp = self.parse_unary()?;
            let span = base.span().join(exp.span());
            return Ok(Expr::Binary { op: BinOp::Pow, lhs: Box::new(base), rhs: Box::new(exp), span });
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        // `spasio:input[mode](channel)` and `spasio:output[mode](value, channel)`
        // are atomic expressions. Detect the `spasio` identifier
        // followed by `:` here so the postfix path can stay focused on plain
        // calls, indexing, and field access.
        if self.is_spasio_call() {
            return self.parse_spasio_expr();
        }
        // `s2d:window(...)`, `s3d:box(...)`, `spio:read_text(...)` - playground
        // namespaces. Same `ns: func(args)` shape as spasio.
        if self.is_native_call() {
            return self.parse_native_expr();
        }
        // Async forms: `spawn f(args)`, `parallel f(args...)`
        // and `await(task_expr)`. Detected by identifier lookahead so the
        // reserved words stay plain `Ident`s (mirrors is_spasio_call).
        if self.is_async_spawn() {
            return self.parse_spawn_expr();
        }
        if self.is_async_await() {
            return self.parse_await_expr();
        }
        // Message-passing forms: `send(target, msg)`,
        // `broadcast(msg)` and `receive [Pattern{conds}]`.
        if self.is_msg_call() {
            return self.parse_msg_expr();
        }
        match self.peek_kind() {
            TokenKind::Minus => {
                let op = self.advance();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary { op: UnaryOp::Neg, expr: Box::new(inner), span: op.span })
            }
            TokenKind::Bang => {
                let op = self.advance();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary { op: UnaryOp::Not, expr: Box::new(inner), span: op.span })
            }
            _ => self.parse_postfix(),
        }
    }

    /// True if the next tokens are `spasio:`, without consuming them.
    /// Used to route the spasio I/O forms to their dedicated parser
    /// before the expression layer would otherwise try to parse
    /// `spasio` as a bare identifier.
    fn is_spasio_call(&self) -> bool {
        if let TokenKind::Ident(name) = &self.peek_kind() {
            if name == "spasio" {
                if let Some(next) = self.tokens.get(self.pos + 1) {
                    return matches!(next.kind, TokenKind::Colon);
                }
            }
        }
        false
    }

    /// True if the next tokens are `spawn <callee> (` / `parallel <callee> (`,
    /// without consuming.. The callee may be preceded by
    /// `@tracked` (or `@independent`) annotations, so we skip over them
    /// in the lookahead.
    fn is_async_spawn(&self) -> bool {
        if !matches!(&self.peek_kind(), TokenKind::Ident(name) if name == "spawn" || name == "parallel")
        {
            return false;
        }
        let mut i = self.pos + 1;
        // Skip optional @tracked / @independent annotations.
        while matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::At)) {
            i += 1;
            match self.tokens.get(i).map(|t| &t.kind) {
                Some(TokenKind::Ident(n)) if n == "tracked" || n == "independent" => i += 1,
                _ => return false,
            }
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Ident(_)))
            && matches!(self.tokens.get(i + 1).map(|t| &t.kind), Some(TokenKind::LParen))
    }

    /// True if the next tokens are `await (`, without consuming.
    fn is_async_await(&self) -> bool {
        if let TokenKind::Ident(name) = &self.peek_kind() {
            if name == "await" {
                if let Some(next) = self.tokens.get(self.pos + 1) {
                    return matches!(next.kind, TokenKind::LParen);
                }
            }
        }
        false
    }

    /// Parse `spawn f(args)` / `parallel f(args...)`.
    /// Optional `@tracked` / `@independent` annotations between the
    /// keyword and the callee are collected into `annotations`.
    fn parse_spawn_expr(&mut self) -> Result<Expr, ParseError> {
        let kw_tok = self.expect_ident()?;
        let is_parallel = kw_tok.name == "parallel";
        let start = kw_tok.span.clone();

        // Optional @tracked / @independent annotations.
        let mut annotations: Vec<Annotation> = Vec::new();
        while matches!(self.peek_kind(), TokenKind::At) {
            let at = self.advance();
            let name_tok = self.expect_ident()?;
            let ann_span = at.span.join(name_tok.span.clone());
            match name_tok.name.as_str() {
                "tracked" if !is_parallel => annotations.push(Annotation::Tracked { span: ann_span }),
                "independent" if is_parallel => annotations.push(Annotation::Independent { span: ann_span }),
                "tracked" | "independent" => {
                    return Err(self.error(&format!(
                        "`@{}` is only valid on `{}`",
                        name_tok.name,
                        if is_parallel { "parallel" } else { "spawn" }
                    )));
                }
                other => {
                    return Err(self.error(&format!(
                        "expected `tracked` or `independent` after `@` in async call, found `{}`",
                        other
                    )));
                }
            }
        }

        let callee_tok = self.expect_ident()?;
        let callee = Expr::Ident { name: callee_tok.name.clone(), span: callee_tok.span.clone() };
        self.expect(TokenKind::LParen)?;
        let mut args: Vec<CallArg> = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                let arg = self.parse_expr()?;
                let span = arg.span();
                args.push(CallArg { value: arg, span });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let rp = self.expect(TokenKind::RParen)?;
        let span = start.join(rp.span);
        if is_parallel {
            Ok(Expr::Parallel { func: Box::new(callee), args, annotations, span })
        } else {
            Ok(Expr::Spawn { func: Box::new(callee), args, annotations, span })
        }
    }

    /// True if the next tokens are `send (` or `broadcast (` or a bare
    /// `receive`, without consuming them. `receive` takes
    /// no parentheses, so the lookahead only checks the identifier.
    fn is_msg_call(&self) -> bool {
        match &self.peek_kind() {
            TokenKind::Ident(name) if name == "receive" => true,
            TokenKind::Ident(name) if name == "send" || name == "broadcast" => {
                matches!(self.tokens.get(self.pos + 1).map(|t| &t.kind), Some(TokenKind::LParen))
            }
            _ => false,
        }
    }

    /// Parse the message-passing forms:
    ///
    /// `send(target, Message{...})` -> Expr::Send
    /// `broadcast(Message{...})` -> Expr::Broadcast
    /// `receive` / `receive @await Alert{urgency > 5}` -> Expr::Receive
    fn parse_msg_expr(&mut self) -> Result<Expr, ParseError> {
        let kw = self.expect_ident()?;
        match kw.name.as_str() {
            "send" => {
                self.expect(TokenKind::LParen)?;
                let target_tok = self.expect_ident()?;
                self.expect(TokenKind::Comma)?;
                let message = self.parse_expr()?;
                let rp = self.expect(TokenKind::RParen)?;
                Ok(Expr::Send {
                    target: target_tok.name,
                    message: Box::new(message),
                    span: kw.span.join(rp.span),
                })
            }
            "broadcast" => {
                self.expect(TokenKind::LParen)?;
                let message = self.parse_expr()?;
                let rp = self.expect(TokenKind::RParen)?;
                Ok(Expr::Broadcast {
                    message: Box::new(message),
                    span: kw.span.join(rp.span),
                })
            }
            // `receive` - the optional `@await` marker is accepted (it is
            // the language's blocking spelling) and the optional
            // `Pattern{cond,...}` filter follows it.
            "receive" => {
                let mut span = kw.span.clone();
                if matches!(self.peek_kind(), TokenKind::At) {
                    self.advance();
                    let await_tok = self.expect_ident()?;
                    if await_tok.name != "await" {
                        return Err(self.error(
                            "only `@await` may follow `receive` in a sequence",
                        ));
                    }
                    span = span.join(await_tok.span);
                }
                // Optional `Pattern{... }` filter.
                if matches!(&self.peek_kind(), TokenKind::Ident(_)) {
                    let pattern = self.parse_receive_pattern()?;
                    span = span.join(pattern.span.clone());
                    Ok(Expr::Receive { pattern: Some(pattern), span })
                } else {
                    Ok(Expr::Receive { pattern: None, span })
                }
            }
            other => Err(self.error(&format!("unknown message form `{}`", other))),
        }
    }

    /// Parse `Pattern{ field op value,... }` - the filter half of a
    /// `receive`. Conditions are (field, comparison-op,
    /// expression) triples.
    fn parse_receive_pattern(&mut self) -> Result<ReceivePattern, ParseError> {
        let type_tok = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;
        let mut conditions: Vec<(String, BinOp, Expr)> = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RBrace) {
            loop {
                let field_tok = self.expect_ident()?;
                let op = match self.peek_kind() {
                    TokenKind::Gt => BinOp::Gt,
                    TokenKind::Lt => BinOp::Lt,
                    TokenKind::Ge => BinOp::Ge,
                    TokenKind::Le => BinOp::Le,
                    TokenKind::EqEq => BinOp::Eq,
                    TokenKind::NotEq => BinOp::Ne,
                    _ => {
                        return Err(self.error(
                            "receive pattern conditions need a comparison operator (`>`, `<`, `>=`, `<=`, `==`, `!=`)",
                        ))
                    }
                };
                self.advance();
                let value = self.parse_expr()?;
                conditions.push((field_tok.name, op, value));
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let rb = self.expect(TokenKind::RBrace)?;
        Ok(ReceivePattern {
            type_name: type_tok.name,
            conditions,
            span: type_tok.span.join(rb.span),
        })
    }

    /// Parse `await(task_expr)`.
    fn parse_await_expr(&mut self) -> Result<Expr, ParseError> {
        let kw = self.expect_ident()?;
        debug_assert_eq!(kw.name, "await");
        self.expect(TokenKind::LParen)?;
        let task = self.parse_expr()?;
        let rp = self.expect(TokenKind::RParen)?;
        Ok(Expr::Await { task: Box::new(task), span: kw.span.join(rp.span) })
    }

    /// Parse a spasio I/O expression. The caller has already confirmed
    /// the next tokens are `spasio:`. Two accepted shapes:
    ///
    /// `spasio:input[mode]("channel")` -> Expr::SpasioInput
    /// `spasio:output[mode](value, "channel")` -> Expr::SpasioOutput
    ///
    /// `mode` is one of `file`, `socket`, `memory`. The channel name is
    /// a string literal. `ponytail`: socket/memory modes parse but
    /// defer to the runtime - the MVP only emits a `file`-mode path
    /// (it just `fopen`s a file in `<data_dir>/input` / `<data_dir>/output`).
    fn parse_spasio_expr(&mut self) -> Result<Expr, ParseError> {
        let spasio_tok = self.expect_ident()?; // `spasio`
        debug_assert_eq!(spasio_tok.name, "spasio");
        self.expect(TokenKind::Colon)?;
        let kind_tok = self.expect_ident()?;
        match kind_tok.name.as_str() {
            "input" => self.parse_spasio_input(spasio_tok.span),
            "output" => self.parse_spasio_output(spasio_tok.span),
            other => Err(self.error(&format!(
                "expected `input` or `output` after `spasio:`, found `{}`",
                other
            ))),
        }
    }

    fn parse_spasio_mode(&mut self) -> Result<SpasioMode, ParseError> {
        self.expect(TokenKind::LBracket)?;
        let mode_tok = self.expect_ident()?;
        let mode = match mode_tok.name.as_str() {
            "file" => SpasioMode::File,
            "socket" => SpasioMode::Socket,
            "memory" => SpasioMode::Memory,
            other => {
                return Err(self.error(&format!(
                    "unknown spasio mode `{}`; expected `file`, `socket`, or `memory`",
                    other
                )));
            }
        };
        self.expect(TokenKind::RBracket)?;
        Ok(mode)
    }

    /// Parse `spasio:input[mode]("channel")`. Caller has
    /// already consumed `spasio:` and the `input` ident.
    fn parse_spasio_input(&mut self, spasio_span: Span) -> Result<Expr, ParseError> {
        let mode = self.parse_spasio_mode()?;
        self.expect(TokenKind::LParen)?;
        let channel_expr = self.parse_expr()?;
        let channel = match &channel_expr {
            Expr::StringLit { value, .. } => value.clone(),
            _ => {
                return Err(self.error(
                    "spasio:input channel name must be a string literal",
                ));
            }
        };
        let rp = self.expect(TokenKind::RParen)?;
        Ok(Expr::SpasioInput {
            mode,
            channel,
            span: spasio_span.join(rp.span),
        })
    }

    /// Parse `spasio:output[mode](value, "channel")`.
    /// Caller has already consumed `spasio:` and the `output` ident.
    fn parse_spasio_output(&mut self, spasio_span: Span) -> Result<Expr, ParseError> {
        let mode = self.parse_spasio_mode()?;
        self.expect(TokenKind::LParen)?;
        let value = self.parse_expr()?;
        self.expect(TokenKind::Comma)?;
        let channel_expr = self.parse_expr()?;
        let channel = match &channel_expr {
            Expr::StringLit { value, .. } => value.clone(),
            _ => {
                return Err(self.error(
                    "spasio:output channel name must be a string literal",
                ));
            }
        };
        let rp = self.expect(TokenKind::RParen)?;
        Ok(Expr::SpasioOutput {
            mode,
            value: Box::new(value),
            channel,
            span: spasio_span.join(rp.span),
        })
    }

    /// True if the next tokens are `<ns>:` where `<ns>` is one of
    /// `s2d`, `s3d` or `spio`, without consuming them.
    fn is_native_call(&self) -> bool {
        if let TokenKind::Ident(name) = &self.peek_kind() {
            if name == "s2d" || name == "s3d" || name == "spio" {
                if let Some(next) = self.tokens.get(self.pos + 1) {
                    return matches!(next.kind, TokenKind::Colon);
                }
            }
        }
        false
    }

    /// Parse `<namespace>:<func>(args...)` - a playground native call.
    /// Caller has confirmed `<ns>:`. Consumes `<ns>`, `:`, `<func>`,
    /// `(`, the argument list and `)`.
    fn parse_native_expr(&mut self) -> Result<Expr, ParseError> {
        let ns_tok = self.expect_ident()?;
        let namespace = ns_tok.name.clone();
        debug_assert!(namespace == "s2d" || namespace == "s3d" || namespace == "spio");
        self.expect(TokenKind::Colon)?;
        let func_tok = self.expect_ident()?;
        let func = func_tok.name.clone();
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                let arg = self.parse_expr()?;
                let span = arg.span();
                args.push(CallArg { value: arg, span });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let rp = self.expect(TokenKind::RParen)?;
        Ok(Expr::NativeCall {
            namespace,
            func,
            args,
            span: ns_tok.span.join(rp.span),
        })
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.advance();
                    let args = self.parse_call_args()?;
                    let rp = self.expect(TokenKind::RParen)?;
                    let is_random = matches!(
                        &expr,
                        Expr::Ident { name, .. } if name == "random"
                    );
                    let span = expr.span().join(rp.span.clone());
                    if is_random {
                        // `random(a, b)(args)` - the second
                        // argument list must touch the closing `)` of
                        // the first. A space makes it a value pick whose
                        // result is then called, which is an error, so
                        // adjacency is the discriminator.
                        let trailing = if matches!(self.peek_kind(), TokenKind::LParen)
                            && self.peek().span.start == rp.span.end
                        {
                            self.advance();
                            let trailing = self.parse_call_args()?;
                            let rp2 = self.expect(TokenKind::RParen)?;
                            Some((trailing, rp2.span))
                        } else {
                            None
                        };
                        let candidates: Vec<Expr> =
                            args.iter().map(|a| a.value.clone()).collect();
                        match trailing {
                            Some((trailing, rp2)) => {
                                expr = Expr::Random {
                                    candidates,
                                    args: Some(trailing),
                                    span: expr.span().join(rp2),
                                };
                            }
                            None => {
                                expr = Expr::Random {
                                    candidates,
                                    args: None,
                                    span,
                                };
                            }
                        }
                    } else {
                        expr = Expr::Call { callee: Box::new(expr), args, span };
                    }
                }
                TokenKind::Dot => {
                    self.advance();
                    let f = self.expect_ident()?;
                    let span = expr.span().join(f.span.clone());
                    expr = Expr::FieldAccess { target: Box::new(expr), field: f.name, span };
                }
                TokenKind::LBracket => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    let rb = self.expect(TokenKind::RBracket)?;
                    let span = expr.span().join(rb.span);
                    expr = Expr::Index { target: Box::new(expr), index: Box::new(idx), span };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse the argument list inside a call's parentheses. The
    /// caller has consumed the `(` and consumes the `)`.
    fn parse_call_args(&mut self) -> Result<Vec<CallArg>, ParseError> {
        let mut args = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                let arg = self.parse_expr()?;
                let span = arg.span();
                args.push(CallArg { value: arg, span });
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(Expr::IntLit { value: n, span: tok.span })
            }
            TokenKind::FloatLit(s) => {
                self.advance();
                let v = parse_rational(&s).map_err(|e: String| self.error(&e))?;
                Ok(Expr::FloatLit { value: v, span: tok.span })
            }
            TokenKind::StringLit(s) => {
                self.advance();
                Ok(Expr::StringLit { value: s, span: tok.span })
            }
            TokenKind::KwTrue => {
                self.advance();
                Ok(Expr::BoolLit { value: true, span: tok.span })
            }
            TokenKind::KwFalse => {
                self.advance();
                Ok(Expr::BoolLit { value: false, span: tok.span })
            }
            TokenKind::Ident(name) => {
                self.advance();
                // Struct literal: `TypeName{field: value,...}`.
                // The opening `{` distinguishes it from a function call
                // (which uses `(`) and from a bare identifier expression.
                //
                // Ambiguity: a `for` / `while` / `if` body also starts
                // with `{` (`for i in 0..n {... }`). Both look the same
                // to a one-token lookahead. We disambiguate by peeking
                // *inside* the `{`: a struct body starts with an
                // identifier and a colon (a field name), whereas a
                // control-flow body starts with a statement (let,
                // return, if, for, while, expression, `@`,...). If the
                // first token inside `{` is not `Ident:`, this is not
                // a struct literal -- leave the `{` in place for the
                // outer rule (for/while/if) to consume.
                if matches!(self.peek_kind(), TokenKind::LBrace)
                    && self.looks_like_struct_literal_body()
                {
                    self.expect(TokenKind::LBrace)?;
                    let mut fields: Vec<(String, Expr)> = Vec::new();
                    if !matches!(self.peek_kind(), TokenKind::RBrace) {
                        loop {
                            let fname = self.expect_ident()?;
                            self.expect(TokenKind::Colon)?;
                            let val = self.parse_expr()?;
                            fields.push((fname.name, val));
                            if matches!(self.peek_kind(), TokenKind::Comma) {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    let rb = self.expect(TokenKind::RBrace)?;
                    return Ok(Expr::StructLit {
                        name: name.clone(),
                        fields,
                        span: tok.span.join(rb.span),
                    });
                }
                Ok(Expr::Ident { name, span: tok.span })
            }
            TokenKind::LParen => {
                self.advance();
                // Distinguish parenthesised expression `(expr)` from tuple
                // literal `(a, b, c)`. We peek past the first
                // expression: if a `,` follows, the user wrote a tuple.
                let start_span = self.previous_span();
                let first = self.parse_expr()?;
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    let mut elements = vec![first];
                    while matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                        if matches!(self.peek_kind(), TokenKind::RParen) {
                            // Trailing comma is allowed: `(a, b,)`.
                            break;
                        }
                        elements.push(self.parse_expr()?);
                    }
                    let end_span = self.expect(TokenKind::RParen)?.span;
                    Ok(Expr::Tuple {
                        elements,
                        span: start_span.join(end_span),
                    })
                } else {
                    self.expect(TokenKind::RParen)?;
                    Ok(first)
                }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut elems = Vec::new();
                if !matches!(self.peek_kind(), TokenKind::RBracket) {
                    loop {
                        elems.push(self.parse_expr()?);
                        if matches!(self.peek_kind(), TokenKind::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Expr::ListLit { elements: elems, span: tok.span })
            }
            _ => Err(self.error(&format!(
                "expected an expression, found {}",
                tok.kind.debug_name()
            ))),
        }
    }

    /// Peek-ahead for the struct-literal disambiguation. Returns
    /// `true` if the next tokens look like a struct body, i.e. an
    /// `Ident:` pair (a field name + its type/value). The current
    /// token must already be `LBrace`; this method peeks at the token
    /// after it and the one after that.
    ///
    /// Used to distinguish `for i in 0..n { total = total + i }`
    /// (a `for` body) from `Point { x: 1, y: 2 }` (a struct literal).
    /// The one-token lookahead of the primary parser cannot tell them
    /// apart on `Ident LBrace` alone; the disambiguator looks one more
    /// token ahead. `ponytail`: a fully correct disambiguation would
    /// consult the symbol table (struct names are reserved), but the
    /// MVP has not done that yet, so we rely on the syntax. A future
    /// pass could replace this with a type-aware check.
    fn looks_like_struct_literal_body(&self) -> bool {
        // Caller has already advanced past the leading `Ident` and
        // confirmed the next token is `LBrace`. So `self.pos` points
        // at the `LBrace`; the first body token is at `pos + 1`, and
        // the token after that at `pos + 2`. We require the body to
        // start with `Ident:` (the first field's name + colon).
        //
        // `ponytail`: a fully correct disambiguation would consult
        // the symbol table (struct names are reserved), but the MVP
        // does not have that pass yet, so we rely on the syntax. A
        // future pass can replace this with a type-aware check.
        let lbrace_pos = self.pos;
        let first = lbrace_pos + 1;
        let second = lbrace_pos + 2;
        match (
            self.tokens.get(lbrace_pos),
            self.tokens.get(first),
            self.tokens.get(second),
        ) {
            (Some(lb), Some(a), Some(b)) => {
                matches!(lb.kind, TokenKind::LBrace)
                    && matches!(a.kind, TokenKind::Ident(_))
                    && matches!(b.kind, TokenKind::Colon)
                    // A reserved namespace prefix is never a struct
                    // name, so `<ns>:` at the head of the body means the
                    // body is a namespace call, not a field list. Without
                    // this, `while i < n { spasio:output[f](x, "c") }`
                    // reads the `n {` as a struct literal and fails to
                    // parse.
                    && !matches!(&a.kind, TokenKind::Ident(n) if is_namespace_name(n))
            }
            _ => false,
        }
    }

    // ========================================================================
    // Annotations
    // ========================================================================

    fn collect_leading_annotations(&mut self) -> Result<Vec<Annotation>, ParseError> {
        let mut out = Vec::new();
        while matches!(self.peek_kind(), TokenKind::At) {
            out.push(self.parse_annotation()?);
        }
        Ok(out)
    }

    fn parse_annotation(&mut self) -> Result<Annotation, ParseError> {
        let at = self.expect(TokenKind::At)?;
        let name = self.expect_ident()?;
        let span = at.span.join(name.span.clone());
        match name.name.as_str() {
            "gpu" => self.parse_gpu_annotation(span),
            "tag" => self.parse_tag_annotation(span),
            _ => Err(ParseError::Syntax {
                message: format!("unknown annotation `@{}`", name.name),
                span,
            }),
        }
    }

    fn parse_gpu_annotation(&mut self, span: Span) -> Result<Annotation, ParseError> {
        let mut workgroup = None;
        let mut local = None;
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.advance();
            if !matches!(self.peek_kind(), TokenKind::RParen) {
                loop {
                    let key = self.expect_ident()?;
                    self.expect(TokenKind::Eq)?;
                    match key.name.as_str() {
                        "workgroup" => {
                            let n = self.expect_int()?;
                            workgroup = Some(n as u32);
                        }
                        "local" => {
                            let s = self.expect_string()?;
                            local = Some(s);
                        }
                        _ => return Err(self.error(&format!("unknown @gpu attribute `{}`", key.name))),
                    }
                    if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen)?;
        }
        Ok(Annotation::Gpu { workgroup, local, span })
    }

    fn parse_tag_annotation(&mut self, span: Span) -> Result<Annotation, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut items = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                let key = self.expect_ident()?;
                if matches!(self.peek_kind(), TokenKind::Eq) {
                    self.advance();
                    let val = self.parse_tag_value()?;
                    items.push(TagItem::Valued(key.name, val, key.span));
                } else {
                    items.push(TagItem::Bare(key.name));
                }
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(Annotation::Tag { items, span })
    }

    fn parse_tag_value(&mut self) -> Result<TagValue, ParseError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Ok(TagValue::Number(Rational64::from_integer(n)))
            }
            TokenKind::FloatLit(s) => {
                self.advance();
                let v = parse_rational(&s).map_err(|e: String| self.error(&e))?;
                Ok(TagValue::Number(v))
            }
            TokenKind::StringLit(s) => {
                self.advance();
                Ok(TagValue::String(s))
            }
            TokenKind::KwTrue => {
                self.advance();
                Ok(TagValue::Bool(true))
            }
            TokenKind::KwFalse => {
                self.advance();
                Ok(TagValue::Bool(false))
            }
            // Bare identifiers in tag-value position are treated as
            // unquoted strings (e.g. `@tag(quality=high)` -> `quality=high`).
            // The tag-value grammar is a small surface and this keeps the
            // common "named category" usage ergonomic.
            TokenKind::Ident(s) => {
                self.advance();
                Ok(TagValue::String(s))
            }
            _ => Err(self.error("expected a literal value in @tag(key=...)")),
        }
    }

    // ========================================================================
    // Token helpers
    // ========================================================================

    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn previous_span(&self) -> Span {
        let idx = self.pos.saturating_sub(1);
        self.tokens[idx].span.clone()
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos.min(self.tokens.len() - 1)].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn is_eof(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        if std::mem::discriminant(self.peek_kind()) == std::mem::discriminant(&kind) {
            Ok(self.advance())
        } else {
            Err(self.error(&format!(
                "expected {}, found {}",
                kind.debug_name(),
                self.peek_kind().debug_name()
            )))
        }
    }

    fn expect_ident(&mut self) -> Result<IdentTok, ParseError> {
        let t = self.peek().clone();
        if let TokenKind::Ident(name) = t.kind {
            self.advance();
            Ok(IdentTok { name, span: t.span })
        } else {
            Err(self.error(&format!("expected identifier, found {}", t.kind.debug_name())))
        }
    }

    fn expect_int(&mut self) -> Result<i64, ParseError> {
        let t = self.peek().clone();
        if let TokenKind::IntLit(n) = t.kind {
            self.advance();
            Ok(n)
        } else {
            Err(self.error("expected integer literal"))
        }
    }

    fn expect_string(&mut self) -> Result<String, ParseError> {
        let t = self.peek().clone();
        if let TokenKind::StringLit(s) = t.kind {
            self.advance();
            Ok(s)
        } else {
            Err(self.error("expected string literal"))
        }
    }

    fn consume_ident_if(&mut self, name: &str) -> bool {
        if let TokenKind::Ident(n) = self.peek_kind() {
            if n == name {
                self.advance();
                return true;
            }
        }
        false
    }

    fn consume_optional_semicolon(&mut self) {
        if matches!(self.peek_kind(), TokenKind::Semi) {
            self.advance();
        }
    }

    fn error(&self, message: &str) -> ParseError {
        ParseError::Syntax { message: message.to_string(), span: self.peek().span.clone() }
    }
}

pub struct IdentTok {
    pub name: String,
    pub span: Span,
}

// ============================================================================
// Free helpers
// ============================================================================

/// Reserved namespace prefixes: `spasio` and the playground
/// namespaces `s2d` / `s3d` / `spio`. These are grammar-level keywords
/// in the sense that they may only appear as the `ns` of an `ns:func(...)`
/// call, so they can never name a struct or a struct field.
fn is_namespace_name(name: &str) -> bool {
    matches!(name, "spasio" | "s2d" | "s3d" | "spio")
}

fn apply_range(base: Type, range: RangeDecl) -> Result<Type, ParseError> {
    let span = range.span.clone();
    match base {
        Type::Float(_) => Ok(Type::Float(Some(range))),
        Type::Int(_) => Ok(Type::Int(Some(range))),
        Type::Uint(_) => Ok(Type::Uint(Some(range))),
        // /3.4: a uniform value-domain declaration
        // `vecN[min, max, step]` applies the same range to every
        // component. Per-component ranges (semicolon-separated) are
        // parsed in a later revision; the syntax is reserved by the
        // language but not implemented here.
        Type::Vec2(_, _) => Ok(Type::Vec2(Some(range.clone()), Some(range))),
        Type::Vec3(_, _, _) => Ok(Type::Vec3(Some(range.clone()), Some(range.clone()), Some(range))),
        Type::Vec4(_, _, _, _) => Ok(Type::Vec4(
            Some(range.clone()),
            Some(range.clone()),
            Some(range.clone()),
            Some(range),
        )),
        other => Err(ParseError::InvalidRange {
            message: format!("type `{}` does not accept a value-domain declaration", other.display_name()),
            span,
        }),
    }
}

/// Parse a numeric literal into an exact rational.
///
/// The literal is read the way the language defines it: the mantissa is
/// an integer and the fractional part is the denominator, so `0.1` is
/// `1/10` and not the nearest `f64`. The optimiser folds constants in
/// the same arithmetic, so it needs this exact reading rather than a
/// second one that could drift.
pub fn parse_rational(s: &str) -> Result<Rational64, String> {
    if s.is_empty() {
        return Err("empty numeric literal".to_string());
    }
    let lower = s.to_ascii_lowercase();
    let (mantissa, exp) = match lower.find('e') {
        Some(idx) => (&lower[..idx], Some(&lower[idx + 1..])),
        None => (lower.as_str(), None),
    };
    let denom: i64 = if let Some(dot) = mantissa.find('.') {
        let frac = &mantissa[dot + 1..];
        10_i64.pow(frac.len() as u32)
    } else {
        1
    };
    let numer_str: String = mantissa.chars().filter(|c| *c != '.' && *c != '_').collect();
    let numer: i64 = numer_str
        .parse()
        .map_err(|_| format!("invalid numeric literal `{}`", s))?;
    let mut r = if denom == 0 {
        return Err(format!("invalid rational `{}` (zero denominator)", s));
    } else {
        Rational64::new(numer, denom)
    };
    if let Some(e) = exp {
        let e: i32 = e.parse().map_err(|_| format!("invalid exponent in `{}`", s))?;
        if e >= 0 {
            r *= Rational64::from_integer(10_i64.pow(e as u32));
        } else {
            r /= Rational64::from_integer(10_i64.pow((-e) as u32));
        }
    }
    Ok(r)
}

// ============================================================================
// TokenKind debug helper
// ============================================================================

trait DebugName {
    fn debug_name(&self) -> String;
}
impl DebugName for TokenKind {
    fn debug_name(&self) -> String {
        match self {
            TokenKind::IntLit(n) => format!("int literal `{}`", n),
            TokenKind::FloatLit(s) => format!("float literal `{}`", s),
            TokenKind::StringLit(s) => format!("string literal `{:?}`", s),
            TokenKind::Ident(s) => format!("identifier `{}`", s),
            TokenKind::KwIf => "`if`".into(),
            TokenKind::KwElse => "`else`".into(),
            TokenKind::KwFor => "`for`".into(),
            TokenKind::KwIn => "`in`".into(),
            TokenKind::KwWhile => "`while`".into(),
            TokenKind::KwBreak => "`break`".into(),
            TokenKind::KwContinue => "`continue`".into(),
            TokenKind::KwStep => "`step`".into(),
            TokenKind::KwFunc => "`func`".into(),
            TokenKind::KwReturn => "`return`".into(),
            TokenKind::KwLet => "`let`".into(),
            TokenKind::KwStruct => "`struct`".into(),
            TokenKind::KwOut => "`out`".into(),
            TokenKind::KwTrue => "`true`".into(),
            TokenKind::KwFalse => "`false`".into(),
            TokenKind::KwFloat => "`float`".into(),
            TokenKind::KwInt => "`int`".into(),
            TokenKind::KwUint => "`uint`".into(),
            TokenKind::KwBool => "`bool`".into(),
            TokenKind::KwString => "`string`".into(),
            TokenKind::KwList => "`list`".into(),
            TokenKind::KwVec2 => "`vec2`".into(),
            TokenKind::KwVec3 => "`vec3`".into(),
            TokenKind::KwVec4 => "`vec4`".into(),
            TokenKind::KwMat3 => "`mat3`".into(),
            TokenKind::KwMat4 => "`mat4`".into(),
            TokenKind::KwTest => "`test`".into(),
            TokenKind::KwWith => "`with`".into(),
            TokenKind::KwAssert => "`assert`".into(),
            TokenKind::KwApprox => "`approx`".into(),
            TokenKind::LParen => "`(`".into(),
            TokenKind::RParen => "`)`".into(),
            TokenKind::LBrace => "`{`".into(),
            TokenKind::RBrace => "`}`".into(),
            TokenKind::LBracket => "`[`".into(),
            TokenKind::RBracket => "`]`".into(),
            TokenKind::Comma => "`,`".into(),
            TokenKind::Semi => "`;`".into(),
            TokenKind::Colon => "`:`".into(),
            TokenKind::Dot => "`.`".into(),
            TokenKind::DotDot => "`..`".into(),
            TokenKind::Arrow => "`->`".into(),
            TokenKind::Eq => "`=`".into(),
            TokenKind::At => "`@`".into(),
            TokenKind::Plus => "`+`".into(),
            TokenKind::Minus => "`-`".into(),
            TokenKind::Star => "`*`".into(),
            TokenKind::Slash => "`/`".into(),
            TokenKind::StarStar => "`**`".into(),
            TokenKind::EqEq => "`==`".into(),
            TokenKind::NotEq => "`!=`".into(),
            TokenKind::Lt => "`<`".into(),
            TokenKind::Le => "`<=`".into(),
            TokenKind::Gt => "`>`".into(),
            TokenKind::Ge => "`>=`".into(),
            TokenKind::AmpAmp => "`&&`".into(),
            TokenKind::PipePipe => "`||`".into(),
            TokenKind::Bang => "`!`".into(),
            TokenKind::Eof => "end of file".into(),
        }
    }
}

// ============================================================================
// Include resolution
// ============================================================================

impl Parser {
    /// Resolve `include` items in a parsed module by re-parsing the included
    /// files. Cycles raise E003. The include expansion is purely textual per
    /// 
    ///
    /// `expanded` is the shared set of files that have already been
    /// fully merged into some module in this resolution pass. A
    /// re-include of a file in this set is a silent no-op; an
    /// `include` that recurses into a file already on the resolution
    /// stack is an E003 cycle. The CLI passes an empty set on the
    /// first call; the recursive descent reuses it.
    pub fn resolve_includes(
        &mut self,
        module: Module,
        file_loader: &dyn FileLoader,
        expanded: &mut HashSet<PathBuf>,
    ) -> Result<Module, ParseError> {
        // Take ownership of the caller's `expanded` so the recursive
        // descent doesn't have to thread the parameter manually. The
        // set is restored to the caller on return so multiple top-level
        // files can share the include-guard knowledge.
        self.expanded = std::mem::take(expanded);
        let out = self.resolve_includes_inner(module, file_loader)?;
        *expanded = std::mem::take(&mut self.expanded);
        Ok(out)
    }

    /// Internal recursive worker. Splits the public entry point from
    /// the per-recursion step so the `expanded` set is only taken
    /// from the caller once.
    fn resolve_includes_inner(
        &mut self,
        module: Module,
        file_loader: &dyn FileLoader,
    ) -> Result<Module, ParseError> {
        let mut out_items = Vec::new();
        for item in module.items {
            if let Item::Include { path, span } = item {
                let resolved = self.base_dir.join(&path);
                let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved.clone());
                // E003 FIRST: a file on the include stack is *currently
                // being expanded* somewhere up the chain - reaching it
                // again before that expansion finishes would loop
                // forever, so we fail fast with the offending path.
                // This check must precede the `expanded` guard because
                // the project entry point pre-seeds top-level files
                // into `expanded` before their own expansion completes
                // (a -> b -> a would otherwise be silently cut instead
                // of reported).
                if self.include_stack.contains(&canonical) {
                    return Err(ParseError::CyclicInclude { path, span });
                }
                // a file that has already been fully
                // expanded in this resolution pass is a silent no-op
                // (the language mandates auto include-guard). This is
                // what makes `include` safe to use in multiple
                // source files without `pragma once` / `ifndef`
                // boilerplate.
                if self.expanded.contains(&canonical) {
                    continue;
                }
                let src = file_loader.load(&canonical).map_err(|e| ParseError::Syntax {
                    message: format!("include failed: {}", e),
                    span: span.clone(),
                })?;
                let tokens = spass_lexer::lex(&src, &canonical.display().to_string())
                    .map_err(|e| ParseError::Syntax { message: format!("{}", e), span: e.span() })?;
                let mut sub_parser = Self::from_tokens(tokens, canonical.parent().unwrap_or(Path::new(".")));
                // Hand the new sub-parser the parent's view of the
                // resolution stack and the shared expanded set so
                // its own recursive descent sees the same context.
                sub_parser.include_stack = self.include_stack.clone();
                sub_parser.expanded = self.expanded.clone();
                sub_parser.include_stack.insert(canonical.clone());
                let sub_module = sub_parser.parse_module()?;
                let expanded = sub_parser.resolve_includes_inner(sub_module, file_loader)?;
                out_items.extend(expanded.items);
                // Bubble the sub-parser's expanded set back up so
                // sibling branches of the include tree see the same
                // include-guard state. (`include_stack` is local to
                // the current chain so we don't need to merge it.)
                self.expanded.extend(sub_parser.expanded.iter().cloned());
                self.expanded.insert(canonical);
            } else {
                out_items.push(item);
            }
        }
        Ok(Module { items: out_items, source_file: module.source_file })
    }
}

/// Abstraction over filesystem access so tests can inject in-memory sources.
pub trait FileLoader {
    fn load(&self, path: &Path) -> Result<String, String>;
}

/// A parsed project and the source files the parse read.
pub struct ParsedProject {
    pub module: Module,
    /// The canonical path of every file the parse read: the top-level
    /// files and every file reached through `include`.
    ///
    /// A build cache must key on this set. A directory listing of
    /// `src/` misses every file pulled in through `include` from
    /// outside `src/`, and a cache keyed on the listing alone serves
    /// stale code that touching the `src/` files cannot fix.
    pub dependencies: Vec<PathBuf>,
}

/// Real filesystem-backed loader.
pub struct FsFileLoader;
impl FileLoader for FsFileLoader {
    fn load(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }
}
