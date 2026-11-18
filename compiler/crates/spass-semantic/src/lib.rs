//! Analyses semantics into symbol tables, checks, and `TypedModule`.

pub mod dispatch;

use spass_lexer::Span;
use spass_parser::ast::{Annotation, BinOp, DispatchBindSource, Expr, ForIter, FuncDef, Item, Module, SequenceDef, Stmt, StructDef, TagItem, Type, UnaryOp};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// A single semantic error. Error codes match the language.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum SemanticError {
    #[error("E002: @dispatch parameter binding missing for `{param}` at {span}")]
    DispatchBindMissing { param: String, span: Span },

    #[error("E004: @dispatch ordering cycle: {cycle} at {span}")]
    DispatchOrderCycle { cycle: String, span: Span },

    #[error("E005: undefined identifier `{name}` at {span}")]
    UndefinedIdent { name: String, span: Span },

    #[error("E006: type mismatch at {span}: {message}")]
    TypeMismatch { message: String, span: Span },

    #[error("E006: not callable at {span}: {message}")]
    NotCallable { message: String, span: Span },

    /// a `fileclude` function (visibility = FilePrivate)
    /// is only callable from the file it was defined in. A call
    /// from a function defined in another file is rejected at
    /// compile time so the visibility contract is enforced
    /// statically rather than via linker errors.
    #[error("E006: function `{name}` is `fileclude` and only visible within its source file `{defined_in}`; called from `{called_from}` at {span}")]
    FilePrivateCrossFile {
        name: String,
        defined_in: String,
        called_from: String,
        span: Span,
    },

    /// an `ownclude` function (visibility = OwnPrivate)
    /// is only callable from the body of the public function that
    /// contains its definition (`enclosing_func`). A call from
    /// anywhere else - another function, or even a different
    /// `ownclude` inside the same public function but outside the
    /// lexical chain - is rejected. The language scopes the closure
    /// to "the enclosing function body", not to the global
    /// namespace and not to the file.
    #[error("E006: function `{name}` is `ownclude` and only visible within `{enclosing}`; called from `{called_from}` at {span}")]
    OwnPrivateOutOfScope {
        name: String,
        enclosing: String,
        called_from: String,
        span: Span,
    },

    /// `@checkpoint name` must be globally unique within
    /// the module so a cross-function `@resume name` has exactly one
    /// target. The codegen layer relies on this when looking up the
    /// task in `spass_task_lookup`.
    #[error("E007: @checkpoint `{name}` is declared more than once at {span}")]
    CheckpointDuplicate { name: String, span: Span },

    /// a `@resume name` must reference a `@checkpoint`
    /// declared somewhere in the module. The check is at the module
    /// level because `@resume` is allowed to cross function boundaries
    /// (cross-function control flow).
    #[error("E007: @resume `{name}` has no matching @checkpoint in the module at {span}")]
    CheckpointUndefined { name: String, span: Span },

    /// `@resume` may not appear directly in the `else`
    /// branch of an `if` - the language wants symmetric control flow so
    /// the state machine can be re-entered deterministically. It is
    /// allowed in the `then` branch and in nested `if`s within `else`.
    #[error("E007: @resume `{name}` is not allowed in the `else` branch at {span}")]
    ResumeInElseBranch { name: String, span: Span },

    #[error("E008: recursive function `{name}` (cycle: {cycle}) at {span}")]
    Recursive { name: String, cycle: String, span: Span },

    #[error("E012: duplicate function name `{name}` at {span}")]
    DuplicateFunction { name: String, span: Span },

    #[error("E014: @dispatch tag selector matched no functions at {span}")]
    DispatchNoMatch { span: Span },

    #[error("W002: @dispatch bind parameter `{param}` unused by `{func}` at {span}")]
    DispatchBindUnused { param: String, func: String, span: Span },

    /// `if_present(name)` must reference a parameter of
    /// the enclosing function. Using a `let`-bound local or any other
    /// identifier is a compile-time error because the dispatcher has
    /// no compile-time way to know whether the caller supplied it.
    #[error("E016: if_present references `{name}` which is not a parameter of the enclosing function at {span}")]
    IfPresentNotParam { name: String, span: Span },

    /// a function can have `if_present` branches for at
    /// most one parameter - the codegen layer splits the function into
    /// exactly two variants (`<name>_present` / `<name>_absent`).
    /// More than one gated parameter would require 2^N variants, which
    /// the MVP doesn't support; rejecting it here keeps the dispatch
    /// contract a clean binary choice.
    #[error("E016: function has `if_present` branches for multiple parameters (`{name1}` and `{name2}`); only one is supported at {span}")]
    IfPresentMultiple { name1: String, name2: String, span: Span },

    /// a `@gpu` function cannot use `@await`. GPU code
    /// cannot suspend to wait for external input (the host/CPU side
    /// has to drive the dispatch), and Spass forbids the combination
    /// at compile time. The language numbers this E013.
    #[error("E013: @gpu function cannot use @await at {span}")]
    GpuWithAwait { span: Span },

    /// every `@await` site must be followed immediately
    /// by a `let v = spasio:input[mode]("channel")` binding. The
    /// pairing is what makes the function suspendable: the await site
    /// marks the suspension point, the next statement declares which
    /// channel to read on resume. We enforce the shape at the
    /// semantic layer so the codegen layer can rely on it.
    #[error("E013: @await must be immediately followed by `let v = spasio:input[...](\"channel\")` (got a different statement) at {span}")]
    AwaitNextStmtWrong { span: Span },

    /// the `spasio:input` form that follows `@await` must
    /// be a *single*-name `let` binding - no destructuring, no
    /// expression statement, no assignment. The codegen layer keys
    /// the awaited value off the bound name.
    #[error("E013: @await must be followed by a single-name `let` binding (got {got}) at {span}")]
    AwaitNextLetShape { got: String, span: Span },
}

pub type SemanticResult<T> = Result<T, SemanticError>;

/// A semantic warning. Warnings are non-fatal: the typed module is
/// still produced and the compiler continues. The CLI surfaces
/// warnings to the user (yellow bold) before running the build /
/// test / run command.
///
/// Today only W002 (unused @dispatch bind entry) is collected by
/// the pass; future revisions can lift other non-fatal conditions
/// (e.g. "function never called", "struct field never read") to
/// this enum without changing the public API.
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticWarning {
    /// W002: a `@dispatch(...)` declared a bind entry that the
    /// matched function doesn't take. Per language the dispatch is
    /// still valid (the extra bind is just ignored) but it almost
    /// certainly indicates a typo in the bind name.
    #[allow(dead_code)]
    DispatchBindUnused { param: String, func: String, span: Span },
}

impl std::fmt::Display for SemanticWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticWarning::DispatchBindUnused { param, func, span } => write!(
                f,
                "W002: @dispatch bind parameter `{}` unused by `{}` at {}",
                param, func, span
            ),
        }
    }
}

/// Output of [`analyze_full`]: the typed module plus any
/// non-fatal warnings collected during analysis. The plain
/// [`analyze`] wrapper discards the warnings for callers that
/// don't need them.
#[derive(Debug, Clone)]
pub struct AnalysisResult {
    pub module: TypedModule,
    pub warnings: Vec<SemanticWarning>,
}

/// A typed function. After semantic analysis each function has resolved
/// parameter types and a return type (or `None` for `out`-only functions).
#[derive(Debug, Clone)]
pub struct TypedFunc {
    pub name: String,
    pub params: Vec<TypedParam>,
    pub return_type: Option<Type>,
    pub multi_returns: Option<Vec<Type>>,
    pub is_gpu: bool,
    pub has_checkpoint: bool,
    pub has_resume: bool,
    pub has_await: bool,
    /// when the body contains an `@await`, the codegen
    /// layer needs to know the binding name (the `v` in
    /// `let v = spasio:input[mode]("channel")`) and the channel
    /// info so it can wire the suspension point. The semantic
    /// pass validates the next-stmt shape; this is the lift of
    /// that validated shape. `None` for non-awaiting functions.
    ///
    /// MVP allows at most one `@await` per function so the codegen
    /// layer can keep the state block shape a single Await slot.
    /// Multiple awaits would require a per-await state channel
    /// table - a future revision can lift the cap.
    pub await_binding: Option<AwaitBinding>,
    /// `true` when the body contains an `if_present`
    /// statement. Codegen reads this to decide whether to emit two
    /// function variants (`<name>_present` / `<name>_absent`) instead
    /// of one.
    pub has_if_present: bool,
    /// the name of the parameter that gates the
    /// `if_present` branches. Codegen needs this so the CLI
    /// dispatcher can decide which variant to call (the user
    /// either supplied the parameter or didn't). MVP allows at
    /// most one gated parameter per function - a function with
    /// `if_present` for two different parameters is rejected at
    /// the semantic layer (E016 variant) so the variant naming
    /// stays a binary split.
    pub if_present_param: Option<String>,
    /// Bare tag names declared on this function via `@tag(A, B,...)`.
    /// Used by `spass run` to resolve a tag name to a function symbol
    /// Valued tags (`key=value`) are not stored here -
    /// they are only consumed by `@dispatch` at compile time.
    pub tags: Vec<String>,
    /// (`ownclude`): names of variables from the
    /// enclosing public function's scope that this nested function
    /// captures by value at definition time ( /// variables from the enclosing scope are captured by value at definition time,
    /// not at call time).
    /// The codegen layer hoists the nested function into a static
    /// helper that takes the captured values as extra leading
    /// parameters, so the closure has a stable C/Rust ABI. The
    /// captured names must be either parameters of the enclosing
    /// function or `let`-bound locals that were defined *before*
    /// the `ownclude` statement in source order. Capturing a local
    /// that was defined after the `ownclude` would be a static
    /// error; the semantic pass rejects that case with E006.
    /// Empty for `func` / `fileclude` functions.
    ///
    /// Each entry is `(name, type)`. The type is lifted from the
    /// enclosing function's parameter list (for captured params)
    /// or the enclosing function's `let` binding (for captured
    /// locals). The codegen layer reads the type directly from
    /// the pair so it can emit the hoisted helper's signature
    /// without re-walking the enclosing function's body.
    pub captures: Vec<(String, Type)>,
    /// (`ownclude`): the function that contains this
    /// nested function in its body. `None` for top-level functions.
    /// Lifted from `FuncDef::enclosing_func` so the codegen layer
    /// doesn't have to re-walk the AST to find the call-site owner.
    pub enclosing_func: Option<String>,
    /// the source file the function was originally
    /// defined in. Used by the codegen layer to skip `fileclude`
    /// functions from the public symbol export ( /// when another file includes this file, `fileclude` functions do not
    /// enter the global namespace).
    pub source_file: String,
    pub span: Span,
    pub original: FuncDef,
}

#[derive(Debug, Clone)]
pub struct TypedParam {
    pub name: String,
    pub ty: Type,
    pub is_out: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypedStruct {
    pub name: String,
    pub fields: Vec<(String, Type)>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypedModule {
    pub functions: Vec<TypedFunc>,
    /// Name -> index into `functions`. Built once when the module is
    /// assembled, so a call-site lookup is a hash probe instead of a
    /// linear scan over every function. The codegen emitters resolve
    /// a call's callee twice per call expression, which made the
    /// scan cost O(functions x call sites) in string comparisons.
    func_index: HashMap<String, usize>,
    pub structs: Vec<TypedStruct>,
    /// top-level sequences. Each is an entry point with
    /// its own state block and message queue; the codegen layer emits
    /// one thread per sequence and the dispatcher starts them all.
    pub sequences: Vec<SequenceDef>,
    /// every spasio channel the program touches (deduped by
    /// mode+channel+direction). This is the machine-readable slot list
    /// that goes into `module.sps` so the runner and external programs
    /// know which channels exist and which direction data flows.
    pub channels: Vec<ChannelInfo>,
    pub source_file: String,
}

/// One spasio channel reference. `dir` says which way the
/// program pushes/pulls data on that channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelDir {
    /// The program reads via `spasio:input` (the outside world `set`s it).
    Input,
    /// The program writes via `spasio:output` (the outside world `get`s it).
    Output,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub mode: spass_parser::ast::SpasioMode,
    pub channel: String,
    pub dir: ChannelDir,
}

/// the validated `@await` companion binding. The
/// semantic pass enforces that `@await` is immediately followed by
/// `let <name> = spasio:input[<mode>]("<channel>")`; this struct
/// is the lift of that shape so the codegen layer doesn't have to
/// re-walk the body to find the bound name and channel.
#[derive(Debug, Clone)]
pub struct AwaitBinding {
    /// The local bound by the `let` (e.g. `v` in `let v =...`).
    /// The codegen layer reserves a state slot for this name so
    /// the value is preserved across the suspension point.
    pub name: String,
    /// The I/O mode of the `spasio:input` call (file/socket/memory).
    pub mode: spass_parser::ast::SpasioMode,
    /// The channel name (the string literal argument).
    pub channel: String,
    /// Span of the `@await` statement, for error reporting.
    pub span: Span,
}

impl Default for TypedModule {
    fn default() -> Self {
        TypedModule::new(Vec::new(), Vec::new(), Vec::new(), Vec::new(), String::new())
    }
}

impl TypedModule {
    /// Assemble a module and index its functions by name.
    pub fn new(
        functions: Vec<TypedFunc>,
        structs: Vec<TypedStruct>,
        sequences: Vec<SequenceDef>,
        channels: Vec<ChannelInfo>,
        source_file: String,
    ) -> Self {
        let func_index = functions
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        TypedModule {
            functions,
            func_index,
            structs,
            sequences,
            channels,
            source_file,
        }
    }

    /// Look up a function by name.
    pub fn func(&self, name: &str) -> Option<&TypedFunc> {
        self.func_index.get(name).map(|i| &self.functions[*i])
    }
}

// ============================================================================
// Symbol table
// ============================================================================

#[derive(Default)]
struct SymbolTable {
    /// Local bindings: name -> type. Used during expression type-checking.
    locals: Vec<HashMap<String, Type>>,
    /// Functions in this module.
    functions: HashMap<String, TypedFunc>,
    /// Structs in this module.
    structs: HashMap<String, TypedStruct>,
    /// every `sequence name {... }` declared in the
    /// module. Sequences are entry points, not callables, so they live
    /// in a separate namespace from functions. `send(target,...)` and
    /// the runtime's message routing both validate against this list;
    /// the codegen layer reads the bodies to emit one thread each.
    sequences: Vec<SequenceDef>,
    /// every spasio channel reference seen during body
    /// checking (deduped). Lifted onto the TypedModule for `module.sps`.
    spasio_channels: Vec<ChannelInfo>,
    /// Names of functions that have been entered but not yet exited; used
    /// to detect recursion.
    call_stack: Vec<String>,
    /// Functions that have been fully analysed.
    analysed: HashSet<String>,
    /// Every `@checkpoint name` declared anywhere in the module.
    /// Populated once before body checking starts; consulted by the
    /// `check_stmt(Stmt::Resume)` path.
    checkpoint_names: HashSet<String>,
    /// `true` while we are walking the `else` branch of an `if` at the
    /// top level. Nested `if`s inside an `else` reset the flag for
    /// their own branches. `@resume` is forbidden when this is `true`
    /// 
    in_else_branch: bool,
    /// Names of the function's parameters, captured on entry to
    /// `check_function` so the body checker can tell a parameter
    /// apart from a `let`-bound local. Used by `if_present`
    /// to reject a branch over a name that isn't a parameter.
    /// `ponytail`: a more aggressive form would store each param's
    /// type alongside the name; the MVP just needs set membership.
    func_params: HashSet<String>,
    /// The first parameter named by an `if_present(...)` statement
    /// inside the current function body. Subsequent `if_present`
    /// statements must name the same parameter (E016 otherwise) so
    /// the codegen layer can keep the variant split a clean binary
    /// choice. Reset on entry to `check_function`.
    first_if_present_param: Option<String>,
    /// Spans of every `@await` site inside the current function.
    /// The semantic pass collects these so the next-stmt shape
    /// check (and the codegen pass) can walk the body in source
    /// order. Reset on entry to `check_function`.
    await_sites: Vec<spass_lexer::Span>,
    /// The validated `@await` companion binding for the current
    /// function. Set when the body-walker sees an `@await` and
    /// confirms the next statement is `let v = spasio:input[mode]("name")`.
    /// `None` for non-awaiting functions. The MVP allows at most
    /// one `@await` per function so this is a single binding, not
    /// a list. Reset on entry to `check_function`.
    await_binding: Option<AwaitBinding>,
    /// Non-fatal warnings collected during analysis. The dispatch
    /// resolver pushes W002 entries here; [`analyze_full`] drains
    /// the vec on exit and surfaces them to the caller. Lives on
    /// the symbol table so the inner passes don't need a separate
    /// output parameter.
    warnings: Vec<SemanticWarning>,
    /// per-enclosing-function table of `ownclude`
    /// functions. Keyed by the owning public function's name
    /// because the language scopes the closure to that function's
    /// body. The MVP doesn't allow an `ownclude` inside another
    /// `ownclude` (the parser pushes the stack so deeper
    /// nestings are syntactically representable but the
    /// semantic pass treats only the outer public function as
    /// the scope owner).
    ownclude_funcs: HashMap<String, Vec<TypedFunc>>,
    /// name of the public function currently being
    /// checked. `None` outside any function body. The
    /// call-visibility check uses this to determine whether an
    /// `ownclude` is in scope. Set on entry to `check_function`,
    /// cleared on exit. The nested ownclude body walk inherits
    /// the same value - an ownclude is checked while its
    /// enclosing public function is on the "current" stack.
    current_enclosing: Option<String>,
    /// source file of the function currently being
    /// checked. Used by the call-visibility check to enforce
    /// `fileclude` (a `fileclude` function may only be called
    /// from a function in the same source file). Set on entry
    /// to `check_function`, cleared on exit. Empty outside a
    /// function body - the call-visibility check treats
    /// `fileclude` calls from empty contexts as out-of-scope.
    current_source_file: String,
}

impl SymbolTable {
    fn new() -> Self {
        Self::default()
    }

    /// Record a spasio channel reference (deduped by mode+channel+dir).
    /// Called from `check_expr` for every `spasio:input/output` site.
    pub fn record_channel(
        &mut self,
        mode: spass_parser::ast::SpasioMode,
        channel: String,
        dir: ChannelDir,
    ) {
        if !self
            .spasio_channels
            .iter()
            .any(|c| c.mode == mode && c.channel == channel && c.dir == dir)
        {
            self.spasio_channels.push(ChannelInfo { mode, channel, dir });
        }
    }

    fn push_scope(&mut self) {
        self.locals.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.locals.pop();
    }

    fn define(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.locals.last_mut() {
            scope.insert(name.to_string(), ty);
        }
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        for scope in self.locals.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    /// Look up a function by name. First checks the module-level
    /// public/fileclude table; then the ownclude table for the
    /// currently-active enclosing function. `None` when the
    /// function name is unknown in either table.
    fn lookup_func(&self, name: &str) -> Option<&TypedFunc> {
        if let Some(f) = self.functions.get(name) {
            return Some(f);
        }
        if let Some(enclosing) = &self.current_enclosing {
            if let Some(list) = self.ownclude_funcs.get(enclosing) {
                if let Some(f) = list.iter().find(|f| f.name == name) {
                    return Some(f);
                }
            }
        }
        None
    }

    /// Build a `TypedModule` snapshot from the current symbol table.
    /// Used by the dispatch resolver to look up matching functions
    /// and to consult their body for @before/@after orderings.
    fn as_typed_module(&self) -> TypedModule {
        let mut functions: Vec<TypedFunc> = self
            .functions
            .values()
            .filter(|f| !is_builtin_name(&f.name))
            .cloned()
            .collect();
        functions.sort_by_key(|f| f.span.start);
        TypedModule::new(
            functions,
            self.structs.values().cloned().collect(),
            self.sequences.clone(),
            self.spasio_channels.clone(),
            String::new(),
        )
    }
}

// ============================================================================
// Semantic analysis
// ============================================================================

/// Run semantic analysis and return the typed module plus any
/// non-fatal warnings collected along the way. This is the
/// "full" entry point: callers that need to surface warnings
/// (the CLI) call this directly; the plain [`analyze`] wrapper
/// discards the warnings and keeps the historical signature.
pub fn analyze_full(module: &Module) -> SemanticResult<AnalysisResult> {
    let mut sym = SymbolTable::new();
    register_builtins(&mut sym);

    // Pre-pass: collect every `@checkpoint name` declared anywhere in
    // the module. The set is consulted during body checking to validate
    // `@resume name` references. We also detect duplicate
    // checkpoint names here - the language requires module-wide uniqueness
    // so a cross-function `@resume` has exactly one target.
    //
    // `@checkpoint` is a statement, not a function-level
    // annotation, so we walk the function bodies in addition to looking
    // at `f.annotations`. Future language revisions might promote it to an
    // annotation; the walker handles both shapes.
    let mut checkpoint_names: HashSet<String> = HashSet::new();
    for item in &module.items {
        if let Item::Func(f) = item {
            collect_checkpoint_names(&f.body.stmts, &mut checkpoint_names)?;
        }
    }
    sym.checkpoint_names = checkpoint_names;

    // First pass: register all functions and structs.
    for item in &module.items {
        match item {
            Item::Func(f) => {
                let name = f.name.clone();
                if sym.functions.contains_key(&name) {
                    return Err(SemanticError::DuplicateFunction {
                        name,
                        span: f.span.clone(),
                    });
                }
                let typed = build_typed_func(f);
                sym.functions.insert(name, typed);
            }
            Item::Struct(s) => {
                register_struct(s, &mut sym)?;
            }
            Item::Sequence(s) => {
                // a sequence is an entry point, not a
                // callable. We register the body as a synthetic zero-arg,
                // no-return function sharing the sequence's name so the
                // regular function machinery checks it and the codegen
                // emits it through the standard function path; the
                // dispatcher starts one thread per sequence instead of
                // exposing it to `spass run` (the arm exists but is not
                // the intended entry). `ponytail`: a user function with
                // the same name collides here (E012) - sequences and
                // functions share the top-level entry namespace.
                if sym.sequences.iter().any(|q| q.name == s.name)
                    || sym.functions.contains_key(&s.name)
                {
                    return Err(SemanticError::DuplicateFunction {
                        name: s.name.clone(),
                        span: s.span.clone(),
                    });
                }
                sym.sequences.push(s.clone());
                let fd = FuncDef {
                    name: s.name.clone(),
                    params: Vec::new(),
                    return_type: None,
                    multi_returns: None,
                    body: s.body.clone(),
                    annotations: Vec::new(),
                    visibility: spass_parser::ast::Visibility::Public,
                    source_file: module.source_file.clone(),
                    enclosing_func: None,
                    span: s.span.clone(),
                };
                let typed = build_typed_func(&fd);
                sym.functions.insert(s.name.clone(), typed);
            }
            Item::Include { .. } => {
            // Includes are already resolved by the parser.
            }
            Item::Test(t) => {
                // `test "name" { body }` is a top-level declaration
                // collected by the `spass test` driver.
                // It is not part of the regular build path, so we do
                // not register it as a function - codegen skips it,
                // and `spass test` re-parses the test file directly to
                // walk the test items. We just type-check the body so
                // references to undefined locals surface here instead
                // of at test time.
                //
                // `ponytail`: the body runs in a fresh scope so test
                // parameters and `with` defaults do not leak between
                // tests. We do not enforce that asserts reference the
                // declared params - a future pass can check that.
                let _ = check_test_body(t, &mut sym)?;
            }
        }
    }

    // Second pass: type-check function bodies and detect recursion.
    let functions: Vec<TypedFunc> = sym
        .functions
        .values()
        .cloned()
        .map(|f| f)
        .collect();

    let mut typed_functions = Vec::new();
    for func in functions {
        if !sym.analysed.contains(&func.name) {
            check_function(&func, &mut sym)?;
        }
        if let Some(checked) = sym.functions.get(&func.name) {
            // Skip the synthetic built-in entries - they're only there so
            // the type-checker accepts calls to them. The codegen layer
            // knows the Spass->runtime mapping directly.
            if !is_builtin_name(&func.name) {
                let mut checked = checked.clone();
                // Lift the @await binding onto the typed function so
                // the codegen layer can read it without re-walking
                // the body. `check_function` stashed it on the
                // symbol table; we take it here and clear the
                // symbol table so the next function starts clean.
                checked.await_binding = sym.await_binding.take();
                typed_functions.push(checked);
            }
        }
    }

    // Preserve source order in the output.
    typed_functions.sort_by_key(|f| f.span.start);

    // merge the ownclude functions (registered by
    // `check_ownclude` during the second pass) into the typed
    // module so the codegen layer can find them via the
    // `TypedModule::functions` vec. The ownclude entry's
    // `original` field is the original `Stmt::Ownclude`-wrapped
    // `FuncDef`; the codegen layer reads `func.original.body`
    // and `func.captures` to emit the hoisted helper. The MVP
    // doesn't track source order for ownclude entries (they
    // are private to their enclosing function, not part of the
    // public ABI), so we append them in arbitrary order - the
    // codegen layer emits them in a single block after the
    // public functions.
    let mut ownclude_entries: Vec<TypedFunc> = Vec::new();
    for (_enclosing, list) in sym.ownclude_funcs.drain() {
        ownclude_entries.extend(list);
    }
    typed_functions.extend(ownclude_entries);

    // type-check each sequence body in its own scope.
    // A sequence has no parameters, so the only bindings are the
    // `let`s it declares (including messages from `receive`).
    let sequences: Vec<SequenceDef> = module
        .items
        .iter()
        .filter_map(|i| if let Item::Sequence(s) = i { Some(s.clone()) } else { None })
        .collect();
    for seq in &sequences {
        sym.push_scope();
        check_block(&seq.body, &mut sym)?;
        sym.pop_scope();
    }

    // The warnings vec is collected by the dispatch resolver via
    // the symbol table (see `SymbolTable::warnings`). Move it out
    // so the caller can surface W002 messages to the user.
    let warnings = std::mem::take(&mut sym.warnings);

    Ok(AnalysisResult {
        module: TypedModule::new(
            typed_functions,
            sym.structs.values().cloned().collect(),
            sequences,
            sym.spasio_channels.clone(),
            module.source_file.clone(),
        ),
        warnings,
    })
}

/// Historical thin wrapper: runs [`analyze_full`] and discards
/// warnings. Most test sites and the codegen test helpers use
/// this signature; the CLI uses [`analyze_full`] directly so it
/// can print warnings to the user.
pub fn analyze(module: &Module) -> SemanticResult<TypedModule> {
    analyze_full(module).map(|r| r.module)
}

/// Names of the built-in functions registered in [`register_builtins`].
/// Kept in sync with that function by hand.
const BUILTIN_NAMES: &[&str] = &[
    "sin", "cos", "tan", "sqrt", "abs", "floor", "ceil", "round", "exp", "log",
    "pow", "min", "max", "to_int", "to_float",
    // interpolation built-ins.
    "lerp", "clamp", "smoothstep",
    // Vector / matrix built-ins. The codegen layer
    // dispatches the dimension from the first argument's type, so
    // the signature registered here only needs to be type-correct
    // for the semantic pass.
    "dot", "length", "normalize", "hadamard", "cross", "transpose",
    // Vec/mat constructors. The codegen layer picks `spass_mk_vec*`
    // by arity; the registered signature is `vec3` so the semantic
    // pass accepts the call shape.
    "mk_vec2", "mk_vec3", "mk_vec4",
    // procedural noise. `noise(x, z)` is binary,
    // `fbm(x, z, octaves, gain)` is quaternary.
    "noise", "fbm",
    // Spass playground namespaces (s2d:/s3d:/spio:). Runtime-provided,
    // so they are excluded from the user-function export list.
    "s2d:window", "s2d:open", "s2d:present", "s2d:clear", "s2d:pixel",
    "s2d:line", "s2d:rect", "s2d:frame", "s2d:circle", "s2d:ellipse",
    "s2d:tri", "s2d:text", "s2d:text_width", "s2d:text_height",
    "s2d:label", "s2d:button", "s2d:slider", "s2d:checkbox",
    "s2d:mouse_x", "s2d:mouse_y", "s2d:mouse_dx", "s2d:mouse_dy",
    "s2d:mouse_down", "s2d:wheel", "s2d:key", "s2d:time", "s2d:dt",
    "s3d:window", "s3d:open", "s3d:present", "s3d:clear", "s3d:camera",
    "s3d:light", "s3d:depth", "s3d:tri", "s3d:quad", "s3d:box",
    "s3d:grid", "s3d:push", "s3d:pop", "s3d:translate", "s3d:rotate",
    "s3d:scale", "s3d:sphere", "s3d:cylinder",
    "s3d:mouse_x", "s3d:mouse_y", "s3d:mouse_dx",
    "s3d:mouse_dy", "s3d:mouse_down", "s3d:key", "s3d:time", "s3d:dt",
    "spio:print", "spio:print_num", "spio:read_text", "spio:write_text",
    "spio:append_text", "spio:read_line", "spio:line_count",
    "spio:replace_text", "spio:read_f64", "spio:write_f64",
    "spio:append_f64", "spio:exists", "spio:size", "spio:remove",
    "spio:mkdir", "spio:copy", "spio:rename", "spio:list_count",
    "spio:list_name",
];

fn is_builtin_name(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

fn build_typed_func(f: &FuncDef) -> TypedFunc {
    let is_gpu = f.annotations.iter().any(|a| matches!(a, Annotation::Gpu { .. }));
    // `@checkpoint` and `@resume` are body statements,
    // not function-level annotations, so the only reliable way to set
    // the `has_*` flags is to walk the body. We also fall back to the
    // annotations list in case a future language revision promotes them
    // back to function-level annotations; the OR keeps both shapes
    // working.
    let has_checkpoint = body_has_stmt(f, |s| matches!(s, Stmt::Checkpoint { .. }))
        || f.annotations
            .iter()
            .any(|a| matches!(a, Annotation::Checkpoint { .. }));
    let has_resume = body_has_stmt(f, |s| matches!(s, Stmt::Resume { .. }))
        || f.annotations
            .iter()
            .any(|a| matches!(a, Annotation::Resume { .. }));
    let has_await = body_has_stmt(f, |s| matches!(s, Stmt::Await { .. }));
    // The body-walker form (`Stmt::Await`) is the only signal the
    // codegen layer needs. There used to be a parallel
    // `Annotation::Await` (function-level) variant; it was never
    // reachable from the parser and is now removed.
    let has_if_present = body_has_stmt(f, |s| matches!(s, Stmt::IfPresent { .. }));
    // The if_present_param is the name of the parameter that gates
    // the present/absent variant split. The MVP allows only one
    // gated parameter per function (E016 enforces this at check time
    // so we can lift the param name here without bailing).
    let if_present_param = collect_if_present_param(f);
    // Lift bare tag names from `@tag(A, B, key=val,...)` onto the typed
    // func. The keys for valued items are not stored here - the dispatch
    // path resolves them via `original.annotations`. Multiple identical
    // bare names in a single `@tag` declaration are collapsed.
    let mut tags: Vec<String> = Vec::new();
    for ann in &f.annotations {
        if let Annotation::Tag { items, .. } = ann {
            for item in items {
                if let TagItem::Bare(n) = item {
                    if !tags.contains(n) {
                        tags.push(n.clone());
                    }
                }
            }
        }
    }
    let params: Vec<TypedParam> = f
        .params
        .iter()
        .map(|p| TypedParam {
            name: p.name.clone(),
            ty: p.ty.clone(),
            is_out: p.is_out,
            span: p.span.clone(),
        })
        .collect();
    TypedFunc {
        name: f.name.clone(),
        params,
        return_type: f.return_type.clone(),
        multi_returns: f.multi_returns.clone(),
        is_gpu,
        has_checkpoint,
        has_resume,
        has_await,
        // Lifted onto the typed function by `analyze` after
        // `check_function` runs. `build_typed_func` doesn't have
        // access to the body-walked binding, so it always starts
        // at `None`; the second pass in `analyze` takes the
        // binding off the symbol table and writes it in.
        await_binding: None,
        has_if_present,
        if_present_param,
        tags,
        // filled in by `check_ownclude` for
        // `ownclude` functions; the top-level first pass leaves
        // it empty. The codegen layer reads the field to emit
        // the hoisted helper with the right capture parameters.
        captures: Vec::new(),
        // lifted from the AST. `None` for top-level
        // `func` / `fileclude`; the parser stamps it for
        // `ownclude` (see `parse_ownclude_stmt`).
        enclosing_func: f.enclosing_func.clone(),
        // lifted from the AST. The parser stamps the
        // source file on every `FuncDef` it produces; the codegen
        // layer uses this to skip `fileclude` from the public
        // symbol export.
        source_file: f.source_file.clone(),
        span: f.span.clone(),
        original: f.clone(),
    }
}

/// Walk a function body and return the name of the parameter named
/// in the first `if_present(...)` statement. The
/// semantic pass enforces that all `if_present` statements in a
/// single function name the same parameter (E016 otherwise), so
/// the first one is enough for codegen to decide which variant to
/// emit.
fn collect_if_present_param(f: &FuncDef) -> Option<String> {
    fn walk(stmts: &[Stmt], out: &mut Option<String>) {
        for s in stmts {
            if out.is_some() {
                return;
            }
            match s {
                Stmt::IfPresent { param, .. } => {
            *out = Some(param.clone());
                }
                Stmt::If { then_branch, else_branch, .. } => {
                    walk(&then_branch.stmts, out);
                    walk(&else_branch.stmts, out);
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => {
                    walk(&body.stmts, out);
                }
                _ => {}
            }
        }
    }
    let mut out = None;
    walk(&f.body.stmts, &mut out);
    out
}

/// Walk a function body and return true if any statement (recursively,
/// through `if`/`for`/`while` blocks) matches `pred`. Used by
/// [`build_typed_func`] to set the `has_checkpoint` / `has_resume`
/// flags without relying on annotations that may not exist in this
/// form on the AST.
fn body_has_stmt<F: Fn(&Stmt) -> bool>(f: &FuncDef, pred: F) -> bool {
    fn walk<F: Fn(&Stmt) -> bool>(stmts: &[Stmt], pred: &F) -> bool {
        for s in stmts {
            if pred(s) {
                return true;
            }
            match s {
                Stmt::If { then_branch, else_branch, .. } => {
                    if walk(&then_branch.stmts, pred) {
                        return true;
                    }
                    if walk(&else_branch.stmts, pred) {
                        return true;
                    }
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => {
                    if walk(&body.stmts, pred) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    walk(&f.body.stmts, &pred)
}

fn register_struct(s: &StructDef, sym: &mut SymbolTable) -> SemanticResult<()> {
    let fields: Vec<(String, Type)> = s
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.ty.clone()))
        .collect();
    let name = s.name.clone();
    let span = s.span.clone();
    // a list is a value with a runtime length. A struct
    // crosses the generated ABI by value and is `Copy` on the Rust
    // side, so a list field would make the struct neither. The list
    // surface is function-local for now; the ABI upgrade path is a
    // typed list descriptor in the manifest.
    for (fname, fty) in &fields {
        if type_contains_list(fty) {
            return Err(SemanticError::TypeMismatch {
                message: format!(
                    "struct field `{}` cannot be a list yet (`list` values are function-local)",
                    fname
                ),
                span: span.clone(),
            });
        }
    }
    if sym.structs.contains_key(&name) {
        return Err(SemanticError::TypeMismatch {
            message: format!("duplicate struct name `{}`", name),
            span,
        });
    }
    sym.structs.insert(
        name,
        TypedStruct {
            name: s.name.clone(),
            fields,
            span,
        },
    );
    Ok(())
}

// ============================================================================
// Built-ins
// ============================================================================
//
// These names resolve to the `spass_*` runtime symbols. We register them as
// ordinary `TypedFunc` entries so the call-site type-checker treats them
// the same as user-defined functions. The codegen layer maps the Spass
// name to the runtime symbol (see `spass-codegen`'s `BUILTIN_MAP`).

fn register_builtins(sym: &mut SymbolTable) {
    let dummy = spass_lexer::Span::dummy();
    let f_param = |name: &str| TypedParam {
        name: name.to_string(),
        ty: Type::Float(None),
        is_out: false,
        span: dummy.clone(),
    };
    let unary = |name: &str| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("x")],
        return_type: Some(Type::Float(None)),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(Type::Float(None)),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            // Builtins are runtime-provided symbols; they have no
            // Spass source file or enclosing function. Visibility
            // is `Public` because the symbol is registered
            // globally in the symbol table and is callable from
            // any user function.
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    let unary_int = |name: &str| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("x")],
        return_type: Some(Type::Int(None)),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(Type::Int(None)),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    for n in ["sin", "cos", "tan", "sqrt", "abs", "floor", "ceil", "round", "exp", "log"] {
        sym.functions.insert(n.to_string(), unary(n));
    }
    // Conversion builtins. The codegen layer maps these names to
    // the C `((int32_t)(x))` / `((double)(x))` cast expressions.
    sym.functions.insert("to_int".to_string(), unary_int("to_int"));
    sym.functions.insert("to_float".to_string(), unary("to_float"));
    let binary = |name: &str| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("a"), f_param("b")],
        return_type: Some(Type::Float(None)),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(Type::Float(None)),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            // Builtins are runtime-provided symbols; they have no
            // Spass source file or enclosing function. Visibility
            // is `Public` because the symbol is registered
            // globally in the symbol table and is callable from
            // any user function.
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    for n in ["pow", "min", "max"] {
        sym.functions.insert(n.to_string(), binary(n));
    }
    // interpolation built-ins. `clamp` is what the game
    // logic (and any world-bounds rule) needs.
    let ternary = |name: &str| {
        let mut f = binary(name);
        f.params = vec![f_param("a"), f_param("b"), f_param("c")];
        f.original.params = vec![];
        f
    };
    sym.functions.insert("lerp".to_string(), ternary("lerp"));
    sym.functions.insert("clamp".to_string(), ternary("clamp"));
    sym.functions.insert("smoothstep".to_string(), ternary("smoothstep"));

    // vector / matrix built-ins. The semantic pass
    // only needs to know the names exist and have the right
    // arity; the codegen layer dispatches on the first
    // argument's type. Signatures here are vec3 for unary and
    // (vec3, vec3) for binary - the MVP only needs a single
    // type-correct shape because the codegen layer rewrites the
    // call site to the dimension-specific runtime symbol.
    let f_vec_param = |name: &str, ty: Type| TypedParam {
        name: name.to_string(),
        ty,
        is_out: false,
        span: dummy.clone(),
    };
    let unary_vec = |name: &str, arg_ty: Type, ret_ty: Type| TypedFunc {
        name: name.to_string(),
        params: vec![f_vec_param("v", arg_ty)],
        return_type: Some(ret_ty.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret_ty),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    let binary_vec = |name: &str, ret_ty: Type| TypedFunc {
        name: name.to_string(),
        params: vec![
            f_vec_param("a", Type::Vec3(None, None, None)),
            f_vec_param("b", Type::Vec3(None, None, None)),
        ],
        return_type: Some(ret_ty.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret_ty),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    // `length(vec3) -> float` - codegen rewrites to spass_length3.
    sym.functions.insert("length".to_string(),
        unary_vec("length", Type::Vec3(None, None, None), Type::Float(None)));
    // `normalize(vec3) -> vec3` - codegen rewrites to spass_normalize3.
    sym.functions.insert("normalize".to_string(),
        unary_vec("normalize", Type::Vec3(None, None, None), Type::Vec3(None, None, None)));
    // `transpose(mat3) -> mat3` - codegen rewrites to spass_transpose3.
    sym.functions.insert("transpose".to_string(),
        unary_vec("transpose", Type::Mat3, Type::Mat3));
    // `dot(vec3, vec3) -> float` - codegen rewrites to spass_dot3.
    sym.functions.insert("dot".to_string(),
        binary_vec("dot", Type::Float(None)));
    // `hadamard(vec3, vec3) -> vec3` - codegen rewrites to spass_hadamard3.
    sym.functions.insert("hadamard".to_string(),
        binary_vec("hadamard", Type::Vec3(None, None, None)));
    // `cross(vec3, vec3) -> vec3` - codegen rewrites to spass_cross.
    sym.functions.insert("cross".to_string(),
        binary_vec("cross", Type::Vec3(None, None, None)));

    // ========================================================================
    // Spass playground built-ins (s2d: / s3d: / spio:). Registered under
    // their namespaced symbol so `s2d:window(...)` etc. type-check. The
    // codegen layer emits `spass_s2d_window(...)`,... directly.
    // ========================================================================
    let s_param = |name: &str| TypedParam {
        name: name.to_string(),
        ty: Type::String,
        is_out: false,
        span: dummy.clone(),
    };
    let b_param = |name: &str| TypedParam {
        name: name.to_string(),
        ty: Type::Bool,
        is_out: false,
        span: dummy.clone(),
    };
    // Counts and indices are integers: a line count or a directory
    // index has no fractional part, and making them `float` forces the
    // caller to convert before every comparison.
    let i_param = |name: &str| TypedParam {
        name: name.to_string(),
        ty: Type::Int(None),
        is_out: false,
        span: dummy.clone(),
    };
    let pf = |name: &str, params: Vec<TypedParam>, ret: Type| TypedFunc {
        name: name.to_string(),
        params,
        return_type: Some(ret.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        captures: vec![],
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: spass_parser::ast::FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    let ff = |name: &str, n: usize, ret: Type| {
        pf(name, (0..n).map(|i| f_param(&format!("a{}", i))).collect(), ret)
    };
    // ---- spass2d -----------------------------------------------------------
    sym.functions.insert("s2d:window".to_string(), pf("s2d:window",
        vec![f_param("w"), f_param("h"), s_param("title")], Type::Bool));
    sym.functions.insert("s2d:open".to_string(), ff("s2d:open", 0, Type::Bool));
    sym.functions.insert("s2d:present".to_string(), ff("s2d:present", 0, Type::Bool));
    sym.functions.insert("s2d:clear".to_string(), ff("s2d:clear", 3, Type::Bool));
    // The 2D primitives draw into the overlay layer, which carries an
    // alpha per pixel: `a` is 0 (invisible) to 1 (opaque).
    sym.functions.insert("s2d:pixel".to_string(), ff("s2d:pixel", 6, Type::Bool));
    sym.functions.insert("s2d:line".to_string(), ff("s2d:line", 8, Type::Bool));
    sym.functions.insert("s2d:rect".to_string(), ff("s2d:rect", 8, Type::Bool));
    sym.functions.insert("s2d:frame".to_string(), ff("s2d:frame", 8, Type::Bool));
    sym.functions.insert("s2d:circle".to_string(), ff("s2d:circle", 7, Type::Bool));
    sym.functions.insert("s2d:ellipse".to_string(), pf("s2d:ellipse",
        vec![f_param("cx"), f_param("cy"), f_param("rx"), f_param("ry"),
             f_param("r"), f_param("g"), f_param("b"), f_param("a")], Type::Bool));
    sym.functions.insert("s2d:tri".to_string(), pf("s2d:tri",
        vec![f_param("x0"), f_param("y0"), f_param("x1"), f_param("y1"), f_param("x2"), f_param("y2"),
             f_param("r"), f_param("g"), f_param("b"), f_param("a")], Type::Bool));
    sym.functions.insert("s2d:text".to_string(), pf("s2d:text",
        vec![f_param("x"), f_param("y"), f_param("scale"), f_param("r"), f_param("g"), f_param("b"), f_param("a"), s_param("text")], Type::Bool));
    // The text metrics: the advance width of the widest line and the
    // height of one line, both in pixels.
    sym.functions.insert("s2d:text_width".to_string(), pf("s2d:text_width",
        vec![s_param("text"), f_param("scale")], Type::Float(None)));
    sym.functions.insert("s2d:text_height".to_string(), ff("s2d:text_height", 1, Type::Float(None)));
    sym.functions.insert("s2d:label".to_string(), pf("s2d:label",
        vec![f_param("x"), f_param("y"), s_param("text"), f_param("a")], Type::Bool));
    sym.functions.insert("s2d:button".to_string(), pf("s2d:button",
        vec![f_param("x"), f_param("y"), f_param("w"), f_param("h"), s_param("label"), f_param("a")], Type::Bool));
    sym.functions.insert("s2d:slider".to_string(), pf("s2d:slider",
        vec![f_param("x"), f_param("y"), f_param("w"), f_param("h"), f_param("v"), f_param("lo"), f_param("hi"), f_param("a")], Type::Float(None)));
    sym.functions.insert("s2d:checkbox".to_string(), pf("s2d:checkbox",
        vec![f_param("x"), f_param("y"), s_param("label"), b_param("on"), f_param("a")], Type::Bool));
    sym.functions.insert("s2d:mouse_x".to_string(), ff("s2d:mouse_x", 0, Type::Float(None)));
    sym.functions.insert("s2d:mouse_y".to_string(), ff("s2d:mouse_y", 0, Type::Float(None)));
    sym.functions.insert("s2d:mouse_dx".to_string(), ff("s2d:mouse_dx", 0, Type::Float(None)));
    sym.functions.insert("s2d:mouse_dy".to_string(), ff("s2d:mouse_dy", 0, Type::Float(None)));
    sym.functions.insert("s2d:mouse_down".to_string(), ff("s2d:mouse_down", 0, Type::Bool));
    sym.functions.insert("s2d:wheel".to_string(), ff("s2d:wheel", 0, Type::Float(None)));
    sym.functions.insert("s2d:key".to_string(), pf("s2d:key", vec![s_param("key")], Type::Bool));
    sym.functions.insert("s2d:time".to_string(), ff("s2d:time", 0, Type::Float(None)));
    sym.functions.insert("s2d:dt".to_string(), ff("s2d:dt", 0, Type::Float(None)));
    // ---- spass3d -----------------------------------------------------------
    sym.functions.insert("s3d:window".to_string(), pf("s3d:window",
        vec![f_param("w"), f_param("h"), s_param("title")], Type::Bool));
    sym.functions.insert("s3d:open".to_string(), ff("s3d:open", 0, Type::Bool));
    sym.functions.insert("s3d:present".to_string(), ff("s3d:present", 0, Type::Bool));
    sym.functions.insert("s3d:clear".to_string(), ff("s3d:clear", 3, Type::Bool));
    sym.functions.insert("s3d:camera".to_string(), pf("s3d:camera",
        vec![f_param("ex"), f_param("ey"), f_param("ez"), f_param("tx"), f_param("ty"), f_param("tz"), f_param("fov")], Type::Bool));
    sym.functions.insert("s3d:light".to_string(), pf("s3d:light",
        vec![f_param("dx"), f_param("dy"), f_param("dz")], Type::Bool));
    sym.functions.insert("s3d:depth".to_string(), pf("s3d:depth", vec![b_param("on")], Type::Bool));
    sym.functions.insert("s3d:tri".to_string(), pf("s3d:tri",
        vec![f_param("x0"), f_param("y0"), f_param("z0"), f_param("x1"), f_param("y1"), f_param("z1"),
             f_param("x2"), f_param("y2"), f_param("z2"), f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    sym.functions.insert("s3d:quad".to_string(), pf("s3d:quad",
        vec![f_param("x0"), f_param("y0"), f_param("z0"), f_param("x1"), f_param("y1"), f_param("z1"),
             f_param("x2"), f_param("y2"), f_param("z2"), f_param("x3"), f_param("y3"), f_param("z3"),
             f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    sym.functions.insert("s3d:box".to_string(), pf("s3d:box",
        vec![f_param("cx"), f_param("cy"), f_param("cz"), f_param("sx"), f_param("sy"), f_param("sz"),
             f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    sym.functions.insert("s3d:grid".to_string(), pf("s3d:grid",
        vec![f_param("half"), f_param("step"), f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    // The model transform: a 4x4 matrix from local coordinates to world
    // coordinates, with a stack so a shape can be built in its own
    // frame and put back afterwards.
    sym.functions.insert("s3d:push".to_string(), ff("s3d:push", 0, Type::Bool));
    sym.functions.insert("s3d:pop".to_string(), ff("s3d:pop", 0, Type::Bool));
    sym.functions.insert("s3d:translate".to_string(), pf("s3d:translate",
        vec![f_param("tx"), f_param("ty"), f_param("tz")], Type::Bool));
    sym.functions.insert("s3d:rotate".to_string(), pf("s3d:rotate",
        vec![f_param("rx"), f_param("ry"), f_param("rz")], Type::Bool));
    sym.functions.insert("s3d:scale".to_string(), pf("s3d:scale",
        vec![f_param("sx"), f_param("sy"), f_param("sz")], Type::Bool));
    // The segment count of a sphere or a cylinder is a count, so it is
    // an `int`: it has no fractional part.
    sym.functions.insert("s3d:sphere".to_string(), pf("s3d:sphere",
        vec![f_param("cx"), f_param("cy"), f_param("cz"), f_param("rad"), i_param("segs"),
             f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    sym.functions.insert("s3d:cylinder".to_string(), pf("s3d:cylinder",
        vec![f_param("cx"), f_param("cy"), f_param("cz"), f_param("rad"), f_param("h"), i_param("segs"),
             f_param("r"), f_param("g"), f_param("b")], Type::Bool));
    sym.functions.insert("s3d:mouse_x".to_string(), ff("s3d:mouse_x", 0, Type::Float(None)));
    sym.functions.insert("s3d:mouse_y".to_string(), ff("s3d:mouse_y", 0, Type::Float(None)));
    sym.functions.insert("s3d:mouse_dx".to_string(), ff("s3d:mouse_dx", 0, Type::Float(None)));
    sym.functions.insert("s3d:mouse_dy".to_string(), ff("s3d:mouse_dy", 0, Type::Float(None)));
    sym.functions.insert("s3d:mouse_down".to_string(), ff("s3d:mouse_down", 0, Type::Bool));
    sym.functions.insert("s3d:key".to_string(), pf("s3d:key", vec![s_param("key")], Type::Bool));
    sym.functions.insert("s3d:time".to_string(), ff("s3d:time", 0, Type::Float(None)));
    sym.functions.insert("s3d:dt".to_string(), ff("s3d:dt", 0, Type::Float(None)));
    // ---- spio --------------------------------------------------------------
    sym.functions.insert("spio:print".to_string(), pf("spio:print", vec![s_param("text")], Type::Bool));
    sym.functions.insert("spio:print_num".to_string(), pf("spio:print_num", vec![f_param("v")], Type::Bool));
    sym.functions.insert("spio:read_text".to_string(), pf("spio:read_text", vec![s_param("path")], Type::String));
    sym.functions.insert("spio:write_text".to_string(), pf("spio:write_text",
        vec![s_param("path"), s_param("text")], Type::Bool));
    sym.functions.insert("spio:append_text".to_string(), pf("spio:append_text",
        vec![s_param("path"), s_param("text")], Type::Bool));
    sym.functions.insert("spio:read_line".to_string(), pf("spio:read_line",
        vec![s_param("path"), i_param("idx")], Type::String));
    sym.functions.insert("spio:line_count".to_string(), pf("spio:line_count", vec![s_param("path")], Type::Int(None)));
    sym.functions.insert("spio:replace_text".to_string(), pf("spio:replace_text",
        vec![s_param("path"), s_param("old"), s_param("new")], Type::Bool));
    sym.functions.insert("spio:read_f64".to_string(), pf("spio:read_f64", vec![s_param("path")], Type::Float(None)));
    sym.functions.insert("spio:write_f64".to_string(), pf("spio:write_f64",
        vec![s_param("path"), f_param("v")], Type::Bool));
    sym.functions.insert("spio:append_f64".to_string(), pf("spio:append_f64",
        vec![s_param("path"), f_param("v")], Type::Bool));
    sym.functions.insert("spio:exists".to_string(), pf("spio:exists", vec![s_param("path")], Type::Bool));
    sym.functions.insert("spio:size".to_string(), pf("spio:size", vec![s_param("path")], Type::Float(None)));
    sym.functions.insert("spio:remove".to_string(), pf("spio:remove", vec![s_param("path")], Type::Bool));
    sym.functions.insert("spio:mkdir".to_string(), pf("spio:mkdir", vec![s_param("path")], Type::Bool));
    sym.functions.insert("spio:copy".to_string(), pf("spio:copy",
        vec![s_param("from"), s_param("to")], Type::Bool));
    sym.functions.insert("spio:rename".to_string(), pf("spio:rename",
        vec![s_param("from"), s_param("to")], Type::Bool));
    sym.functions.insert("spio:list_count".to_string(), pf("spio:list_count", vec![s_param("dir")], Type::Int(None)));
    sym.functions.insert("spio:list_name".to_string(), pf("spio:list_name",
        vec![s_param("dir"), i_param("idx")], Type::String));

    // Vec constructors. The codegen layer picks `spass_mk_vec*`
    // by the number of arguments. Each constructor has a
    // distinct arity, so the registered signature matches the
    // runtime exactly: mk_vec2 takes 2 f64s, mk_vec3 takes 3,
    // mk_vec4 takes 4.
    let binary_constructor = |name: &str, ret_ty: Type| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("a"), f_param("b")],
        return_type: Some(ret_ty.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret_ty),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    let ternary_constructor = |name: &str, ret_ty: Type| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("a"), f_param("b"), f_param("c")],
        return_type: Some(ret_ty.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret_ty),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    let quaternary_constructor = |name: &str, ret_ty: Type| TypedFunc {
        name: name.to_string(),
        params: vec![f_param("a"), f_param("b"), f_param("c"), f_param("d")],
        return_type: Some(ret_ty.clone()),
        multi_returns: None,
        is_gpu: false,
        has_checkpoint: false,
        has_resume: false,
        has_await: false,
        await_binding: None,
        has_if_present: false,
        if_present_param: None,
        tags: vec![],
        // built-ins are runtime-provided, so they
        // have no captured variables, no enclosing function, and
        // no Spass source file. The codegen layer reads these
        // fields to decide whether the function should be hoisted
        // (ownclude) or skipped from the public symbol export
        // (fileclude). Built-ins are always public and live in
        // the global symbol table.
        captures: Vec::new(),
        enclosing_func: None,
        source_file: String::new(),
        span: dummy.clone(),
        original: FuncDef {
            name: name.to_string(),
            params: vec![],
            return_type: Some(ret_ty),
            multi_returns: None,
            body: spass_parser::ast::Block { stmts: vec![], span: dummy.clone() },
            annotations: vec![],
            visibility: spass_parser::ast::Visibility::Public,
            source_file: String::new(),
            enclosing_func: None,
            span: dummy.clone(),
        },
    };
    sym.functions.insert("mk_vec2".to_string(),
        binary_constructor("mk_vec2", Type::Vec2(None, None)));
    sym.functions.insert("mk_vec3".to_string(),
        ternary_constructor("mk_vec3", Type::Vec3(None, None, None)));
    // `mk_vec4` takes 4 f64 args.
    sym.functions.insert("mk_vec4".to_string(),
        quaternary_constructor("mk_vec4", Type::Vec4(None, None, None, None)));

    // `noise(x, z)` is binary; `fbm(x, z, octaves, gain)`
    // is quaternary. Both are provided by the runtime (`spass_noise_2d`
    // and `spass_fbm`) and must be registered here or a language-conformant
    // call site is rejected with E005.
    sym.functions.insert("noise".to_string(), binary("noise"));
    sym.functions.insert("fbm".to_string(),
        quaternary_constructor("fbm", Type::Float(None)));
}

// ============================================================================
// Recursion + body checking
// ============================================================================

fn check_function(func: &TypedFunc, sym: &mut SymbolTable) -> SemanticResult<()> {
    // a function with `out` parameters cannot also declare
    // a return type (or a multi-return). The two are mutually exclusive
    // - choose one way to communicate the result. Codegen also depends
    // on this: the call-site ABI changes shape (no return slot vs an
    // extra pointer) and silently allowing both would produce wrong code.
    let has_out = func.params.iter().any(|p| p.is_out);
    if has_out && (func.return_type.is_some() || func.multi_returns.is_some()) {
        return Err(SemanticError::TypeMismatch {
            message: format!(
                "function `{}` has `out` parameters and a return type; the two are mutually exclusive",
                func.name
            ),
            span: func.span.clone(),
        });
    }
    if sym.call_stack.iter().any(|n| n == &func.name) {
        let mut cycle = sym.call_stack.clone();
        cycle.push(func.name.clone());
        return Err(SemanticError::Recursive {
            name: func.name.clone(),
            cycle: cycle.join(" -> "),
            span: func.span.clone(),
        });
    }
    sym.call_stack.push(func.name.clone());
    sym.push_scope();

    // Bind parameters in scope.
    for p in &func.params {
        sym.define(&p.name, p.ty.clone());
    }
    // Capture the param names so the body checker can distinguish a
    // parameter (allowed as the `if_present` argument, )
    // from a `let`-bound local.
    sym.func_params = func.params.iter().map(|p| p.name.clone()).collect();

    // lists are function-local values for now. A list
    // return would point into the callee's frame in C and is not
    // FFI-safe as a `Vec` in Rust; a list parameter on a public
    // function cannot cross the module's numeric call ABI. Private
    // helpers may take lists - their call sites are generated inside
    // the module and pass the value directly.
    if let Some(rt) = &func.return_type {
        if type_contains_list(rt) {
            return Err(SemanticError::TypeMismatch {
                message: format!(
                    "`{}` cannot return a list yet; keep the list inside a function body",
                    func.name
                ),
                span: func.span.clone(),
            });
        }
    }
    if let Some(mr) = &func.multi_returns {
        if mr.iter().any(type_contains_list) {
            return Err(SemanticError::TypeMismatch {
                message: format!(
                    "`{}` cannot return a list yet; keep the list inside a function body",
                    func.name
                ),
                span: func.span.clone(),
            });
        }
    }
    if func.original.visibility == spass_parser::ast::Visibility::Public {
        for p in &func.params {
            if type_contains_list(&p.ty) {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "public function `{}` cannot take a list parameter yet (the module call ABI carries numbers)",
                        func.name
                    ),
                    span: p.span.clone(),
                });
            }
        }
    }
    // Reset the first-gate tracker for this function. The semantic
    // pass enforces that all `if_present` statements in a single
    // function name the same parameter (E016), and the codegen layer
    // lifts this name as the variant key.
    sym.first_if_present_param = None;
    // Reset the @await site list. Populated as the body is walked.
    sym.await_sites.clear();
    sym.await_binding = None;

    // while the body of a top-level public function
    // is being checked, the ownclude table is keyed by the public
    // function's own name (so `ownclude helper(...)` inside
    // `func generate_chunk` looks up `ownclude_funcs["generate_chunk"]`).
    // The MVP doesn't allow an ownclude inside another ownclude
    // (the parser does push the stack, but the semantic pass
    // treats the outer public function as the scope owner), so
    // for an ownclude body check we keep the outer value in
    // place. This is what makes the
    // `OwnPrivateOutOfScope` error fire when a call site is
    // outside the right function.
    let prev_enclosing = sym.current_enclosing.clone();
    if func.original.enclosing_func.is_none() {
        sym.current_enclosing = Some(func.name.clone());
    }
    // Same idea for the source file. We always set it (for both
    // top-level and ownclude body checks) so the fileclude
    // visibility check has a stable file string to compare
    // against the callee's `source_file` field.
    let prev_source = std::mem::replace(&mut sym.current_source_file, func.original.source_file.clone());

    // Check body. We iterate with an index so each `check_stmt` can
    // see the next statement - needed for the `@await` next-stmt
    // shape check which validates the immediately
    // following statement is `let v = spasio:input[mode]("channel")`.
    let stmts = &func.original.body.stmts;
    for i in 0..stmts.len() {
        let next = stmts.get(i + 1);
        check_stmt(&stmts[i], next, sym)?;
    }

    // a `@gpu` function cannot use `@await` (E013).
    // We check this AFTER the body walk because the await span is
    // only recorded as a side effect of the body walk - checking
    // before would always see an empty `await_sites` and silently
    // accept `@gpu` + `@await`. `ponytail`: the `is_gpu` flag
    // comes from the function-level annotation; the `has_await`
    // flag is computed by `build_typed_func`'s body walk. Both
    // are stable by the time we get here.
    if func.is_gpu && func.has_await {
        if let Some(span) = sym.await_sites.first().cloned() {
            return Err(SemanticError::GpuWithAwait { span });
        }
    }

    // Note: `sym.await_binding` is left in place here. The caller in
    // `analyze` reads it after `check_function` returns and lifts it
    // onto the typed function (see `build_typed_func`'s caller at
    // the second pass). Earlier we tried to `take` it here, but
    // that lost the binding - the symbol table field is the only
    // place the body-walked binding lives between the body check
    // and the typed-func construction.

    sym.pop_scope();
    sym.call_stack.pop();
    // restore the outer `current_enclosing` and
    // `current_source_file` so a sibling top-level function
    // check isn't poisoned by the previous one. For an ownclude
    // body check we never set `current_enclosing`, so the
    // `prev_enclosing` write is a no-op (it just writes the
    // same value back) - for a public function check, this
    // clears the field so the next top-level function check
    // starts clean.
    sym.current_enclosing = prev_enclosing;
    sym.current_source_file = prev_source;
    sym.analysed.insert(func.name.clone());
    Ok(())
}

fn check_stmt(
    stmt: &Stmt,
    next_stmt: Option<&Stmt>,
    sym: &mut SymbolTable,
) -> SemanticResult<()> {
    match stmt {
        Stmt::Let { names, ty, value, span } => {
            let value_ty = check_expr(value, sym)?;
            if names.len() == 1 {
                // Single-binding form. Honour an explicit type annotation
                // if present, else fall back to the inferred type.
                let n = &names[0];
                if let Some(declared) = ty {
                    if !type_compatible(&value_ty, declared) {
                        return Err(SemanticError::TypeMismatch {
                            message: format!(
                                "let `{}` declared as `{}` but bound to `{}`",
                                n,
                                declared.display_name(),
                                value_ty.display_name()
                            ),
                            span: span.clone(),
                        });
                    }
                    sym.define(n, declared.clone());
                } else {
                    sym.define(n, value_ty);
                }
            } else {
                // Destructuring form. The right-hand side
                // must be a direct call to a function declared with
                // `multi_returns` of matching arity. We look it up here so
                // codegen can simply read the element types from the
                // symbol table.
                //
                // `ponytail`: this only allows `let (a, b) = f(args)`;
                // a richer checker would also accept `(a, b) = ((x, y), z)`
                // or tuple-shaped identifiers. MVP is fine - the language
                // shows the call-site shape.
                let call_types = match value {
                    Expr::Call { callee, .. } => {
                        if let Expr::Ident { name, .. } = callee.as_ref() {
                            if let Some(f) = sym.lookup_func(name) {
                                if let Some(mr) = &f.multi_returns {
                                    if mr.len() != names.len() {
                                        return Err(SemanticError::TypeMismatch {
                                            message: format!(
                                                "destructuring into {} names but `{}` returns {}",
                                                names.len(),
                                                name,
                                                mr.len()
                                            ),
                                            span: span.clone(),
                                        });
                                    }
                                    mr.clone()
                                } else {
                                    return Err(SemanticError::TypeMismatch {
                                        message: format!(
                                            "destructuring into {} names but `{}` is not a multi-return function",
                                            names.len(),
                                            name
                                        ),
                                        span: span.clone(),
                                    });
                                }
                            } else {
                                return Err(SemanticError::UndefinedIdent {
                                    name: name.clone(),
                                    span: span.clone(),
                                });
                            }
                        } else {
                            return Err(SemanticError::NotCallable {
                                message: "destructuring requires a direct function call on the right-hand side".into(),
                                span: span.clone(),
                            });
                        }
                    }
                    _ => {
                        return Err(SemanticError::TypeMismatch {
                            message: format!(
                                "destructuring into {} names requires a multi-return call on the right-hand side",
                                names.len()
                            ),
                            span: span.clone(),
                        });
                    }
                };
                for (i, n) in names.iter().enumerate() {
                    sym.define(n, call_types[i].clone());
                }
            }
        }
        Stmt::Assign { name, value, span } => {
            let value_ty = check_expr(value, sym)?;
            let var_ty = sym.lookup(name).ok_or_else(|| SemanticError::UndefinedIdent {
                name: name.clone(),
                span: span.clone(),
            })?;
            if !type_compatible(&value_ty, &var_ty) {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "cannot assign `{}` to `{}` of type `{}`",
                        value_ty.display_name(),
                        name,
                        var_ty.display_name()
                    ),
                    span: span.clone(),
                });
            }
        }
        Stmt::Return { value, span } => {
            // Soft check: if a return value is given, ensure it is non-void
            // and a scalar type. Full type matching against the declared
            // return type is done in a later pass.
            if let Some(v) = value {
                let _ = check_expr(v, sym)?;
            } else {
            // void return is fine
            }
            // We do not enforce return type matches here; the IR builder
            // does. This keeps the semantic pass local.
            let _ = span;
        }
        Stmt::Expr(e, _) => {
            check_expr(e, sym)?;
        }
        Stmt::If { cond, then_branch, else_branch, .. } => {
            check_expr(cond, sym)?;
            // The `then` branch is never in an `else` context - we
            // explicitly reset the flag so a `then` nested inside an
            // `else` (a fresh `if` depth per the language) is treated as
            // a normal control-flow block. The `else` branch sets the
            // flag back to true; after both branches we restore the
            // outer state so an outer `else` is not poisoned.
            let prev = sym.in_else_branch;
            sym.in_else_branch = false;
            check_block(then_branch, sym)?;
            sym.in_else_branch = true;
            check_block(else_branch, sym)?;
            sym.in_else_branch = prev;
        }
        Stmt::For { var, iter, body, span } => {
            let iter_ty = match iter {
                spass_parser::ast::ForIter::Range { .. } => Type::Int(None),
                spass_parser::ast::ForIter::List(e) => list_element_type(&check_expr(e, sym)?),
            };
            sym.push_scope();
            sym.define(var, iter_ty);
            check_block(body, sym)?;
            sym.pop_scope();
            let _ = span;
        }
        Stmt::While { cond, body, .. } => {
            check_expr(cond, sym)?;
            check_block(body, sym)?;
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Checkpoint { name, span } => {
            // The pre-pass in `analyze` already enforced module-wide
            // uniqueness (E007), so by the time we reach the body checker
            // every checkpoint name is unique. Nothing else to do here -
            // codegen reads the `has_checkpoint` flag on `TypedFunc` to
            // decide whether to lower the function to a state machine.
            let _ = (name, span);
        }
        Stmt::Resume { name, span } => {
            // E007: a `@resume name` must match a `@checkpoint` declared
            // somewhere in the module. Cross-function references are
            // allowed and validated against the module-wide set.
            if !sym.checkpoint_names.contains(name) {
                return Err(SemanticError::CheckpointUndefined {
                    name: name.clone(),
                    span: span.clone(),
                });
            }
            // E007: a `@resume` may not appear in the `else` branch of
            // a top-level `if`. Nested `if`s inside the `else` reset the
            // flag (the language wants symmetric control flow at the same
            // nesting depth only).
            if sym.in_else_branch {
                return Err(SemanticError::ResumeInElseBranch {
                    name: name.clone(),
                    span: span.clone(),
                });
            }
        }
        Stmt::OrderDecl { .. } => {}
        Stmt::Await { span } => {
            // `@await` is a marker. The next statement
            // must be a `let name = spasio:input[mode]("name")`
            // binding. We do a structural shape check here (rather
            // than letting codegen fail) so the user gets a clear
            // error at compile time. The codegen layer keys the
            // awaited value off the bound name; the `mode` and
            // `channel` are needed to emit the runtime I/O call.
            let next = next_stmt.ok_or_else(|| SemanticError::AwaitNextStmtWrong {
                span: span.clone(),
            })?;
            let binding = match next {
                Stmt::Let { names, value, span: let_span, .. } => {
                    if names.len() != 1 {
                        return Err(SemanticError::AwaitNextLetShape {
                            got: format!("a let with {} names", names.len()),
                            span: let_span.clone(),
                        });
                    }
                    let (mode, channel) = match value {
                        Expr::SpasioInput { mode, channel, .. } => {
                            (mode.clone(), channel.clone())
                        }
                        other => {
                            return Err(SemanticError::AwaitNextLetShape {
                                got: format!(
                                    "a let with `{}` on the right-hand side (expected `spasio:input[...](\"...\")`)",
                                    expr_kind_name(other)
                                ),
                                span: let_span.clone(),
                            });
                        }
                    };
                    AwaitBinding {
                        name: names[0].clone(),
                        mode,
                        channel,
                        span: span.clone(),
                    }
                }
                other => {
                    let got = match other {
                        Stmt::If { .. } => "an `if`",
                        Stmt::For { .. } => "a `for`",
                        Stmt::While { .. } => "a `while`",
                        Stmt::Return { .. } => "a `return`",
                        Stmt::Assign { .. } => "an assignment",
                        Stmt::Expr(_, _) => "an expression statement",
                        Stmt::Checkpoint { .. } => "a `@checkpoint`",
                        Stmt::Resume { .. } => "a `@resume`",
                        Stmt::Assert { .. } => "an `assert`",
                        Stmt::OrderDecl { .. } => "an ordering declaration",
                        Stmt::Dispatch { .. } => "a `@dispatch`",
                        Stmt::IfPresent { .. } => "an `if_present`",
                        Stmt::Await { .. } => "another `@await`",
                        Stmt::Break { .. } => "a `break`",
                        Stmt::Continue { .. } => "a `continue`",
                        // Handled in the `Let` arm above; the compiler
                        // doesn't know that and needs an explicit case
                        // for exhaustiveness.
                        Stmt::Let { .. } => unreachable!("let is handled above"),
                        Stmt::Ownclude(_) => "an `ownclude` declaration",
                    };
                    return Err(SemanticError::AwaitNextLetShape {
                        got: got.to_string(),
                        span: next.span(),
                    });
                }
            };
            // MVP: at most one `@await` per function. A second
            // await would require a per-await state channel table
            // which the codegen layer doesn't carry yet.
            if sym.await_binding.is_some() {
                return Err(SemanticError::AwaitNextStmtWrong {
                    span: span.clone(),
                });
            }
            sym.await_binding = Some(binding);
            sym.await_sites.push(span.clone());
        }
        Stmt::Dispatch { decl, apply, .. } => {
            // validate the dispatch site. Match functions
            // against the tag selector, ensure every param is bound,
            // topologically sort with @before/@after, and surface the
            // matching errors (E002, E004, E014, W002). The actual
            // expansion into a call sequence is the codegen pass's job.
            //
            // `ponytail`: the `apply(...)` call is not a real function
            // call - the codegen pass replaces it with a chain of calls
            // to the matched functions. We type-check the args (so
            // undefined locals still surface as E005) but skip the
            // function-name lookup for `apply` itself.
            for b in &decl.bind {
                if let spass_parser::ast::DispatchBindSource::Literal(e) = &b.source {
                    let _ = check_expr(e, sym)?;
                }
            }
            if let Expr::Call { args, .. } = &apply.call {
                for a in args {
                    let _ = check_expr(&a.value, sym)?;
                }
            }
            // D4: the dispatch resolver returns `(ResolvedDispatch,
            // Vec<SemanticWarning>)`. The resolved dispatch is
            // stashed for the codegen pass (which reads it from the
            // symbol table); the warnings are pushed into the
            // symbol-table vec so `analyze_full` can drain them
            // at the end and surface them to the CLI.
            let (_resolved, ws) =
                crate::dispatch::resolve_dispatch(decl, apply, &sym.as_typed_module())?;
            sym.warnings.extend(ws);
        }
        Stmt::Assert { kind, span } => {
            // `assert(...)` operates on a local identifier (and a
            // compile-time-known range / approx target). We do not need
            // the value's type - the test driver reads the value at
            // runtime and compares. Just make sure the identifier is
            // defined so a typo doesn't fail silently at test time.
            //
            // `ponytail`: range bounds and `approx` targets are
            // expressions; if they reference undefined names, the
            // existing `check_expr` path surfaces E005. We do not
            // currently enforce that `lo`/`hi` are compile-time
            // constants - the language allows identifiers (e.g. `sea`),
            // which the test driver will look up at test time. A
            // future pass can require const-eval here.
            let _ = span;
            match kind {
                spass_parser::ast::AssertKind::InRange { value, range, .. } => {
                    if sym.lookup(value).is_none() {
                        return Err(SemanticError::UndefinedIdent {
                            name: value.clone(),
                            span: range.span.clone(),
                        });
                    }
                }
                spass_parser::ast::AssertKind::Approx { value, target, tolerance, .. } => {
                    if sym.lookup(value).is_none() {
                        return Err(SemanticError::UndefinedIdent {
                            name: value.clone(),
                            span: target.span(),
                        });
                    }
                    let _ = check_expr(target, sym)?;
                    let _ = check_expr(tolerance, sym)?;
                }
            }
        }
        Stmt::IfPresent { param, present_branch, absent_branch, span } => {
            // the argument must be a parameter of the
            // enclosing function. We compare against `func_params`
            // (captured on entry to `check_function`) rather than
            // `sym.lookup` because a `let` can shadow a parameter
            // name and we want the user-facing rule: only params are
            // usable as the gate.
            if !sym.func_params.contains(param) {
                return Err(SemanticError::IfPresentNotParam {
                    name: param.clone(),
                    span: span.clone(),
                });
            }
            // MVP: at most one gated parameter per function. The
            // codegen layer emits exactly two variants (present /
            // absent); multiple gated parameters would require a
            // cross-product of variants, which the binary dispatch
            // contract doesn't support. We track the first gate we
            // see on the symbol table and reject any subsequent
            // `if_present` for a different parameter.
            match &sym.first_if_present_param {
                None => sym.first_if_present_param = Some(param.clone()),
                Some(existing) if existing == param => {}
                Some(existing) => {
                    return Err(SemanticError::IfPresentMultiple {
                        name1: existing.clone(),
                        name2: param.clone(),
                        span: span.clone(),
                    });
                }
            }
            // Each branch runs in its own scope so `let` bindings do
            // not leak between them. The dispatcher enters only one
            // branch at runtime; both must type-check independently.
            sym.push_scope();
            check_block(present_branch, sym)?;
            sym.pop_scope();
            if let Some(absent) = absent_branch {
                sym.push_scope();
                check_block(absent, sym)?;
                sym.pop_scope();
            }
        }
        Stmt::Ownclude(f) => {
            // `ownclude` introduces a function-local
            // nested function. The body is type-checked in a fresh
            // scope that inherits the enclosing function's
            // parameters and `let` bindings (the closure capture
            // is the union of these minus the inner function's
            // own parameters). We do NOT register the inner
            // function in the module-level symbol table - the
            // visibility check below uses a side table scoped to
            // the current enclosing function.
            check_ownclude(f, sym)?;
        }
    }
    Ok(())
}

fn check_block(block: &spass_parser::ast::Block, sym: &mut SymbolTable) -> SemanticResult<()> {
    // Indexed iteration so each `check_stmt` can see its next
    // statement - needed for the `@await` next-stmt shape check
    // `ponytail`: nested blocks are walked via
    // recursive `check_block` calls inside `check_stmt`, and each
    // of those gets its own indexed iteration, so the next-stmt
    // rule is local to each block.
    let stmts = &block.stmts;
    for i in 0..stmts.len() {
        let next = stmts.get(i + 1);
        check_stmt(&stmts[i], next, sym)?;
    }
    Ok(())
}

/// type-check the body of a nested `ownclude` function
/// and register it in the enclosing function's ownclude table.
///
/// The check is split into three steps:
///
/// 1. **Compute the capture set.** The language captures enclosing variables by
/// value at definition time. The capture list is the
/// intersection of the names the nested body actually *reads*
/// ([`collect_stmt_read_names`]) and the enclosing-scope bindings
/// that exist *before* the ownclude site in source order
/// ([`collect_enclosing_bindings_until`]). Reading a name that is
/// bound later (or not at all) is an E005 undefined identifier.
/// We also enforce that the inner function never tries to *write*
/// to a captured name (the language says captured values cannot be modified
/// inside an `ownclude` function).
///
/// 2. **Register the typed function.** Stored in
/// `sym.ownclude_funcs[enclosing]` so the call-site type-check
/// in `check_expr` can find it. The typed function's
/// `captures` vec carries the list from step 1 to the
/// codegen layer.
///
/// 3. **Type-check the body in a fresh scope.** The inner body
/// sees (a) the inner function's own parameters, (b) the
/// captures as if they were additional `let` bindings (so
/// reads resolve correctly), and (c) no other enclosing
/// names - a reference to a name that's not a parameter and
/// not a capture is an E005 undefined identifier, which is
/// exactly the static scope the language wants.
fn check_ownclude(f: &FuncDef, sym: &mut SymbolTable) -> SemanticResult<()> {
    // The parser already stamped `enclosing_func`; we just read it.
    let enclosing = f.enclosing_func.clone().ok_or_else(|| SemanticError::TypeMismatch {
        message: "internal: ownclude func missing enclosing_func".to_string(),
        span: f.span.clone(),
    })?;

    // 1. Compute the set of names that are visible in the enclosing
    // function's scope at the point where the `ownclude` keyword
    // appeared. We re-derive it here (rather than reading it off
    // the symbol table) so the capture is strictly lexical - the
    // language's "by value at definition" rule. The result is:
    // enclosing params + every `let`/`out`/if-`let` etc. that
    // appeared earlier in source order. We walk the enclosing
    // function's body up to the ownclude site, but we only have
    // the AST here; the helper [`collect_enclosing_bindings_until`]
    // does the walk in source order.
    //
    // The capture set is the intersection of:
    // (a) the names the ownclude body actually *reads*, and
    // (b) the enclosing-scope bindings that exist *before* the
    // ownclude site in source order ("capture happens at definition time").
    // A name the body reads but that is bound later (or not at
    // all) is an UndefinedIdent error - by-value capture needs a
    // stable, already-initialised value. A binding the body never
    // reads is not captured at all, keeping the hoisted helper's
    // ABI (and its mangled name, which encodes the capture types)
    // minimal.
    let own_params: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let mut referenced: HashSet<String> = HashSet::new();
    for s in &f.body.stmts {
        collect_stmt_read_names(s, &mut referenced);
    }
    // Drop the inner function's own name (a recursive call is allowed
    // but wouldn't go through a capture) and its own parameters.
    referenced.remove(&f.name);
    let bound = collect_enclosing_bindings_until(sym, &enclosing, &f.span);
    let mut captures: Vec<(String, Type)> = Vec::new();
    for name in referenced {
        if own_params.contains(&name) {
            continue;
        }
        match bound.iter().find(|(n, _)| n == &name) {
            Some((_, ty)) => captures.push((name, ty.clone())),
            None => {
                return Err(SemanticError::UndefinedIdent {
                    name,
                    span: f.span.clone(),
                });
            }
        }
    }
    // Stable order so codegen can emit a deterministic signature.
    captures.sort_by(|a, b| a.0.cmp(&b.0));

    // Walk the body for the two static checks the language requires:
    // * a captured name is never the LHS of an assignment, and
    // * a captured name is never the target of a compound assign.
    // We surface both as E006 (visibility / scope) so the user
    // sees one error category for "you can't touch this from
    // inside the closure".
    for s in &f.body.stmts {
        check_ownclude_no_capture_writes(s, &captures, &f.name)?;
    }

    // 2. Build a typed function for the closure. The captures are
    // stored on the typed func so the codegen layer can emit
    // them as extra leading parameters on the hoisted helper.
    let mut typed = build_typed_func(f);
    typed.captures = captures.clone();
    // Register in the enclosing function's ownclude table. The
    // MVP doesn't allow an ownclude inside another ownclude
    // (the parser pushes the stack, but the lookup table is
    // always keyed by the outermost public function's name -
    // that's the "scope" the language wants).
    sym.ownclude_funcs
        .entry(enclosing.clone())
        .or_default()
        .push(typed.clone());

    // 3. Type-check the body in a fresh scope. The body sees:
    // (a) the inner function's parameters (so type-checking
    // those works as normal), (b) the captures as a single
    // `let` block (so reads resolve). The MVP does not
    // support recursion through the closure: a call from
    // inside the body to the ownclude's own name would
    // require the closure to reference itself before it's
    // registered, which we don't support.
    sym.push_scope();
    for p in &f.params {
        sym.define(&p.name, p.ty.clone());
    }
    // Insert the captures as a "scope" of locals. The type for
    // each capture was already resolved in step 1 (where we
    // built the `captures` vec) so we can define them here
    // without an extra lookup.
    for (name, ty) in &captures {
        sym.define(name, ty.clone());
    }
    // Check the body. We do not push the call stack here -
    // the ownclude body can call the enclosing function (which
    // is on the stack already) but not itself (the MVP doesn't
    // support recursive ownclude).
    for i in 0..f.body.stmts.len() {
        let next = f.body.stmts.get(i + 1);
        check_stmt(&f.body.stmts[i], next, sym)?;
    }
    sym.pop_scope();
    Ok(())
}

/// collect the identifiers *read* by a statement -
/// recursively through nested blocks and expressions. Used to compute
/// the minimal by-value capture set of an `ownclude` body: only names
/// the body actually references become captures, so the hoisted
/// helper's ABI stays as small as the closure's real footprint.
fn collect_stmt_read_names(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } => {
            collect_expr_read_names(value, out);
        }
        Stmt::Return { value: Some(e), .. } => collect_expr_read_names(e, out),
        Stmt::Expr(e, _) => collect_expr_read_names(e, out),
        Stmt::If { cond, then_branch, else_branch, .. } => {
            collect_expr_read_names(cond, out);
            for s in &then_branch.stmts {
                collect_stmt_read_names(s, out);
            }
            for s in &else_branch.stmts {
                collect_stmt_read_names(s, out);
            }
        }
        Stmt::For { iter, body, .. } => {
            match iter {
                ForIter::Range { start, end, step } => {
                    collect_expr_read_names(start, out);
                    collect_expr_read_names(end, out);
                    if let Some(s) = step {
                        collect_expr_read_names(s, out);
                    }
                }
                ForIter::List(e) => collect_expr_read_names(e, out),
            }
            for s in &body.stmts {
                collect_stmt_read_names(s, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_expr_read_names(cond, out);
            for s in &body.stmts {
                collect_stmt_read_names(s, out);
            }
        }
        // A nested `ownclude` reads the enclosing scope through its
        // own body; walking it keeps the outer capture set complete.
        Stmt::Ownclude(inner) => {
            for s in &inner.body.stmts {
                collect_stmt_read_names(s, out);
            }
        }
        Stmt::Dispatch { decl, apply, .. } => {
            for b in &decl.bind {
                if let DispatchBindSource::Literal(e) = &b.source {
                    collect_expr_read_names(e, out);
                }
            }
            collect_expr_read_names(&apply.call, out);
        }
        Stmt::IfPresent { present_branch, absent_branch, .. } => {
            for s in &present_branch.stmts {
                collect_stmt_read_names(s, out);
            }
            if let Some(absent) = absent_branch {
                for s in &absent.stmts {
                    collect_stmt_read_names(s, out);
                }
            }
        }
        // Break / Continue / Checkpoint / Resume / OrderDecl / Assert /
        // Await read no identifiers.
        _ => {}
    }
}

/// Expression half of [`collect_stmt_read_names`]: a
/// `send`/`broadcast` message and a `receive` filter both reference
/// locals that an `ownclude` closure must capture.
fn collect_expr_read_names(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Ident { name, .. } => {
            out.insert(name.clone());
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_read_names(lhs, out);
            collect_expr_read_names(rhs, out);
        }
        Expr::Unary { expr: inner, .. } => collect_expr_read_names(inner, out),
        Expr::Call { callee, args, .. } => {
            collect_expr_read_names(callee, out);
            for a in args {
                collect_expr_read_names(&a.value, out);
            }
        }
        Expr::FieldAccess { target, .. } => collect_expr_read_names(target, out),
        Expr::Index { target, index, .. } => {
            collect_expr_read_names(target, out);
            collect_expr_read_names(index, out);
        }
        Expr::Tuple { elements, .. } | Expr::ListLit { elements, .. } => {
            for e in elements {
                collect_expr_read_names(e, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, e) in fields {
                collect_expr_read_names(e, out);
            }
        }
        Expr::IfExpr { cond, then_branch, else_branch, .. } => {
            collect_expr_read_names(cond, out);
            collect_expr_read_names(then_branch, out);
            collect_expr_read_names(else_branch, out);
        }
        Expr::Spawn { func, args, .. } | Expr::Parallel { func, args, .. } => {
            collect_expr_read_names(func, out);
            for a in args {
                collect_expr_read_names(&a.value, out);
            }
        }
        Expr::Await { task, .. } => collect_expr_read_names(task, out),
        Expr::Random { candidates, args, .. } => {
            for c in candidates {
                collect_expr_read_names(c, out);
            }
            if let Some(args) = args {
                for a in args {
                    collect_expr_read_names(&a.value, out);
                }
            }
        }
        Expr::AsType { value, .. } | Expr::NarrowCast { value, .. } | Expr::Conv { value, .. } => {
            collect_expr_read_names(value, out);
        }
        Expr::SpasioOutput { value, .. } => collect_expr_read_names(value, out),
        Expr::NativeCall { args, .. } => {
            for a in args {
                collect_expr_read_names(&a.value, out);
            }
        }
        Expr::IfPresent { then_branch, absent_branch, .. } => {
            for s in &then_branch.stmts {
                collect_stmt_read_names(s, out);
            }
            if let Some(absent) = absent_branch {
                for s in &absent.stmts {
                    collect_stmt_read_names(s, out);
                }
            }
        }
        // message passing reads the message expression
        // (`send`/`broadcast`) and the filter conditions (`receive`).
        Expr::Send { message, .. } | Expr::Broadcast { message, .. } => {
            collect_expr_read_names(message, out);
        }
        Expr::Receive { pattern, .. } => {
            if let Some(p) = pattern {
                for (_field, _op, value) in &p.conditions {
                    collect_expr_read_names(value, out);
                }
            }
        }
        // Literals and the channel-only `spasio:input` read nothing.
        Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. }
        | Expr::StringLit { .. }
        | Expr::SpasioInput { .. } => {}
    }
}

/// the enclosing function's bindings (parameters first,
/// then `let`/`for` locals) that exist *before* the given `ownclude`
/// site in source order. Capture happens at definition time: a binding created after
/// the ownclude statement is not part of its by-value capture scope.
///
/// The walk stops at the first statement whose span matches the
/// ownclude site; everything after it is invisible to the capture.
/// We do not consult the symbol table because it is mutated by the
/// outer body-walker; the AST walk gives a stable, purely-lexical
/// answer.
///
/// `ponytail`: the walker does not track shadowing or scoping rules -
/// a `let` rebinding a parameter name in a nested block would still
/// be reported as in scope. The MVP's user-facing rule (E005
/// undefined) catches the case where the user reads the wrong name;
/// the shadowing nuance is left to a future revision.
fn collect_enclosing_bindings_until(
    sym: &SymbolTable,
    enclosing: &str,
    ownclude_span: &Span,
) -> Vec<(String, Type)> {
    let mut out: Vec<(String, Type)> = Vec::new();
    let f = match sym.functions.get(enclosing) {
        Some(f) => f,
        None => return out,
    };
    // Parameters are always visible from the first statement.
    for p in &f.params {
        out.push((p.name.clone(), p.ty.clone()));
    }
    fn walk(
        stmts: &[Stmt],
        own: &Span,
        out: &mut Vec<(String, Type)>,
        stopped: &mut bool,
    ) {
        for s in stmts {
            if *stopped {
                return;
            }
            // The ownclude site itself: bindings from here on are not
            // visible at definition time. Span equality is safe - the
            // `Stmt::Ownclude` in the enclosing body is the same node
            // the checker is visiting (same file + byte offset).
            if let Stmt::Ownclude(inner) = s {
                if &inner.span == own {
                *stopped = true;
                    return;
                }
            }
            match s {
                Stmt::Let { names, ty, .. } => {
                    // Single-name form carries the declared type; a
                    // multi-name destructure falls back to `Float`
                    // (same approximation the codegen makes - the
                    // semantic body check has already validated the
                    // actual types, so the fallback only affects the
                    // hoisted helper's declared ABI).
                    if names.len() == 1 {
                        out.push((
                            names[0].clone(),
                            ty.clone().unwrap_or(Type::Float(None)),
                        ));
                    } else {
                        for n in names {
                            out.push((n.clone(), Type::Float(None)));
                        }
                    }
                }
                Stmt::If { then_branch, else_branch, .. } => {
                    walk(&then_branch.stmts, own, out, stopped);
                    if !*stopped {
                        walk(&else_branch.stmts, own, out, stopped);
                    }
                }
                Stmt::For { var, body, .. } => {
                    out.push((var.clone(), Type::Int(None)));
                    walk(&body.stmts, own, out, stopped);
                }
                Stmt::While { body, .. } => walk(&body.stmts, own, out, stopped),
                _ => {}
            }
        }
    }
    let mut stopped = false;
    walk(&f.original.body.stmts, ownclude_span, &mut out, &mut stopped);
    out
}

/// captured values cannot be modified inside an `ownclude`
/// function: a captured name cannot appear on the LHS of
/// an assignment (or be the target of any other write). We walk
/// the body of the ownclude and surface every offending site as
/// an E006 (visibility/scope) error.
///
/// `ponytail`: this catches plain `=` and the `let` rebinding
/// of the same name. It does not catch compound-assignment
/// operators (which the language doesn't currently expose in
/// Spass, but a future revision may add). A full pass would
/// run a proper use-def analysis; the MVP's flat scan is
/// sufficient because the language is purely textual - any
/// `name =...` write site is an error.
fn check_ownclude_no_capture_writes(
    stmt: &Stmt,
    captures: &[(String, Type)],
    own_name: &str,
) -> SemanticResult<()> {
    if captures.is_empty() {
        return Ok(());
    }
    fn walk(
        stmts: &[Stmt],
        captures: &[(String, Type)],
        own_name: &str,
        span_for: &spass_lexer::Span,
    ) -> SemanticResult<()> {
        for s in stmts {
            match s {
                Stmt::Assign { name, span, .. } => {
                    if captures.iter().any(|(n, _)| n == name) {
                        return Err(SemanticError::TypeMismatch {
                            message: format!(
                                "captured variable `{}` is read-only inside `ownclude` `{}`; \
                                 capture-by-value forbids modification",
                                name, own_name
                            ),
                            span: span.clone(),
                        });
                    }
                }
                Stmt::Let { names, .. } => {
                    for n in names {
                        if captures.iter().any(|(c, _)| c == n) {
                            return Err(SemanticError::TypeMismatch {
                                message: format!(
                                    "captured variable `{}` cannot be re-bound inside \
                                     `ownclude` `{}`; capture-by-value forbids modification",
                                    n, own_name
                                ),
                                span: span_for.clone(),
                            });
                        }
                    }
                }
                Stmt::If { then_branch, else_branch, .. } => {
                    walk(&then_branch.stmts, captures, own_name, span_for)?;
                    walk(&else_branch.stmts, captures, own_name, span_for)?;
                }
                Stmt::For { body, .. } | Stmt::While { body, .. } => {
                    walk(&body.stmts, captures, own_name, span_for)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    // Use the outer stmt's span as the per-error source location.
    walk(std::slice::from_ref(stmt), captures, own_name, &stmt.span())
}

/// enforce the visibility contract at a call site.
/// Called from `check_expr(Expr::Call)` after the callee has
/// been looked up. The callee's `original.visibility` field
/// drives the check:
///
/// * `Public` - always allowed (no extra check).
/// * `FilePrivate` - the calling function must be in the
/// same source file. Compared via
/// `sym.current_source_file` against the callee's
/// `f.source_file`.
/// * `OwnPrivate` - the calling function must be inside the
/// body of the public function that owns the ownclude
/// (recorded in the callee's `f.enclosing_func` field).
/// Compared via `sym.current_enclosing` against that
/// owner. The MVP does not support an ownclude-inside-
/// ownclude, so the `current_enclosing` is the *outer*
/// public function's name while we walk the inner
/// ownclude's body - which means a call to a sibling
/// ownclude from inside another ownclude is allowed.
/// That's a deliberate scope relaxation, not a bug.
///
/// `ponytail`: we only check direct calls. A future revision
/// could also enforce visibility on `Expr::Ident` (a 0-arity
/// function used as a value); the MVP only supports direct
/// calls, so the call-site check is sufficient.
fn enforce_visibility(
    f: &TypedFunc,
    sym: &SymbolTable,
    name: &str,
    span: &Span,
) -> SemanticResult<()> {
    match f.original.visibility {
        spass_parser::ast::Visibility::Public => Ok(()),
        spass_parser::ast::Visibility::FilePrivate => {
            // Same source file? Compare absolute paths so a
            // relative vs. canonical mismatch (e.g. the lexer
            // records the lexer-supplied path and the include
            // resolver canonicalises) doesn't generate a false
            // E006. Both strings are written by the same
            // path-resolution code path, so they should already
            // be identical for the common case; the equality
            // check is the safe-by-construction one.
            if f.original.source_file == sym.current_source_file {
                Ok(())
            } else {
                Err(SemanticError::FilePrivateCrossFile {
                    name: name.to_string(),
                    defined_in: f.original.source_file.clone(),
                    called_from: sym.current_source_file.clone(),
                    span: span.clone(),
                })
            }
        }
        spass_parser::ast::Visibility::OwnPrivate => {
            let owner = f.original.enclosing_func.as_deref().unwrap_or("");
            if sym.current_enclosing.as_deref() == Some(owner) {
                Ok(())
            } else {
                let called_from = sym
                    .current_enclosing
                    .clone()
                    .unwrap_or_else(|| "<module level>".to_string());
                Err(SemanticError::OwnPrivateOutOfScope {
                    name: name.to_string(),
                    enclosing: owner.to_string(),
                    called_from,
                    span: span.clone(),
                })
            }
        }
    }
}

/// Type-check the body of a `test "name" { body }` declaration.
/// The test body runs in a fresh scope with the `with` defaults bound
/// as locals. Used by [`analyze`] to surface undefined-name errors
/// (E005) before the test driver picks the test up.
fn check_test_body(
    test: &spass_parser::ast::TestDef,
    sym: &mut SymbolTable,
) -> SemanticResult<()> {
    sym.push_scope();
    for p in &test.params {
        let _ = check_expr(&p.default, sym)?;
        sym.define(&p.name, p.ty.clone());
    }
    check_block(&test.body, sym)?;
    sym.pop_scope();
    Ok(())
}

/// Walk a list of statements and collect every `@checkpoint name` we
/// find. Used by the pre-pass in [`analyze`] to enforce module-wide
/// uniqueness. The walker descends into nested blocks (`if`, `for`,
/// `while`) but does not follow cross-function references - the language
/// requires every `@checkpoint` to be declared in the same module.
fn collect_checkpoint_names(
    stmts: &[Stmt],
    into: &mut HashSet<String>,
) -> SemanticResult<()> {
    for s in stmts {
        match s {
            Stmt::Checkpoint { name, span } => {
                if !into.insert(name.clone()) {
                    return Err(SemanticError::CheckpointDuplicate {
                        name: name.clone(),
                        span: span.clone(),
                    });
                }
            }
            Stmt::If { then_branch, else_branch, .. } => {
                collect_checkpoint_names(&then_branch.stmts, into)?;
                collect_checkpoint_names(&else_branch.stmts, into)?;
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                collect_checkpoint_names(&body.stmts, into)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// dimension of a vec type. Returns `None` for
/// scalars, structs, lists, and other types the vec built-ins
/// don't accept. The mirror helper `mat_dim` covers the matrix
/// family.
fn vec_dim(ty: &Type) -> Option<u32> {
    match ty {
        Type::Vec2(_, _) => Some(2),
        Type::Vec3(_, _, _) => Some(3),
        Type::Vec4(_, _, _, _) => Some(4),
        _ => None,
    }
}

fn mat_dim(ty: &Type) -> Option<u32> {
    match ty {
        Type::Mat3 => Some(3),
        Type::Mat4 => Some(4),
        _ => None,
    }
}

/// The argument category a vec/mat built-in expects. The MVP
/// treats all vec built-ins as accepting any vec dim, and all
/// mat built-ins as accepting any mat dim - the dimension is
/// resolved at codegen time from the first argument's type.
#[derive(Debug, Clone, Copy)]
enum VecMatCategory {
    AnyVec,
    AnyMat,
}

impl VecMatCategory {
    fn expected_name(self) -> &'static str {
        match self {
            VecMatCategory::AnyVec => "vec",
            VecMatCategory::AnyMat => "mat",
        }
    }
}

/// For vec/mat built-ins, return the category and the
/// representative return type. `length` / `dot` return float;
/// `normalize` / `hadamard` / `cross` / `transpose` return
/// the matching vec / mat. The return type is only used for
/// the type-checker; the codegen layer resolves the dim.
fn is_vec_mat_builtin(name: &str) -> Option<(VecMatCategory, Type)> {
    match name {
        // vec built-ins: any vec, any dim, return vec3 (or float for dot/length)
        "length" => Some((VecMatCategory::AnyVec, Type::Float(None))),
        "normalize" => Some((VecMatCategory::AnyVec, Type::Vec3(None, None, None))),
        "dot" => Some((VecMatCategory::AnyVec, Type::Float(None))),
        "hadamard" => Some((VecMatCategory::AnyVec, Type::Vec3(None, None, None))),
        "cross" => Some((VecMatCategory::AnyVec, Type::Vec3(None, None, None))),
        // mat built-ins: any mat, return mat3
        "transpose" => Some((VecMatCategory::AnyMat, Type::Mat3)),
        _ => None,
    }
}

fn check_expr(expr: &Expr, sym: &mut SymbolTable) -> SemanticResult<Type> {
    match expr {
        Expr::IntLit { .. } => Ok(Type::Int(None)),
        Expr::FloatLit { .. } => Ok(Type::Float(None)),
        Expr::BoolLit { .. } => Ok(Type::Bool),
        Expr::StringLit { .. } => Ok(Type::String),
        Expr::Ident { name, span } => {
            // Could be a local variable or a 0-arity function call.
            if let Some(t) = sym.lookup(name) {
                Ok(t)
            } else if let Some(f) = sym.lookup_func(name) {
                // enforce visibility on 0-arity
                // "call-by-name" sites the same way as on a
                // regular `f` call. The MVP doesn't have
                // first-class functions, so this is the only
                // place a function symbol can be referenced
                // without an explicit call.
                enforce_visibility(&f, sym, name, span)?;
                if f.params.is_empty() {
                    if let Some(ret) = &f.return_type {
                        Ok(ret.clone())
                    } else {
                        Ok(Type::Bool) // out-only function, treated as void; conservative
                    }
                } else {
                    Err(SemanticError::NotCallable {
                        message: format!(
                            "`{}` is a function with {} parameters, use `{}()` to call it",
                            name,
                            f.params.len(),
                            name
                        ),
                        span: span.clone(),
                    })
                }
            } else {
                Err(SemanticError::UndefinedIdent {
                    name: name.clone(),
                    span: span.clone(),
                })
            }
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let lt = check_expr(lhs, sym)?;
            let rt = check_expr(rhs, sym)?;
            binary_op_type(op, &lt, &rt, span)
        }
        Expr::Unary { op, expr: inner, .. } => {
            let t = check_expr(inner, sym)?;
            match op {
                UnaryOp::Neg => Ok(t),
                UnaryOp::Not => Ok(Type::Bool),
            }
        }
        // `s2d:window(...)`, `s3d:box(...)`, `spio:read_text(...)` -
        // playground native calls. Registered in `register_builtins`
        // under their namespaced symbol (`s2d:window`,...).
        Expr::NativeCall { namespace, func, args, span } => {
            let name = format!("{}:{}", namespace, func);
            let f = sym
                .lookup_func(&name)
                .ok_or_else(|| SemanticError::UndefinedIdent {
                    name: name.clone(),
                    span: span.clone(),
                })?
                .clone();
            if args.len() != f.params.len() {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "function `{}` expects {} args, got {}",
                        f.name,
                        f.params.len(),
                        args.len()
                    ),
                    span: span.clone(),
                });
            }
            for (i, arg) in args.iter().enumerate() {
                let at = check_expr(&arg.value, sym)?;
                let pt = &f.params[i].ty;
                if !type_compatible(&at, pt) {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "arg {} of `{}` expected `{}`, got `{}`",
                            i,
                            f.name,
                            pt.display_name(),
                            at.display_name()
                        ),
                        span: span.clone(),
                    });
                }
            }
            Ok(f.return_type.clone().unwrap_or(Type::Bool))
        }
        Expr::Call { callee, args, span } => {
            // The callee is expected to be a function name. We do not
            // support function pointers / first-class functions.
            let func_name = match callee.as_ref() {
                Expr::Ident { name, .. } => name.clone(),
                _ => {
                    return Err(SemanticError::NotCallable {
                        message: "only direct function calls are supported".to_string(),
                        span: span.clone(),
                    })
                }
            };
            // Recursively check the called function to detect cycles. The
            // call stack grows as we descend; if a function is already on
            // the stack, that's a recursive cycle.
            if !sym.analysed.contains(&func_name) {
                if let Some(f) = sym.functions.get(&func_name).cloned() {
                    check_function(&f, sym)?;
                }
            }
            let f = sym.lookup_func(&func_name).ok_or_else(|| {
                SemanticError::UndefinedIdent {
                    name: func_name.clone(),
                    span: span.clone(),
                }
            })?.clone();
            // enforce visibility at the call site.
            // * `fileclude` is only visible to functions defined
            // in the same source file.
            // * `ownclude` is only visible inside the body of
            // its enclosing public function.
            // The check uses the calling function's metadata on
            // the symbol table: the file is `current_source_file`
            // (set on entry to `check_function`), the enclosing
            // public function is `current_enclosing` (set the
            // same way). The ownclude case requires knowing the
            // call site is inside the *outer* public function,
            // not the current inner ownclude body - so the check
            // walks the ownclude table looking for the entry
            // whose `enclosing_func` matches `current_enclosing`.
            enforce_visibility(&f, sym, &func_name, span)?;
            // Recursion check: if we're calling a function on the stack
            // (other than the current frame at the top, which is the
            // recursion we already caught), that's a recursive cycle.
            if sym.call_stack.iter().any(|n| n == &func_name) {
                let mut cycle = sym.call_stack.clone();
                cycle.push(func_name.clone());
                return Err(SemanticError::Recursive {
                    name: func_name.clone(),
                    cycle: cycle.join(" -> "),
                    span: span.clone(),
                });
            }
            if args.len() != f.params.len() {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "function `{}` expects {} args, got {}",
                        f.name,
                        f.params.len(),
                        args.len()
                    ),
                    span: span.clone(),
                });
            }
            // vec/mat built-ins (`dot`, `length`,...)
            // are registered with a single representative signature
            // (e.g. `vec3` for the unary ones, `vec3, vec3` for the
            // binary ones). The semantic pass must accept calls
            // with any vec/mat dim, not just the registered one,
            // because the codegen layer dispatches by first-arg
            // type. The `is_vec_mat_builtin` helper returns the
            // category (any-vec / any-mat) and the registered
            // return type for the special-case path; we use that
            // category to skip the per-arg type check.
            if let Some((cat, ret)) = is_vec_mat_builtin(&func_name) {
                if args.len() != f.params.len() {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "function `{}` expects {} args, got {}",
                            f.name, f.params.len(), args.len()
                        ),
                        span: span.clone(),
                    });
                }
                for (i, arg) in args.iter().enumerate() {
                    let at = check_expr(&arg.value, sym)?;
                    let ok = match cat {
                        VecMatCategory::AnyVec => vec_dim(&at).is_some(),
                        VecMatCategory::AnyMat => mat_dim(&at).is_some(),
                    };
                    if !ok {
                        return Err(SemanticError::TypeMismatch {
                            message: format!(
                                "arg {} of `{}` expected a {}, got `{}`",
                                i, f.name, cat.expected_name(),
                                at.display_name()
                            ),
                            span: span.clone(),
                        });
                    }
                }
                return Ok(ret);
            }
            // Verify arg types.
            for (i, arg) in args.iter().enumerate() {
                let at = check_expr(&arg.value, sym)?;
                let pt = &f.params[i].ty;
                if !type_compatible(&at, pt) {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "arg {} of `{}` expected `{}`, got `{}`",
                            i,
                            f.name,
                            pt.display_name(),
                            at.display_name()
                        ),
                        span: span.clone(),
                    });
                }
            }
            Ok(f.return_type.clone().unwrap_or(Type::Bool))
        }
        Expr::FieldAccess { target, field, span } => {
            let t = check_expr(target, sym)?;
            // vec2/3/4 component access. The allowed
            // field names are the component names - `x`/`y` for
            // vec2, `x`/`y`/`z` for vec3, `x`/`y`/`z`/`w` for
            // vec4. The C/Rust emitter also accepts `.xy` /
            // `.rgb` swizzles as a future extension.
            if let Some(d) = vec_dim(&t) {
                let allowed: &[&str] = match d {
                    2 => &["x", "y"],
                    3 => &["x", "y", "z"],
                    4 => &["x", "y", "z", "w"],
                    _ => &[],
                };
                if allowed.contains(&field.as_str()) {
                    return Ok(Type::Float(None));
                }
            }
            // `list.length` is the only field a list has.
            // It is an `int`, and the language calls it a compile-time
            // constant - the emitter still reads the length from the
            // list value, which is exactly the same number.
            if let Type::List(_) = &t {
                if field == "length" {
                    return Ok(Type::Int(None));
                }
                return Err(SemanticError::TypeMismatch {
                    message: format!("`list` has no field `{}` (only `length`)", field),
                    span: span.clone(),
                });
            }
            if let Type::Struct(sname, _) = &t {
                if let Some(s) = sym.structs.get(sname) {
                    for (n, ft) in &s.fields {
                        if n == field {
                            return Ok(ft.clone());
                        }
                    }
                }
            }
            Err(SemanticError::TypeMismatch {
                message: format!("no field `{}` on `{}`", field, t.display_name()),
                span: span.clone(),
            })
        }
        Expr::Index { target, index, span } => {
            let t = check_expr(target, sym)?;
            let _ = check_expr(index, sym)?;
            if let Type::List(inner) = t {
                Ok(*inner)
            } else {
                Err(SemanticError::TypeMismatch {
                    message: format!(
                        "indexing requires a `list`, got `{}`",
                        t.display_name()
                    ),
                    span: span.clone(),
                })
            }
        }
        Expr::ListLit { elements, .. } => {
            let first = elements
                .first()
                .map(|e| check_expr(e, sym))
                .transpose()?
                .unwrap_or(Type::Float(None));
            Ok(Type::List(Box::new(first)))
        }
        Expr::Tuple { elements, span } => {
            // A tuple is only meaningful as the return value of a
            // multi-return function. The actual element
            // types come from the function declaration, so we just
            // type-check each element and return a placeholder.
            //
            // `ponytail`: returning `Type::Float(None)` here is wrong in
            // general - a proper checker would plumb a tuple type. For
            // MVP we leave that to the codegen pass, which knows the
            // target type from the enclosing function's `multi_returns`.
            for e in elements {
                let _ = check_expr(e, sym)?;
            }
            let _ = span;
            Ok(Type::Float(None))
        }
        Expr::StructLit { name, fields, span } => {
            // a struct literal must (1) name an existing
            // struct, (2) name only fields that exist on that struct,
            // and (3) supply a value of the declared type for each
            // named field. The MVP does not require all fields to
            // be present (the codegen layer zero-initialises missing
            // fields via `Default` for the Rust backend and `{0}` for
            // the C backend) but it does require the names and types
            // it does supply to be correct.
            let s = sym.structs.get(name).cloned().ok_or_else(|| {
                SemanticError::UndefinedIdent {
                    name: name.clone(),
                    span: span.clone(),
                }
            })?;
            for (fname, fexpr) in fields {
                let field_ty = s
                    .fields
                    .iter()
                    .find(|(n, _)| n == fname)
                    .map(|(_, t)| t.clone())
                    .ok_or_else(|| SemanticError::TypeMismatch {
                        message: format!(
                            "no field `{}` on struct `{}`",
                            fname, name
                        ),
                        span: span.clone(),
                    })?;
                let val_ty = check_expr(fexpr, sym)?;
                if !type_compatible(&val_ty, &field_ty) {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "field `{}` of `{}` expects `{}`, got `{}`",
                            fname,
                            name,
                            field_ty.display_name(),
                            val_ty.display_name(),
                        ),
                        span: span.clone(),
                    });
                }
            }
            Ok(Type::Struct(name.clone(), span.clone()))
        }
        Expr::IfExpr { cond, then_branch, else_branch, .. } => {
            check_expr(cond, sym)?;
            let tt = check_expr(then_branch, sym)?;
            let et = check_expr(else_branch, sym)?;
            if type_compatible(&tt, &et) {
                Ok(tt)
            } else {
                Ok(tt) // widen to first branch for MVP
            }
        }
        Expr::Spawn { func, args, span, .. } => {
            // spawn returns a `task` handle, not the
            // callee's return value. `await(task)` recovers it.
            let call = Expr::Call {
                callee: func.clone(),
                args: args.clone(),
                span: span.clone(),
            };
            let fallback = check_expr(&call, sym)?;
            let payload = match func.as_ref() {
                Expr::Ident { name, .. } => sym.functions.get(name).map(|f| {
                    f.return_type
                        .clone()
                        .or_else(|| f.multi_returns.as_ref().map(|m| m[0].clone()))
                        .unwrap_or(fallback)
                }),
                _ => Some(fallback),
            };
            Ok(Type::Task(payload.map(Box::new)))
        }
        Expr::Parallel { func, args, span, .. } => {
            // `parallel f(xs, ys, h)` calls `f` once per
            // element of the list arguments. Each list argument stands
            // for the corresponding parameter, so the element type has
            // to match the parameter type; scalar arguments are
            // repeated for every call. Every list must have the same
            // length, which is a runtime property of the values - the
            // generated code checks it.
            let Expr::Ident { name, .. } = func.as_ref() else {
                return Err(SemanticError::NotCallable {
                    message: "`parallel` needs a function name".into(),
                    span: span.clone(),
                });
            };
            let f = sym
                .functions
                .get(name)
                .cloned()
                .ok_or_else(|| SemanticError::UndefinedIdent {
                    name: name.clone(),
                    span: span.clone(),
                })?;
            if args.len() != f.params.len() {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "`{}` takes {} argument(s), `parallel` passes {}",
                        name,
                        f.params.len(),
                        args.len()
                    ),
                    span: span.clone(),
                });
            }
            let mut has_list = false;
            for (i, a) in args.iter().enumerate() {
                let at = check_expr(&a.value, sym)?;
                let pt = &f.params[i].ty;
                match &at {
                    Type::List(inner) => {
                        has_list = true;
                        if !type_compatible(inner, pt) {
                            return Err(SemanticError::TypeMismatch {
                                message: format!(
                                    "`parallel` argument {} has element type `{}`, but `{}` expects `{}`",
                                    i,
                                    inner.display_name(),
                                    name,
                                    pt.display_name()
                                ),
                                span: a.span.clone(),
                            });
                        }
                    }
                    _ if !type_compatible(&at, pt) => {
                        return Err(SemanticError::TypeMismatch {
                            message: format!(
                                "`parallel` argument {} has type `{}`, but `{}` expects `{}`",
                                i,
                                at.display_name(),
                                name,
                                pt.display_name()
                            ),
                            span: a.span.clone(),
                        });
                    }
                    _ => {}
                }
            }
            let ret = f.return_type.clone().unwrap_or(Type::Bool);
            let payload = if has_list {
                Type::List(Box::new(ret))
            } else {
                ret
            };
            Ok(Type::Task(Some(Box::new(payload))))
        }
        Expr::Random { candidates, args, span } => {
            if candidates.is_empty() {
                return Err(SemanticError::TypeMismatch {
                    message: "`random` needs at least one candidate".into(),
                    span: span.clone(),
                });
            }
            let mut result: Option<Type> = None;
            match args {
                // value mode. Every candidate must have a
                // compatible type; the result is that type (a task
                // candidate is awaited first, so its result is the
                // awaited value).
                None => {
                    for c in candidates {
                        let t = check_expr(c, sym)?;
                        if !matches!(
                            t,
                            Type::Float(_) | Type::Int(_) | Type::Uint(_) | Type::Bool | Type::Task(_)
                        ) {
                            return Err(SemanticError::TypeMismatch {
                                message: format!(
                                    "`random` value candidates must be numeric, got `{}`",
                                    t.display_name()
                                ),
                                span: span.clone(),
                            });
                        }
                        let t = match t {
                            Type::Task(inner) => inner.map(|b| *b).unwrap_or(Type::Float(None)),
                            other => other,
                        };
                        match result.as_ref() {
                            Some(first) if !type_compatible(first, &t) => {
                                return Err(SemanticError::TypeMismatch {
                                    message: format!(
                                        "`random` candidates have incompatible types: `{}` and `{}`",
                                        first.display_name(),
                                        t.display_name()
                                    ),
                                    span: span.clone(),
                                });
                            }
                            Some(_) => {}
                            None => result = Some(t),
                        }
                    }
                }
                // Function mode: every candidate must name a function
                // and accept the trailing argument list.
                Some(args) => {
                    for c in candidates {
                        if !matches!(c, Expr::Ident { .. }) {
                            return Err(SemanticError::TypeMismatch {
                                message: "`random(...)(args)` candidates must be function names"
                                    .into(),
                                span: span.clone(),
                            });
                        }
                        let call = Expr::Call {
                            callee: Box::new(c.clone()),
                            args: args.clone(),
                            span: span.clone(),
                        };
                        let t = check_expr(&call, sym)?;
                        match result.as_ref() {
                            Some(first) if !type_compatible(first, &t) => {
                                return Err(SemanticError::TypeMismatch {
                                    message: format!(
                                        "`random` candidates return incompatible types: `{}` and `{}`",
                                        first.display_name(),
                                        t.display_name()
                                    ),
                                    span: span.clone(),
                                });
                            }
                            Some(_) => {}
                            None => result = Some(t),
                        }
                    }
                }
            }
            Ok(result.unwrap_or(Type::Float(None)))
        }
        Expr::Await { task, .. } => {
            let t = check_expr(task, sym)?;
            match t {
                Type::Task(Some(inner)) => Ok(*inner),
                Type::Task(None) => Err(SemanticError::TypeMismatch {
                    message: "`await` on a task that produces no value".into(),
                    span: task.span(),
                }),
                other => Err(SemanticError::TypeMismatch {
                    message: format!(
                        "`await` expects a `task` handle, got `{}`",
                        other.display_name()
                    ),
                    span: task.span(),
                }),
            }
        }
        Expr::Send { target, message, span } => {
            if !sym.sequences.iter().any(|q| q.name == *target) {
                return Err(SemanticError::UndefinedIdent {
                    name: format!("sequence `{}`", target),
                    span: span.clone(),
                });
            }
            let ty = check_expr(message, sym)?;
            // a message is a struct value. The runtime
            // marshals struct fields through a fixed f64 slot array, so
            // non-struct messages are rejected here rather than
            // emitting silently-wrong packing code.
            match ty {
                Type::Struct(_, _) => {}
                other => {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "`send` message must be a struct value, got `{}`",
                            other.display_name()
                        ),
                        span: message.span(),
                    })
                }
            }
            // `send` is a statement-shaped call; the bool is a
            // delivery-success flag (always true in the MVP).
            Ok(Type::Bool)
        }
        Expr::Broadcast { message, span } => {
            let ty = check_expr(message, sym)?;
            match ty {
                Type::Struct(_, _) => {}
                other => {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "`broadcast` message must be a struct value, got `{}`",
                            other.display_name()
                        ),
                        span: span.clone(),
                    })
                }
            }
            Ok(Type::Bool)
        }
        Expr::Receive { pattern, span } => {
            // The MVP requires a `Pattern{...}` filter: it names the
            // message type, which is what makes the binding's type
            // statically known. Upgrade path: an untyped `receive`
            // returning an optional message once the type system has
            // one.
            let pattern = match pattern {
                Some(p) => p,
                None => {
                    return Err(SemanticError::TypeMismatch {
                        message: "`receive` needs a `Pattern{...}` filter (the message \
                                  type cannot be inferred without it)"
                            .to_string(),
                        span: span.clone(),
                    })
                }
            };
            let st = sym
                .structs
                .get(&pattern.type_name)
                .ok_or_else(|| SemanticError::TypeMismatch {
                    message: format!("unknown message type `{}`", pattern.type_name),
                    span: pattern.span.clone(),
                })?
                .clone();
            for (field, _op, value) in &pattern.conditions {
                if !st.fields.iter().any(|(n, _)| n == field) {
                    return Err(SemanticError::TypeMismatch {
                        message: format!(
                            "message type `{}` has no field `{}`",
                            pattern.type_name, field
                        ),
                        span: span.clone(),
                    });
                }
                check_expr(value, sym)?;
            }
            Ok(Type::Struct(pattern.type_name.clone(), span.clone()))
        }
        Expr::AsType { value, target, .. } => {
            check_expr(value, sym)?;
            Ok(target.clone())
        }
        Expr::NarrowCast { value, target, .. } => {
            check_expr(value, sym)?;
            Ok(target.clone())
        }
        Expr::Conv { value, .. } => {
            let t = check_expr(value, sym)?;
            // The conv flips float<->int.
            match t {
                Type::Float(_) => Ok(Type::Int(None)),
                Type::Int(_) => Ok(Type::Float(None)),
                other => Ok(other),
            }
        }
        Expr::IfPresent { param, then_branch, absent_branch, .. } => {
            if sym.lookup(param).is_none() && sym.lookup_func(param).is_none() {
                return Err(SemanticError::UndefinedIdent {
                    name: param.clone(),
                    span: Span::dummy(),
                });
            }
            check_block(then_branch, sym)?;
            if let Some(b) = absent_branch {
                check_block(b, sym)?;
            }
            Ok(Type::Bool)
        }
        Expr::SpasioInput { mode, channel, .. } => {
            sym.record_channel(mode.clone(), channel.clone(), ChannelDir::Input);
            Ok(Type::Float(None))
        }
        Expr::SpasioOutput { mode, channel, .. } => {
            sym.record_channel(mode.clone(), channel.clone(), ChannelDir::Output);
            Ok(Type::Bool)
        }
    }
}

fn binary_op_type(op: &BinOp, lt: &Type, rt: &Type, span: &Span) -> SemanticResult<Type> {
    use BinOp::*;
    // vec-vec / vec-scalar / scalar-vec arithmetic.
    // `+` / `-` accept vec-vec of the same dim. `*` / `/` accept
    // vec-scalar (and the language example also has scalar-vec). The
    // matching dim is enforced for vec-vec `+`/`-` here so
    // mismatches surface as compile errors instead of C
    // compile errors.
    if matches!(op, Add | Sub) {
        if let (Some(ld), Some(rd)) = (vec_dim(lt), vec_dim(rt)) {
            if ld == rd {
                return Ok(lt.clone());
            }
            return Err(SemanticError::TypeMismatch {
                message: format!(
                    "operator `{}` not defined on `vec{}` and `vec{}` (dimensions must match)",
                    op, ld, rd
                ),
                span: span.clone(),
            });
        }
        // vec + scalar / scalar + vec - allowed by language
        // example (vec3(1, 2, 3) + 1.0).
        if vec_dim(lt).is_some() && is_numeric(rt) {
            return Ok(lt.clone());
        }
        if is_numeric(lt) && vec_dim(rt).is_some() {
            return Ok(rt.clone());
        }
    }
    if matches!(op, Mul | Div) {
        // vec * scalar, scalar * vec, vec * vec (forbidden by language
        // for arithmetic - `hadamard` is the explicit name).
        if let (Some(_ld), Some(_rd)) = (vec_dim(lt), vec_dim(rt)) {
            return Err(SemanticError::TypeMismatch {
                message: format!(
                    "operator `{}` not defined on `{}` and `{}`; use `hadamard(a, b)` for elementwise product",
                    op, lt.display_name(), rt.display_name()
                ),
                span: span.clone(),
            });
        }
        // vec * scalar, vec / scalar, scalar * vec (the last one
        // is uncommon but the codegen supports it; we accept it
        // here too).
        if vec_dim(lt).is_some() && is_numeric(rt) {
            return Ok(lt.clone());
        }
        if is_numeric(lt) && vec_dim(rt).is_some() {
            return Ok(rt.clone());
        }
    }
    match op {
        Eq | Ne | Lt | Le | Gt | Ge | And | Or => Ok(Type::Bool),
        Add | Sub | Mul | Div | Pow => {
            if !is_numeric(lt) || !is_numeric(rt) {
                return Err(SemanticError::TypeMismatch {
                    message: format!(
                        "operator `{}` not defined on `{}` and `{}`",
                        op, lt.display_name(), rt.display_name()
                    ),
                    span: span.clone(),
                });
            }
            // int / uint + int / uint => int; otherwise float
            if matches!(lt, Type::Float(_)) || matches!(rt, Type::Float(_)) {
                Ok(Type::Float(None))
            } else {
                Ok(Type::Int(None))
            }
        }
    }
}

fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Float(_) | Type::Int(_) | Type::Uint(_))
}

fn list_element_type(t: &Type) -> Type {
    if let Type::List(inner) = t {
            *inner.clone()
    } else {
        t.clone()
    }
}

/// Short, human-readable name of an expression kind. Used in
/// `@await` next-stmt error messages so the user sees something
/// like "got a let with `2 + 2` on the right-hand side (expected
/// `spasio:input[...](\"...\")`)" rather than an opaque `Expr::Binary`.
/// `ponytail`: this is a deliberately lossy description - it omits
/// ranges, value-domain info, and most other details because the
/// error message is meant as a hint, not a full type dump.
fn expr_kind_name(e: &Expr) -> &'static str {
    match e {
        Expr::IntLit { .. } => "an integer literal",
        Expr::FloatLit { .. } => "a float literal",
        Expr::BoolLit { .. } => "a bool literal",
        Expr::StringLit { .. } => "a string literal",
        Expr::Ident { .. } => "an identifier",
        Expr::Binary { .. } => "a binary expression",
        Expr::Unary { .. } => "a unary expression",
        Expr::Call { .. } => "a function call",
        Expr::FieldAccess { .. } => "a field access",
        Expr::Index { .. } => "an index expression",
        Expr::IfExpr { .. } => "an `if` expression",
        Expr::ListLit { .. } => "a list literal",
        Expr::StructLit { .. } => "a struct literal",
        Expr::Tuple { .. } => "a tuple literal",
        Expr::Spawn { .. } => "a `spawn` expression",
        Expr::Parallel { .. } => "a `parallel` expression",
        Expr::Await { .. } => "an `await` expression",
        Expr::Random { .. } => "a `random` expression",
        Expr::Send { .. } => "a `send` call",
        Expr::Broadcast { .. } => "a `broadcast` call",
        Expr::Receive { .. } => "a `receive` call",
        Expr::AsType { .. } => "an `as_type` call",
        Expr::NarrowCast { .. } => "a `narrow_cast` call",
        Expr::Conv { .. } => "a `to_int`/`to_float` call",
        Expr::IfPresent { .. } => "an `if_present` expression",
        Expr::SpasioInput { .. } => "a `spasio:input` call",
        Expr::SpasioOutput { .. } => "a `spasio:output` call",
        Expr::NativeCall { .. } => "a playground call",
    }
}

/// Whether a type is a list (the function-local list boundary is
/// expressed with this predicate: return types, public parameters and
/// struct fields are checked against it).
fn type_contains_list(t: &Type) -> bool {
    matches!(t, Type::List(_))
}

/// Loose type compatibility: same kind, ignoring range. The range inference
/// pass handles the strict range checks later.
fn type_compatible(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Float(_), Type::Float(_))
        | (Type::Int(_), Type::Int(_))
        | (Type::Uint(_), Type::Uint(_))
        | (Type::Bool, Type::Bool)
        | (Type::String, Type::String) => true,
        (Type::List(x), Type::List(y)) => type_compatible(x, y),
        // vectors are compared by dimension only; per-component
        // ranges are checked by the range-inference pass. Matrices are
        // matched on the kind (`Mat3` vs `Mat4`).
        (Type::Vec2(_, _), Type::Vec2(_, _))
        | (Type::Vec3(_, _, _), Type::Vec3(_, _, _))
        | (Type::Vec4(_, _, _, _), Type::Vec4(_, _, _, _))
        | (Type::Mat3, Type::Mat3)
        | (Type::Mat4, Type::Mat4) => true,
        (Type::Struct(x, _), Type::Struct(y, _)) => x == y,
        // numeric promotions in expressions
        (Type::Int(_), Type::Float(_)) | (Type::Float(_), Type::Int(_)) => true,
        _ => false,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use spass_parser::Parser;

    fn parse(src: &str) -> Module {
        Parser::parse_source(src, "<test>").expect("parse")
    }

    #[test]
    fn simple_function_passes() {
        let m = parse("func add(a: int, b: int): int { return a + b }");
        let typed = analyze(&m).expect("analyse");
        assert_eq!(typed.functions.len(), 1);
        assert_eq!(typed.functions[0].name, "add");
    }

    #[test]
    fn undefined_ident_reports_e005() {
        let m = parse("func f(): int { return x }");
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::UndefinedIdent { .. }));
    }

    #[test]
    fn recursion_reports_e008() {
        let m = parse("func f(): int { return f() }");
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::Recursive { .. }));
    }

    #[test]
    fn mutual_recursion_reports_e008() {
        let m = parse(
            "func a(): int { return b() } func b(): int { return a() }"
        );
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::Recursive { .. }));
    }

    #[test]
    fn duplicate_function_reports_e012() {
        // The parser already enforces uniqueness, so we test that the
        // parser-side E012 fires (which has the same error code).
        use spass_parser::ParseError;
        let result = spass_parser::Parser::parse_source(
            "func f(): int { return 1 } func f(): int { return 2 }",
            "<test>",
        );
        assert!(matches!(result, Err(ParseError::DuplicateFunction { .. })));
    }

    #[test]
    fn arity_mismatch_reports_e006() {
        let m = parse("func add(a: int, b: int): int { return a } func main(): int { return add(1) }");
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::TypeMismatch { .. }));
    }

    /// `out` parameters and a return type are mutually
    /// exclusive. The two conflict at the ABI level (the call site must
    /// either allocate a return slot or pass extra pointer arguments)
    /// and the language forbids mixing them.
    #[test]
    fn out_param_with_return_is_rejected() {
        let m = parse("func f(out r: float): float { r = 1.0; return 0.0 }");
        let err = analyze(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("mutually exclusive"),
                "expected `mutually exclusive` error, got: {}", msg);
    }

    /// `out` parameters with no return type and no multi-return is the
    /// normal form - it should pass.
    #[test]
    fn out_param_only_is_accepted() {
        let m = parse("func f(out r: float) { r = 1.0 }");
        analyze(&m).expect("out-only function should pass");
    }

    // ----------------------------------------------------------------
    // if_present - semantic tests
    // ----------------------------------------------------------------

    /// E016: `if_present(name)` must reference a real function parameter.
    /// A `let`-bound local with the same name must NOT be accepted
    /// because the dispatcher cannot tell at compile time whether the
    /// caller supplied it (it was bound inside the body, so it always
    /// exists by the time the gate is checked).
    #[test]
    fn if_present_referencing_local_is_rejected() {
        let src = r#"
            func f(x: float): float {
                let y: float = 1.0
                if_present(y) {
                    return x
                }
                return 0.0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::IfPresentNotParam { .. }),
            "expected IfPresentNotParam, got {:?}", err
        );
    }

    /// E016: a function with `if_present` for two different parameters
    /// is rejected because the MVP only supports a binary
    /// present/absent split. A 2^N matrix of variants would be needed
    /// for N gated parameters, which the language doesn't promise.
    #[test]
    fn if_present_with_two_distinct_params_is_rejected() {
        let src = r#"
            func f(x: float, y: float): float {
                if_present(x) {
                    return x
                }
                if_present(y) {
                    return y
                }
                return 0.0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::IfPresentMultiple { .. }),
            "expected IfPresentMultiple, got {:?}", err
        );
    }

    /// Multiple `if_present` statements for the SAME parameter are
    /// fine - they all share the same dispatch decision.
    #[test]
    fn if_present_repeated_for_same_param_is_accepted() {
        let src = r#"
            func f(x: float, y: float): float {
                if_present(x) {
                    return y
                } @absent {
                    return 0.0 - y
                }
                if_present(x) {
                    return 2.0 * y
                } @absent {
                    return 0.0 - 2.0 * y
                }
            }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("repeated if_present for the same param should pass");
        assert!(typed.functions[0].has_if_present);
        assert_eq!(typed.functions[0].if_present_param.as_deref(), Some("x"));
    }

    /// The absent branch can be omitted; codegen drops the
    /// `if_present` in the absent variant so the statements following
    /// the gate run normally.
    #[test]
    fn if_present_without_absent_branch_is_accepted() {
        let src = r#"
            func f(x: float, y: float): float {
                let pre: float = 1.0
                if_present(x) {
                    return y
                }
                let post: float = 2.0
                return pre + post
            }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("if_present without @absent should pass");
        assert!(typed.functions[0].has_if_present);
        assert_eq!(typed.functions[0].if_present_param.as_deref(), Some("x"));
    }

    /// bare tag names declared via `@tag(A, B, key=val,...)`
    /// are lifted onto the `TypedFunc`. Valued items do not appear in the
    /// `tags` list (they are `@dispatch`-only metadata).
    #[test]
    fn bare_tags_are_lifted_to_typed_func() {
        let m = parse(
            "@tag(noise, macro, strength=0.7) \
             func f(): float { return 1.0 }",
        );
        let typed = analyze(&m).expect("analyse");
        assert_eq!(
            typed.functions[0].tags,
            vec!["noise".to_string(), "macro".to_string()]
        );
    }

    /// Functions without `@tag` end up with an empty `tags` vec.
    #[test]
    fn no_tag_annotation_yields_empty_tags() {
        let m = parse("func f(): float { return 1.0 }");
        let typed = analyze(&m).expect("analyse");
        assert!(typed.functions[0].tags.is_empty());
    }

    /// Duplicate bare names in a single `@tag(...)` are collapsed
    /// (keeping the first occurrence).
    #[test]
    fn duplicate_bare_tags_are_deduplicated() {
        let m = parse("@tag(noise, noise, macro) func f(): float { return 1.0 }");
        let typed = analyze(&m).expect("analyse");
        assert_eq!(typed.functions[0].tags, vec!["noise".to_string(), "macro".to_string()]);
    }

    // ----------------------------------------------------------------
    // @dispatch semantic tests
    // ----------------------------------------------------------------

    /// E014: a tag selector that matches no function fails.
    /// The `@dispatch` directive is a statement inside a function body;
    /// we embed it in `main` and assert that the unmatched selector is
    /// reported.
    #[test]
    fn dispatch_no_match_reports_e014() {
        let src = r#"
            @tag(noise) func f(pos: float, h: float): float { return h }
            func main(pos: float, h: float): float {
                @dispatch(tags = {unknown_tag})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::DispatchNoMatch { .. }),
                "expected DispatchNoMatch, got {:?}", err);
    }

    /// E002: a bind entry is missing for a matched function's parameter.
    /// The selector matches both functions; the dispatch only binds one
    /// of the two `extra` params, so the other function fails to bind.
    #[test]
    fn dispatch_missing_bind_reports_e002() {
        let src = r#"
            @tag(erosion) func erode(pos: float, h: float, strength: float): float { return h - strength }
            @tag(erosion) func deposit(pos: float, h: float, rate: float): float { return h + rate }
            func main(pos: float, h: float): float {
                @dispatch(tags = {erosion}, bind = {strength: 0.5})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("E002") || msg.contains("binding missing"),
                "expected E002 missing bind, got: {}", msg);
    }

    /// E004: `@before`/`@after` cycle is reported.
    /// `a` must run before `b` and `b` must run before `a` -> cycle.
    #[test]
    fn dispatch_order_cycle_reports_e004() {
        let src = r#"
            @tag(cyc)
            func a(pos: float, h: float): float {
                @before(b, require)
                return h
            }
            @tag(cyc)
            func b(pos: float, h: float): float {
                @before(a, require)
                return h
            }
            func main(pos: float, h: float): float {
                @dispatch(tags = {cyc})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("E004") || msg.contains("cycle"),
                "expected E004 cycle, got: {}", msg);
    }

    /// E004 again, this time via `@after` (the reverse direction).
    #[test]
    fn dispatch_order_cycle_via_after_reports_e004() {
        let src = r#"
            @tag(cyc2)
            func a(pos: float, h: float): float {
                @after(b, require)
                return h
            }
            @tag(cyc2)
            func b(pos: float, h: float): float {
                @after(a, require)
                return h
            }
            func main(pos: float, h: float): float {
                @dispatch(tags = {cyc2})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("E004") || msg.contains("cycle"),
                "expected E004 cycle, got: {}", msg);
    }

    /// OR selector matches functions with at least one of the listed tags.
    /// Both `a` and `b` have one of `{alpha, beta}` so both match.
    #[test]
    fn dispatch_or_selector_matches_either() {
        let src = r#"
            @tag(alpha) func a(pos: float, h: float): float { return h }
            @tag(beta) func b(pos: float, h: float): float { return h }
            func main(pos: float, h: float): float {
                @dispatch(tags = {alpha, beta})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("OR dispatch should analyse cleanly");
    }

    /// AND selector requires the function to have ALL listed bare tags.
    /// `a` only has `alpha`, so the AND selector `[alpha, beta]` matches
    /// nothing -> E014.
    #[test]
    fn dispatch_and_selector_requires_all() {
        let src = r#"
            @tag(alpha) func a(pos: float, h: float): float { return h }
            @tag(beta) func b(pos: float, h: float): float { return h }
            func main(pos: float, h: float): float {
                @dispatch(tags = [alpha, beta])
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::DispatchNoMatch { .. }),
                "expected DispatchNoMatch, got {:?}", err);
    }

    /// `@before(F, require)` puts the declaring function before F in the
    /// expanded call order. The semantic pass topo-sorts; we can verify
    /// the resolved dispatch is well-formed.
    #[test]
    fn dispatch_topo_sort_with_before() {
        let src = r#"
            @tag(ord)
            func a(pos: float, h: float): float {
                @before(b, require)
                return h
            }
            @tag(ord)
            func b(pos: float, h: float): float { return h }
            func main(pos: float, h: float): float {
                @dispatch(tags = {ord})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("dispatch with @before should analyse cleanly");
    }

    /// Valued-tag selector filters functions whose `@tag(key=val)` matches.
    /// Only `b` has `quality=high`; `a` is `quality=low`. The selector
    /// `{quality=high}` matches only `b`.
    #[test]
    fn dispatch_valued_tag_selector() {
        let src = r#"
            @tag(quality=low) func a(pos: float, h: float): float { return h }
            @tag(quality=high) func b(pos: float, h: float): float { return h }
            func main(pos: float, h: float): float {
                @dispatch(tags = {quality=high})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("valued-tag dispatch should analyse cleanly");
    }

    // ----------------------------------------------------------------
    // @checkpoint / @resume semantic tests
    // ----------------------------------------------------------------

    /// E007: `@resume name` must reference a `@checkpoint` declared
    /// somewhere in the module. A typo in the resume name is rejected.
    #[test]
    fn resume_with_undefined_checkpoint_reports_e007() {
        let src = r#"
            func driver(): int {
                @resume nonexistent
                return 0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::CheckpointUndefined { .. }),
                "expected CheckpointUndefined, got {:?}", err);
    }

    /// E007: a `@checkpoint name` may not be declared twice in the
    /// same module. The names are globally unique so a cross-function
    /// `@resume` has exactly one target.
    #[test]
    fn duplicate_checkpoint_name_reports_e007() {
        let src = r#"
            func a(): int {
                @checkpoint loop
                return 0
            }
            func b(): int {
                @checkpoint loop
                return 0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::CheckpointDuplicate { .. }),
                "expected CheckpointDuplicate, got {:?}", err);
    }

    /// E007: `@resume` may not appear in the `else` branch of an `if`.
    /// The language wants symmetric control flow at the same nesting depth.
    /// Nested `if`s inside the `else` branch are themselves a fresh
    /// nesting depth and `@resume` is allowed there.
    #[test]
    fn resume_in_else_branch_reports_e007() {
        let src = r#"
            func driver(): int {
                @checkpoint loop
                if true {
                    @resume loop
                } else {
                    @resume loop
                }
                return 0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::ResumeInElseBranch { .. }),
                "expected ResumeInElseBranch, got {:?}", err);
    }

    /// `@resume` is allowed inside an `if` placed inside an `else`
    /// branch - the language only forbids it at the direct `else` depth.
    #[test]
    fn resume_in_nested_inside_else_is_allowed() {
        let src = r#"
            func driver(): int {
                @checkpoint loop
                if true {
                    return 0
                } else {
                    if true {
                        @resume loop
                    } else {
                        return 1
                    }
                }
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("nested-if @resume inside else should be allowed");
    }

    /// `@resume name` may reference a `@checkpoint` declared in a
    /// different function - this is the cross-function case.
    #[test]
    fn cross_function_resolve_resume_target() {
        let src = r#"
            func producer(): int {
                @checkpoint shared
                return 0
            }
            func consumer(): int {
                @resume shared
                return 1
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("cross-function @resume should analyse cleanly");
    }

    // ========================================================================
    // @await
    // ========================================================================

    /// The happy path: `@await` followed by a single-name
    /// `let v = spasio:input[mode]("name")` binding is accepted, and
    /// the resulting `TypedFunc` carries the binding so the codegen
    /// layer can reserve an Await slot.
    #[test]
    fn await_with_valid_spasio_input_lifts_binding() {
        let src = r#"
            func f(x: float): float {
                @await
                let v: float = spasio:input[file]("my_channel")
                return v
            }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("valid @await should analyse cleanly");
        let func = typed.functions.last().expect("at least one function");
        let binding = func.await_binding.as_ref().expect("await binding lifted");
        assert_eq!(binding.name, "v");
        assert_eq!(binding.channel, "my_channel");
        assert!(matches!(binding.mode, spass_parser::ast::SpasioMode::File));
    }

    /// `@await` followed by a non-`let` next statement (e.g. a
    /// `return`) is a shape error. The user gets a clear compile-
    /// time error rather than a confusing codegen failure. The
    /// `other` arm of the @await next-stmt check produces
    /// `AwaitNextLetShape` with a description of what the next
    /// statement actually was.
    #[test]
    fn await_with_non_let_next_stmt_is_rejected() {
        let src = r#"
            func f(): float {
                @await
                return 1.0
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::AwaitNextLetShape { .. }),
                "expected AwaitNextLetShape, got {:?}", err);
    }

    /// `@await` followed by a `let` whose right-hand side is not a
    /// `spasio:input[...]("...")` is rejected so the runtime
    /// never sees an ambiguous value.
    #[test]
    fn await_with_non_spasio_let_rhs_is_rejected() {
        let src = r#"
            func f(): float {
                @await
                let v: float = 1.0
                return v
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::AwaitNextLetShape { .. }),
                "expected AwaitNextLetShape, got {:?}", err);
    }

    /// `@await` followed by a destructuring `let` (more than one
    /// name) is rejected - the MVP's Await slot is a single 8-byte
    /// slot.
    #[test]
    fn await_with_destructuring_let_is_rejected() {
        let src = r#"
            func f(): float {
                @await
                let (a, b) = spasio:input[file]("chan")
                return a
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::AwaitNextLetShape { .. }),
                "expected AwaitNextLetShape, got {:?}", err);
    }

    /// `@gpu` and `@await` cannot coexist. The
    /// `GpuWithAwait` error is raised before body checking so the
    /// user gets a precise pointer.
    #[test]
    fn await_in_gpu_function_is_rejected() {
        let src = r#"
            @gpu
            func f(): float {
                @await
                let v: float = spasio:input[file]("chan")
                return v
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::GpuWithAwait { .. }),
                "expected GpuWithAwait, got {:?}", err);
    }


    // ========================================================================
    // Test system
    // ========================================================================

    /// A `test "name" { body }` declaration co-existing with regular
    /// `func` definitions must analyse cleanly: the test body's
    /// identifiers (e.g. `height`) must resolve, and `assert(...)` on
    /// a local must pass the E005 check.
    #[test]
    fn test_item_with_function_call_body_passes() {
        let src = r#"
            func height(x: float, z: float): float { return 0.0 }

            test "height at origin" {
                let h: float = height(0.0, 0.0)
                assert(h in [-0.01, 0.01])
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("test + func should analyse");
    }

    /// `assert(ident in [lo, hi])` with identifier bounds (`sea`,
    /// `max_h`) must work - the test body is allowed to define
    /// `let sea: float = 62.0` and reference it inside the range.
    /// The semantic pass type-checks the assertion; the test driver
    /// resolves the value at test time.
    #[test]
    fn test_with_identifier_range_bounds() {
        let src = r#"
            test "named bounds" {
                let sea: float = 62.0
                let max_h: float = 380.0
                let h: float = 100.0
                assert(h in [sea, max_h])
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("identifier bounds should analyse");
    }

    /// `assert(approx(value, target, tolerance))` must pass when the
    /// value identifier is defined and the target/tolerance
    /// expressions type-check.
    #[test]
    fn test_with_approx_assertion_passes() {
        let src = r#"
            test "approx" {
                let s: float = 0.3
                let target: float = 0.3
                let tol: float = 0.0001
                assert(approx(s, target, tol))
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("approx assertion should analyse");
    }

    /// A test that references an undefined identifier inside
    /// `assert(...)` must surface as E005. This is the one check the
    /// semantic pass performs for `assert` - anything else is the
    /// test driver's job.
    #[test]
    fn test_with_undefined_assert_value_reports_e005() {
        let src = r#"
            test "oops" {
                assert(missing in [0.0, 1.0])
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::UndefinedIdent { .. }),
                "expected E005, got {:?}", err);
    }

    /// `with` block parameters must be type-checked; a missing
    /// default (parse error) is the parser's job, but a wrong
    /// type in the default expression would surface here. We just
    /// verify the happy path: a `with`-block test with float
    /// defaults analyses cleanly.
    #[test]
    fn test_with_block_params_define_scope() {
        let src = r#"
            test "parameterised" with {
                x: float = 1.0,
                y: float = 2.0
            } {
                let h: float = x + y
                assert(h in [2.5, 3.5])
            }
        "#;
        let m = parse(src);
        analyze(&m).expect("parameterised test should analyse");
    }

    // ========================================================================
    // vec/mat semantic checks
    // ========================================================================

    /// `vec3 + vec3` is well-typed; the result is `vec3`.
    #[test]
    fn vec_plus_vec_is_vec() {
        let src = r#"
            func f(a: vec3, b: vec3): vec3 { return a + b }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("vec+vec should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Vec3(_, _, _))), "got {:?}", ret);
    }

    /// `vec3 + float` is well-typed; the result is `vec3`. The
    /// scalar is added component-wise.
    #[test]
    fn vec_plus_scalar_is_vec() {
        let src = r#"
            func f(a: vec3): vec3 { return a + 1.0 }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("vec+scalar should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Vec3(_, _, _))), "got {:?}", ret);
    }

    /// `float + vec3` (commutativity) is also well-typed.
    #[test]
    fn scalar_plus_vec_is_vec() {
        let src = r#"
            func f(a: vec3): vec3 { return 1.0 + a }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("scalar+vec should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Vec3(_, _, _))), "got {:?}", ret);
    }

    /// `vec2` and `vec3` are not compatible - adding a 2-vec and a
    /// 3-vec is rejected at the semantic layer.
    #[test]
    fn vec_dim_mismatch_is_rejected() {
        let src = r#"
            func f(a: vec2, b: vec3): vec3 { return a + b }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::TypeMismatch { .. }),
                "expected TypeMismatch, got {:?}", err);
    }

    /// Field access on a vec must use a valid component name. The
    /// vec3 components are `x`/`y`/`z`; anything else is rejected
    /// at the semantic layer.
    #[test]
    fn vec_field_must_be_valid_component() {
        let src = r#"
            func f(a: vec3): float { return a.w }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::TypeMismatch { .. }),
                "expected TypeMismatch, got {:?}", err);
    }

    /// Field access with a valid component name is well-typed and
    /// yields a `float`.
    #[test]
    fn vec_field_valid_component_returns_float() {
        let src = r#"
            func f(a: vec3): float { return a.y }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("vec3.y should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Float(_))), "got {:?}", ret);
    }

    /// `dot(vec3, vec3)` returns a `float`.
    #[test]
    fn vec_dot_returns_float() {
        let src = r#"
            func f(a: vec3, b: vec3): float { return dot(a, b) }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("dot should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Float(_))), "got {:?}", ret);
    }

    /// `cross(vec3, vec3)` returns a `vec3`.
    #[test]
    fn vec_cross_returns_vec3() {
        let src = r#"
            func f(a: vec3, b: vec3): vec3 { return cross(a, b) }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("cross should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Vec3(_, _, _))), "got {:?}", ret);
    }

    /// `mk_vec3(x, y, z)` constructs a `vec3` from three scalars.
    #[test]
    fn mk_vec3_returns_vec3() {
        let src = r#"
            func f(x: float, y: float, z: float): vec3 { return mk_vec3(x, y, z) }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("mk_vec3 should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Vec3(_, _, _))), "got {:?}", ret);
    }

    // ====================================================================
    // struct literal field validation (D2)
    // ====================================================================

    /// A struct literal that names a non-existent field is rejected
    /// with E006.
    #[test]
    fn struct_lit_with_unknown_field_reports_e006() {
        let src = r#"
            struct Point { x: float, y: float }
            func f(): Point { return Point{x: 1.0, bogus: 2.0} }
        "#;
        let m = parse(src);
        let err = analyze(&m).err().expect("bogus field should be rejected");
        assert!(matches!(err, SemanticError::TypeMismatch { .. }), "got {:?}", err);
    }

    /// A struct literal whose field value type doesn't match the
    /// declared field type is rejected with E006. We use a
    /// `vec3` field given an `int` literal because `int`/`float`
    /// are treated as compatible by the numeric-promotion rule
    /// (so a plain scalar mismatch would silently pass).
    #[test]
    fn struct_lit_with_wrong_field_type_reports_e006() {
        let src = r#"
            struct Curve { slope: float, dir: vec3 }
            func f(): Curve { return Curve{slope: 1.0, dir: 7} }
        "#;
        let m = parse(src);
        let err = analyze(&m).err().expect("vec3 field given int should be rejected");
        assert!(matches!(err, SemanticError::TypeMismatch { .. }), "got {:?}", err);
    }

    /// A struct literal that names every field with the right type
    /// passes.
    #[test]
    fn struct_lit_with_matching_fields_passes() {
        let src = r#"
            struct Point { x: float, y: float }
            func f(): Point { return Point{x: 1.0, y: 2.0} }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("matching struct literal should type-check");
        let ret = &typed.functions[0].return_type;
        assert!(matches!(ret, Some(Type::Struct(n, _)) if n == "Point"), "got {:?}", ret);
    }

    // ====================================================================
    // W002 warning collection (D4)
    // ====================================================================

    // ===================================================================
    // built-in signatures
    // ===================================================================

    /// `lerp(a, b, t)` is ternary per and both runtimes
    /// (`spass_lerp` in runtime.c and the Rust prelude). Regression test:
    /// it was registered via the `binary` helper, so every language-conformant
    /// call was rejected with "function `lerp` expects 2 args, got 3".
    #[test]
    fn lerp_is_ternary() {
        let src = r#"
            func f(a: float, b: float, t: float): float { return lerp(a, b, t) }
        "#;
        let m = parse(src);
        analyze(&m).expect("lerp(a, b, t) must type-check");
    }

    /// `clamp(v, lo, hi)` is ternary.
    #[test]
    fn clamp_is_ternary() {
        let src = r#"
            func f(v: float): float { return clamp(v, 0.0, 1.0) }
        "#;
        let m = parse(src);
        analyze(&m).expect("clamp(v, lo, hi) must type-check");
    }

    /// `smoothstep(e0, e1, x)` is ternary.
    #[test]
    fn smoothstep_is_ternary() {
        let src = r#"
            func f(x: float): float { return smoothstep(0.0, 1.0, x) }
        "#;
        let m = parse(src);
        analyze(&m).expect("smoothstep(e0, e1, x) must type-check");
    }

    /// `noise(x, z)` is listed in and provided by the runtime
    /// as `spass_noise_2d`. Regression test: it was missing from
    /// `register_builtins`, so calls failed with E005.
    #[test]
    fn noise_is_registered() {
        let src = r#"
            func n(x: float, z: float): float { return noise(x, z) }
        "#;
        let m = parse(src);
        analyze(&m).expect("noise(x, z) must type-check");
    }

    /// `fbm(x, z, octaves, gain)` is quaternary and maps to
    /// `spass_fbm`. Regression test: missing from `register_builtins`.
    #[test]
    fn fbm_is_registered() {
        let src = r#"
            func f2(x: float, z: float): float { return fbm(x, z, 4, 0.5) }
        "#;
        let m = parse(src);
        analyze(&m).expect("fbm(x, z, octaves, gain) must type-check");
    }

    /// `analyze_full` returns the typed module plus the warnings
    /// collected along the way. A program that has no warnings
    /// returns an empty vec.
    #[test]
    fn analyze_full_collects_no_warnings_for_clean_module() {
        let src = r#"
            func add(a: float, b: float): float { return a + b }
        "#;
        let m = parse(src);
        let result = analyze_full(&m).expect("analyse");
        assert!(result.warnings.is_empty(), "got {:?}", result.warnings);
    }

    /// A `@dispatch` with a bind entry that no matched function
    /// takes triggers a W002 warning. The dispatch is still
    /// semantically valid (analyse returns Ok) and the warning
    /// appears in the `warnings` vec.
    #[test]
    fn analyze_full_collects_w002_for_unused_dispatch_bind() {
        let src = r#"
            @tag(noise) func noisy(pos: float, h: float): float { return h }
            @tag(smooth) func smooth(pos: float, h: float): float { return h }
            func driver(pos: float, h: float): float {
                @dispatch(tags = {noise, smooth}, bind = {factor: 0.5})
                h = apply(pos, h)
                return h
            }
        "#;
        let m = parse(src);
        let result = analyze_full(&m).expect("analyse should succeed despite unused bind");
        // Both `noisy` and `smooth` don't take a `factor` param,
        // so we expect two W002 warnings.
        let w002: Vec<_> = result
            .warnings
            .iter()
            .filter(|w| matches!(w, SemanticWarning::DispatchBindUnused { .. }))
            .collect();
        assert_eq!(w002.len(), 2, "got {:?} warnings", result.warnings);
    }

    // ====================================================================
    // module system (fileclude visibility / ownclude capture)
    // ====================================================================

    use std::path::{Path, PathBuf};

    /// In-memory file loader mirroring the parser tests' helper. Test
    /// files never exist on disk, so `canonicalize` falls back to the
    /// literal path and single-segment names match exactly.
    struct TestLoader {
        files: HashMap<String, String>,
    }

    impl TestLoader {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                files: entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }
    }

    impl spass_parser::FileLoader for TestLoader {
        fn load(&self, path: &Path) -> Result<String, String> {
            self.files
                .get(&path.display().to_string())
                .cloned()
                .ok_or_else(|| format!("no such file: {}", path.display()))
        }
    }

    fn parse_project(files: &[(&str, &str)], tops: &[&str]) -> Module {
        let loader = TestLoader::new(files);
        let top_paths: Vec<PathBuf> = tops.iter().map(PathBuf::from).collect();
        spass_parser::Parser::parse_project_with_includes(&top_paths, &loader)
            .unwrap_or_else(|e| panic!("project parse failed: {}", e))
    }

    /// `ownclude` captures only the names its body actually reads -
    /// not every binding in scope, and not the enclosing function's
    /// parameters it never touches. The hoisted helper's ABI (and its
    /// mangled name) depends on this list staying minimal.
    #[test]
    fn ownclude_capture_set_is_minimal() {
        let src = r#"
            func add(a: float, b: float): float { return a + b }
            func compute_b(x: float): float {
                let base = x + 5.0
                ownclude func scale(t: float): float {
                    return t * base + 1.0
                }
                let a = scale(2.0)
                let b = scale(3.0)
                return add(a, b)
            }
        "#;
        let m = parse(src);
        let typed = analyze(&m).expect("analyse");
        let scale = typed
            .func("scale")
            .expect("ownclude hoisted into typed module");
        assert_eq!(scale.enclosing_func.as_deref(), Some("compute_b"));
        let cap_names: Vec<&str> = scale.captures.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(cap_names, vec!["base"], "captures must be exactly the read names");
        assert!(matches!(scale.captures[0].1, Type::Float(None)));
    }

    /// Capture happens at definition time: a name bound *after* the ownclude statement
    /// is not visible at definition time, so reading it from the
    /// closure body is an E005.
    #[test]
    fn ownclude_cannot_read_later_binding() {
        let src = r#"
            func f(x: float): float {
                ownclude func g(t: float): float {
                    return t * later
                }
                let later = x + 1.0
                return g(x)
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::UndefinedIdent { ref name, .. } if name == "later"),
            "expected UndefinedIdent for `later`, got {:?}",
            err
        );
    }

    /// Capture-by-value forbids writes to captured names (E006 family).
    #[test]
    fn ownclude_capture_write_is_rejected() {
        let src = r#"
            func f(x: float): float {
                let base = x + 1.0
                ownclude func g(t: float): float {
                    base = t
                    return t * base
                }
                return g(x)
            }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(matches!(err, SemanticError::TypeMismatch { .. }), "got {:?}", err);
    }

    /// The closure is scoped to its enclosing function: a sibling
    /// function cannot even see the name - from `h`'s scope the
    /// ownclude is simply not registered, so the call is E005.
    #[test]
    fn ownclude_out_of_scope_call_is_rejected() {
        let src = r#"
            func f(x: float): float {
                ownclude func g(t: float): float { return t }
                return g(x)
            }
            func h(x: float): float { return g(x) }
        "#;
        let m = parse(src);
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::UndefinedIdent { ref name, .. } if name == "g"),
            "expected UndefinedIdent for `g`, got {:?}",
            err
        );
    }

    /// A `fileclude` function is callable from its own file but not
    /// from a function defined in another file (E006 cross-file).
    #[test]
    fn fileclude_cross_file_call_is_rejected() {
        let files = [
            (
                "a.spass",
                "fileclude func half(x: float): float { return x * 0.5 }\n\
                 func compute_a(x: float): float { let h = half(x) return add(h, h) }\n\
                 func add(a: float, b: float): float { return a + b }",
            ),
            (
                "b.spass",
                "func compute_b(x: float): float { return half(x) }",
            ),
        ];
        let m = parse_project(&files, &["a.spass", "b.spass"]);
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::FilePrivateCrossFile { .. }),
            "expected FilePrivateCrossFile, got {:?}",
            err
        );
    }

    /// The positive case: a `fileclude` call from the same file (even
    /// after include-merging into a third top-level file) is fine.
    #[test]
    fn fileclude_same_file_call_is_allowed() {
        let files = [
            (
                "a.spass",
                "fileclude func half(x: float): float { return x * 0.5 }\n\
                 func compute_a(x: float): float { let h = half(x) return add(h, h) }\n\
                 func add(a: float, b: float): float { return a + b }",
            ),
            (
                "main.spass",
                "include \"a.spass\"\nfunc main_f(x: float): float { return compute_a(x) }",
            ),
        ];
        let m = parse_project(&files, &["main.spass"]);
        analyze(&m).unwrap_or_else(|e| panic!("same-file fileclude call must pass: {:?}", e));
    }

    /// `list.length` and indexing are typed; a list walk
    /// binds the element type.
    #[test]
    fn list_length_and_index_are_typed() {
        let m = parse(
            "func f(): int { let xs = [10, 20] let n = xs.length for x in xs { let y = x } return xs[1] + n }",
        );
        analyze(&m).expect("list access must type-check");
    }

    /// a list may not escape a function. A list return
    /// would point into the callee's frame in C and is not FFI-safe in
    /// Rust.
    #[test]
    fn list_return_is_rejected() {
        let m = parse("func f(): list<float> { return [1.0] }");
        let err = analyze(&m).unwrap_err();
        assert!(
            matches!(err, SemanticError::TypeMismatch { .. }),
            "expected TypeMismatch, got {:?}",
            err
        );
        assert!(format!("{}", err).contains("cannot return a list"));
    }

    /// A public function is part of the numeric module call ABI, which
    /// has no list shape. A private helper may take one.
    #[test]
    fn public_list_parameter_is_rejected_but_private_one_is_allowed() {
        let public = parse("func f(xs: list<float>): int { return xs.length }");
        let err = analyze(&public).unwrap_err();
        assert!(format!("{}", err).contains("cannot take a list parameter"));

        let private = parse(
            "fileclude func total(xs: list<float>): float { \
                 let t: float = 0.0 for x in xs { t = t + x } return t } \
             func f(): float { return total([1.0, 2.0]) }",
        );
        analyze(&private).expect("a private helper may take a list");
    }

    /// `parallel` maps a list argument element-wise and
    /// awaiting it yields the list of results.
    #[test]
    fn parallel_over_list_arguments_yields_a_list() {
        let m = parse(
            "func pair(a: float, b: float): float { return a + b } \
             func f(): float { \
                 let xs: list<float> = [1.0, 2.0] \
                 let t = parallel pair(xs, xs) \
                 let rs: list<float> = await(t) \
                 return rs[0] }",
        );
        analyze(&m).expect("parallel over lists must type-check");
    }

    /// The playground builtins this layer registers must be the ones the
    /// runtime exports, with the same parameter count.
    ///
    /// A builtin the runtime exports but this layer does not register is
    /// a call that does not type-check, and a parameter count that
    /// disagrees with the runtime is a call the generated module cannot
    /// compile - and nothing in the workspace would notice either,
    /// because no test compiles a generated module.
    #[test]
    fn playground_builtins_match_the_runtime_surface() {
        let mut sym = SymbolTable::new();
        register_builtins(&mut sym);
        for (name, arity) in spass_playground::surface::SURFACE {
            let func = sym
                .functions
                .get(*name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            assert_eq!(
                func.params.len(),
            *arity,
                "{name} is registered with the wrong number of parameters"
            );
        }
        // And nothing is registered that the runtime does not export.
        for name in BUILTIN_NAMES
            .iter()
            .filter(|n| n.starts_with("s2d:") || n.starts_with("s3d:") || n.starts_with("spio:"))
        {
            assert!(
                spass_playground::surface::SURFACE.iter().any(|(n, _)| n == name),
                "{name} is registered but is not part of the playground surface"
            );
        }
    }
}

