//! Defines the Spass AST nodes shared by every compiler stage.

use num_rational::Rational64;
use spass_lexer::Span;
use std::fmt;

// Re-export Span so downstream code can write `spass_parser::Span` if it
// prefers, without depending on `spass-lexer` directly.
pub use spass_lexer::Span as AstSpan;

pub mod token {
    //! Re-export the lexer token types for convenience.
    pub use spass_lexer::{Token, TokenKind};
}

// ============================================================================
// Range (value-domain declaration, see -3.5)
// ============================================================================

/// A rational literal bound used in value-domain declarations.
///
/// Spass requires `step` to be a negative power of two or ten (E001),
/// but `min` and `max` can be any rational.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RationalLiteral {
    pub value: Rational64,
    pub span: Span,
}

/// A value-domain declaration `T[min, max, step]`. `None` for any field
/// means "use the default for the underlying type".
///
/// Example: `float[0.0, 380.0, 0.1]` => `min=Some(0)`, `max=Some(380/10)`,
/// `step=Some(1/10)`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RangeDecl {
    pub min: Option<RationalLiteral>,
    pub max: Option<RationalLiteral>,
    pub step: Option<RationalLiteral>,
    pub span: Span,
}

impl RangeDecl {
    pub fn is_default(&self) -> bool {
        self.min.is_none() && self.max.is_none() && self.step.is_none()
    }
}

// ============================================================================
// Types (see )
// ============================================================================

/// A fully resolved type reference after parsing.
///
/// For MVP we only support the scalar types. Vectors/matrices/lists/structs
/// are added in later iterations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// IEEE 754 binary32 float, optionally with a value-domain declaration.
    Float(Option<RangeDecl>),
    /// 32-bit signed integer, optionally with a value-domain declaration.
    Int(Option<RangeDecl>),
    /// 32-bit unsigned integer, optionally with a value-domain declaration.
    Uint(Option<RangeDecl>),
    /// Boolean (true/false). No value-domain.
    Bool,
    /// UTF-8 string. Metadata only, no value-domain.
    String,
    /// Parameterised list `list<T>`. No value-domain.
    List(Box<Type>),
    /// Column vector with a value-domain per component.
    Vec2(Option<RangeDecl>, Option<RangeDecl>),
    Vec3(Option<RangeDecl>, Option<RangeDecl>, Option<RangeDecl>),
    Vec4(Option<RangeDecl>, Option<RangeDecl>, Option<RangeDecl>, Option<RangeDecl>),
    /// 3x3 column-major matrix. No value-domain.
    Mat3,
    /// 4x4 column-major matrix. No value-domain.
    Mat4,
    /// Reference to a user-defined struct type.
    Struct(String, Span),
    /// Async task handle. Produced by `spawn`/`parallel`,
    /// consumed by `await(task)`. The payload is the value `await`
    /// produces: a `spawn` carries the callee's return type, a
    /// `parallel` over list arguments carries `list<return type>`.
    Task(Option<Box<Type>>),
}

impl Type {
    /// Returns the default range for this type (or `None` if not applicable).
    ///
    /// The defaults come from `float32`, `int32`, `uint32`.
    pub fn default_range(&self) -> Option<RangeDecl> {
        match self {
            Type::Float(_) | Type::Int(_) | Type::Uint(_) => Some(RangeDecl::default()),
            _ => None,
        }
    }

    /// True if this type carries a numeric value-domain (and thus needs
    /// compile-time range inference).
    pub fn is_numeric_with_range(&self) -> bool {
        matches!(self, Type::Float(_) | Type::Int(_) | Type::Uint(_))
    }

    /// Pretty-printed name used in error messages.
    pub fn display_name(&self) -> String {
        match self {
            Type::Float(r) => type_with_range("float", r),
            Type::Int(r) => type_with_range("int", r),
            Type::Uint(r) => type_with_range("uint", r),
            Type::Bool => "bool".to_string(),
            Type::String => "string".to_string(),
            Type::List(inner) => format!("list<{}>", inner.display_name()),
            Type::Vec2(_, _) => "vec2".to_string(),
            Type::Vec3(_, _, _) => "vec3".to_string(),
            Type::Vec4(_, _, _, _) => "vec4".to_string(),
            Type::Mat3 => "mat3".to_string(),
            Type::Mat4 => "mat4".to_string(),
            Type::Struct(name, _) => name.clone(),
            Type::Task(inner) => match inner {
                Some(t) => format!("task<{}>", t.display_name()),
                None => "task".to_string(),
            },
        }
    }
}

fn type_with_range(base: &str, r: &Option<RangeDecl>) -> String {
    match r {
        Some(rd) if !rd.is_default() => {
            let min = rd.min.as_ref().map(|m| m.value.to_string()).unwrap_or_else(|| "?".into());
            let max = rd.max.as_ref().map(|m| m.value.to_string()).unwrap_or_else(|| "?".into());
            let step = rd.step.as_ref().map(|m| m.value.to_string()).unwrap_or_else(|| "?".into());
            format!("{}[{}, {}, {}]", base, min, max, step)
        }
        _ => base.to_string(),
    }
}

// ============================================================================
// Annotations
// ============================================================================

/// A function- or statement-level annotation.
///
/// We keep this as a flat enum so the parser is straightforward; semantic
/// analysis later resolves which annotations are valid in which contexts.
#[derive(Debug, Clone, PartialEq)]
pub enum Annotation {
    /// `@gpu(workgroup=N, local="48KB")` on a function.
    Gpu { workgroup: Option<u32>, local: Option<String>, span: Span },
    /// `@tag(A, B, key=value)` on a function.
    Tag { items: Vec<TagItem>, span: Span },
    /// `@checkpoint name` inside a function body.
    Checkpoint { name: String, span: Span },
    /// `@resume name` inside a function body.
    Resume { name: String, span: Span },
    /// `@absent` block inside an `if_present` body.
    Absent { span: Span },
    /// `@before(F [, require])` ordering declaration.
    Before { target: String, require: bool, span: Span },
    /// `@after(F [, require])` ordering declaration.
    After { target: String, require: bool, span: Span },
    /// `@tracked` on a `spawn`/`parallel` expression.
    Tracked { span: Span },
    /// `@independent` on a `parallel` expression.
    Independent { span: Span },
}

/// One item inside a `@tag(...)` annotation.
#[derive(Debug, Clone, PartialEq)]
pub enum TagItem {
    /// Bare tag (no value), e.g. `erosion` in `@tag(erosion, thermal)`.
    Bare(String),
    /// Valued tag, e.g. `strength_factor=0.7` in `@tag(strength_factor=0.7)`.
    Valued(String, TagValue, Span),
}

/// Value of a `@tag` key=value item. Only numeric/string/bool literals are
/// allowed per.
#[derive(Debug, Clone, PartialEq)]
pub enum TagValue {
    Number(Rational64),
    String(String),
    Bool(bool),
}

// ============================================================================
// Top-level items
// ============================================================================

/// A compilation unit. A `Module` is the result of expanding all `include`
/// directives in a single source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
    pub source_file: String,
}

/// A top-level declaration in a module.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A function declaration. May carry annotations (e.g. `@gpu`, `@tag`).
    Func(FuncDef),
    /// A struct type definition. No annotations.
    Struct(StructDef),
    /// `include "file.spass"` -- already expanded by the parser stage.
    /// Kept for debugging only.
    Include { path: String, span: Span },
    /// `test "name" {... }` or `test "name" with {... } {... }`
    /// Tests are top-level declarations that are
    /// collected by `spass test` and executed. The body is a regular
    /// block of statements; the optional `params` come from a `with`
    /// block, each with a default value expression.
    Test(TestDef),
    /// `sequence name {... }`. A top-level execution
    /// stream: every sequence is instantiated once at program start and
    /// runs in parallel, with its own state block and message queue.
    Sequence(SequenceDef),
}

/// A sequence declaration.
///
/// Sequences communicate through `send` / `receive` / `broadcast`. The
/// body is a regular statement block; the sequence itself takes no
/// parameters (its entry point is the first statement).
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceDef {
    pub name: String,
    pub body: Block,
    pub span: Span,
}

/// The filter part of `receive Alert{urgency > 5}`. A
/// compiled sequence blocks until a message of `type_name` arrives whose
/// fields satisfy every condition. Conditions referencing an unknown
/// field are a semantic error.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivePattern {
    pub type_name: String,
    /// (field, operator, right-hand side) triples, e.g. `urgency > 5`.
    pub conditions: Vec<(String, BinOp, Expr)>,
    pub span: Span,
}

/// A test declaration.
///
/// The simplest form is `test "name" { body }`. Parameterised form is
/// `test "name" with { p1: T = default,... } { body }`. The `params`
/// list is empty for the non-parameterised form.
#[derive(Debug, Clone, PartialEq)]
pub struct TestDef {
    pub name: String,
    /// Parameter list from the `with` block. Each entry is a (name, type,
    /// default-expression) triple. Empty for the bare form.
    pub params: Vec<TestParam>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestParam {
    pub name: String,
    pub ty: Type,
    /// Default value expression (the `= expr` in the `with` block).
    pub default: Expr,
    pub span: Span,
}

/// A function parameter. `out` parameters write to caller-provided storage
/// and functions with `out` parameters cannot have a return type.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
    pub is_out: bool,
    pub span: Span,
}

/// A function definition.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<Param>,
    /// Return type. `None` if the function has only `out` parameters.
    pub return_type: Option<Type>,
    /// Multi-return tuple types. `Some(non_empty)` implies
    /// `return_type` is `None` and the function uses the tuple form.
    pub multi_returns: Option<Vec<Type>>,
    pub body: Block,
    pub annotations: Vec<Annotation>,
    pub visibility: Visibility,
    /// the file the function was originally defined in.
    /// Used by the semantic pass to enforce `fileclude` visibility
    /// across the project: a call from a function in file `A` to a
    /// `fileclude` function in file `B` is an E006 error. The
    /// parser sets this when the FuncDef is constructed; the value
    /// is preserved through the include-expansion step.
    pub source_file: String,
    /// for `ownclude` (own-private) functions, the name
    /// of the public function whose body contains the `ownclude`
    /// definition. `None` for `func` / `fileclude` functions (the
    /// top-level case). Used by the semantic pass to enforce that
    /// an `ownclude` is only callable from the body of its enclosing
    /// public function.
    pub enclosing_func: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    /// Default: globally visible.
    Public,
    /// `fileclude func...` - visible only within the file.
    FilePrivate,
    /// `ownclude func...` - visible only within the enclosing
    /// function body.
    OwnPrivate,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<StructField>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

// ============================================================================
// Statements
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let name = expr;` - single binding.
    /// `let (a, b) = expr;` - tuple destructuring, used for multi-return
    /// call sites. `names.len > 1` means a destructure.
    /// `ty` is only valid when `names.len == 1`; for destructuring the
    /// element types come from the right-hand side's compile-time type
    /// (the called function's `multi_returns`).
    Let { names: Vec<String>, ty: Option<Type>, value: Expr, span: Span },
    /// `name = expr;` - assignment (only to mutable variables, e.g. inside
    /// `@checkpoint` loops where the variable is automatically promoted).
    Assign { name: String, value: Expr, span: Span },
    /// `return expr;`.
    Return { value: Option<Expr>, span: Span },
    /// An expression used as a statement (typically a function call).
    Expr(Expr, Span),
    /// `if cond {... } else {... }`. `else` branch is
    /// always present in well-formed Spass.
    If { cond: Expr, then_branch: Block, else_branch: Block, span: Span },
    /// `for i in 0..10 {... }` (numeric), `for i in 0..10 step 5 {... }`
    /// (with step), or `for item in list {... }` (iteration).
    For { var: String, iter: ForIter, body: Block, span: Span },
    /// `while cond {... }`.
    While { cond: Expr, body: Block, span: Span },
    /// `break;`
    Break { span: Span },
    /// `continue;`
    Continue { span: Span },
    /// `@checkpoint name`.
    Checkpoint { name: String, span: Span },
    /// `@resume name`. May be wrapped in `if`.
    Resume { name: String, span: Span },
    /// `@before(F [, require])` or `@after(F [, require])`.
    OrderDecl { kind: OrderKind, target: String, require: bool, span: Span },
    /// `@dispatch(...) apply(...)` block.
    Dispatch { decl: DispatchDecl, apply: ApplyStmt, span: Span },
    /// `assert(...)` statement. Either a range check
    /// (`assert(ident in [lo, hi])` with bracket variants) or an
    /// approximation check (`assert(approx(value, target, tolerance))`).
    Assert { kind: AssertKind, span: Span },
    /// `if_present(name) {... }` optionally followed by `@absent {... }`
    /// The `param` must be the name of a parameter in
    /// the enclosing function's signature. Codegen compiles the
    /// enclosing function into two variants; the dispatcher picks
    /// the variant that matches whether the caller supplied `param`.
    /// The `absent_branch` is `None` when the function has no
    /// `@absent` block (the absent path falls through to whatever
    /// follows the `if_present`).
    IfPresent {
        param: String,
        present_branch: Block,
        absent_branch: Option<Block>,
        span: Span,
    },
    /// `@await`. The marker takes a whole line on its
    /// own; the next statement must be `let v = spasio:input[mode]("name")`
    /// (the binding name is `v`, the channel name comes from the
    /// `spasio:input` call). The semantic pass enforces the
    /// next-stmt shape. Codegen reuses the state machine and
    /// adds an `Await` slot to the state block so the function
    /// suspends, returns a `task` handle, and resumes when the
    /// runtime delivers data on the channel.
    Await { span: Span },
    /// `ownclude func name(...)... {... }` - a
    /// function-local nested function. The inner `FuncDef` carries
    /// `visibility = OwnPrivate` and `enclosing_func` set to the
    /// name of the public function whose body this statement
    /// appears in. The semantic pass registers the inner function
    /// in the symbol table for the duration of the enclosing
    /// function's body check so the call site can resolve it; the
    /// codegen pass lifts it into a static helper that receives
    /// the captured-by-value closure variables as extra parameters.
    Ownclude(Box<FuncDef>),
}

/// The shape of an `assert` statement.
#[derive(Debug, Clone, PartialEq)]
pub enum AssertKind {
    /// `assert(ident in RANGE)` where RANGE picks one of the four
    /// bracket combinations in the language: closed `[a, b]`, open
    /// `{a, b}`, half-open left `[a, b}`, half-open right `{a, b]`.
    InRange { value: String, range: RangeBound, span: Span },
    /// `assert(approx(value, target, tolerance))`. Used to verify
    /// value-domain-precision results where tiny floating-point drift
    /// is acceptable.
    Approx { value: String, target: Expr, tolerance: Expr, span: Span },
}

/// A half-open or closed interval used in `assert(... in...)`. The
/// `lo` / `hi` are the same expression type the parser already has
/// (`Expr`), so an interval can refer to a literal, a constant
/// identifier, or any other compile-time-known value.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeBound {
    pub lo: Expr,
    pub hi: Expr,
    /// `true` = endpoint is included (closed `[]`).
    pub lo_inclusive: bool,
    pub hi_inclusive: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForIter {
    /// `start..end` numeric range (half-open, ).
    Range { start: Expr, end: Expr, step: Option<Expr> },
    /// `item in expr` where `expr` evaluates to a `list<T>`.
    List(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderKind {
    Before,
    After,
}

/// The body of a `@dispatch` statement.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchDecl {
    pub tags: TagSelector,
    pub bind: Vec<DispatchBind>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyStmt {
    /// Optional assignment target (lhs), e.g. `h` in `h = apply(pos, h)`.
    /// When `None`, the apply call result is discarded (void form).
    pub target: Option<String>,
    /// The call expression itself, e.g. `apply(pos, h)`. The first arg
    /// is the "static" first parameter; the second arg is the "carry"
    /// that is threaded through the left-fold expansion.
    pub call: Expr,
    pub span: Span,
}

/// Filter mode for `@dispatch(tags =...)`.
#[derive(Debug, Clone, PartialEq)]
pub enum TagSelector {
    /// `{A, B, key=value}` - OR over bare tags; AND with valued matches.
    Or(Vec<TagItem>),
    /// `[A, B]` - bare-tag AND.
    And(Vec<TagItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DispatchBind {
    pub param: String,
    pub source: DispatchBindSource,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DispatchBindSource {
    Literal(Expr),
    TagRef(String, Span),
}

impl DispatchBindSource {
    pub fn span(&self) -> Span {
        match self {
            DispatchBindSource::Literal(e) => e.span(),
            DispatchBindSource::TagRef(_, s) => s.clone(),
        }
    }
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    IntLit { value: i64, span: Span },
    FloatLit { value: Rational64, span: Span },
    BoolLit { value: bool, span: Span },
    StringLit { value: String, span: Span },
    Ident { name: String, span: Span },
    /// Binary operation. Op precedence is encoded by the parser tree shape,
    /// not stored here.
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Unary { op: UnaryOp, expr: Box<Expr>, span: Span },
    /// Function call. `args` may be empty.
    Call { callee: Box<Expr>, args: Vec<CallArg>, span: Span },
    /// `target.field` access.
    FieldAccess { target: Box<Expr>, field: String, span: Span },
    /// `target[index]` index access.
    Index { target: Box<Expr>, index: Box<Expr>, span: Span },
    /// `(a, b, c)` tuple literal. Used as the return value
    /// of a multi-return function. The element count must match the
    /// function's declared `multi_returns` arity.
    Tuple { elements: Vec<Expr>, span: Span },
    /// `[a, b, c]` list literal.
    ListLit { elements: Vec<Expr>, span: Span },
    /// `TypeName{ field: value,... }` struct literal.
    StructLit { name: String, fields: Vec<(String, Expr)>, span: Span },
    /// `if cond { a } else { b }` expression form.
    IfExpr { cond: Box<Expr>, then_branch: Box<Expr>, else_branch: Box<Expr>, span: Span },
    /// `spawn func(args)`.
    Spawn { func: Box<Expr>, args: Vec<CallArg>, annotations: Vec<Annotation>, span: Span },
    /// `parallel func(args1, args2)`.
    Parallel { func: Box<Expr>, args: Vec<CallArg>, annotations: Vec<Annotation>, span: Span },
    /// `await(expr)`.
    Await { task: Box<Expr>, span: Span },
    /// `random(a, b, c)` and `random(f, g)(args)`.
    ///
    /// The value form has `args == None` and picks one candidate;
    /// the function form has `args == Some(..)` and calls the picked
    /// candidate with them.
    Random {
        candidates: Vec<Expr>,
        args: Option<Vec<CallArg>>,
        span: Span,
    },
    /// `as_type(value, new_min, new_max, new_step)`.
    AsType { value: Box<Expr>, target: Type, span: Span },
    /// `narrow_cast<T>(value)`.
    NarrowCast { value: Box<Expr>, target: Type, span: Span },
    /// `to_int(x)` / `to_float(x)`.
    Conv { kind: ConvKind, value: Box<Expr>, span: Span },
    /// `if_present(name) {... }` block - checked at compile time.
    IfPresent {
        param: String,
        then_branch: Block,
        absent_branch: Option<Block>,
        span: Span,
    },
    /// `spasio:input[mode]("name")` -.
    SpasioInput { mode: SpasioMode, channel: String, span: Span },
    /// `spasio:output[mode](value, "name")` -.
    SpasioOutput { mode: SpasioMode, value: Box<Expr>, channel: String, span: Span },
    /// `s2d:window(...)`, `s3d:box(...)`, `spio:read_text(...)` - playground
    /// namespaces. `<ns>:<func>(args...)`.
    NativeCall {
        namespace: String,
        func: String,
        args: Vec<CallArg>,
        span: Span,
    },
    /// `send(target, message)`. `target` is a sequence
    /// name; `message` is a struct literal.
    Send { target: String, message: Box<Expr>, span: Span },
    /// `broadcast(message)` - delivered to every live
    /// sequence, including the sender.
    Broadcast { message: Box<Expr>, span: Span },
    /// `receive [Pattern{conds}]`. Blocks until a message
    /// arrives; with a pattern, messages that don't satisfy the
    /// conditions are discarded.
    Receive { pattern: Option<ReceivePattern>, span: Span },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpasioMode {
    File,
    Socket,
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvKind {
    ToInt,
    ToFloat,
}

/// A call argument. Most calls use `Plain`, but a few built-ins accept
/// range-typed or pattern arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Pow,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
}

impl BinOp {
    pub fn precedence(self) -> u8 {
        // Mirrors ** > *,/ > +,- > compare > and > or.
        match self {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::Add | BinOp::Sub => 4,
            BinOp::Mul | BinOp::Div => 5,
            BinOp::Pow => 7,
        }
    }
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Pow => "**",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

// ============================================================================
// Trait impls: Span accessors
// ============================================================================

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::IntLit { span, .. }
            | Expr::FloatLit { span, .. }
            | Expr::BoolLit { span, .. }
            | Expr::StringLit { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Call { span, .. }
            | Expr::FieldAccess { span, .. }
            | Expr::Index { span, .. }
            | Expr::Tuple { span, .. }
            | Expr::ListLit { span, .. }
            | Expr::StructLit { span, .. }
            | Expr::IfExpr { span, .. }
            | Expr::Spawn { span, .. }
            | Expr::Parallel { span, .. }
            | Expr::Await { span, .. }
            | Expr::Random { span, .. }
            | Expr::AsType { span, .. }
            | Expr::NarrowCast { span, .. }
            | Expr::Conv { span, .. }
            | Expr::IfPresent { span, .. }
            | Expr::SpasioInput { span, .. }
            | Expr::SpasioOutput { span, .. }
            | Expr::NativeCall { span, .. }
            | Expr::Send { span, .. }
            | Expr::Broadcast { span, .. }
            | Expr::Receive { span, .. } => span.clone(),
        }
    }
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr(_, span)
            | Stmt::If { span, .. }
            | Stmt::For { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Checkpoint { span, .. }
            | Stmt::Resume { span, .. }
            | Stmt::OrderDecl { span, .. }
            | Stmt::Dispatch { span, .. }
            | Stmt::Assert { span, .. }
            | Stmt::IfPresent { span, .. }
            | Stmt::Await { span } => span.clone(),
            Stmt::Ownclude(f) => f.span.clone(),
        }
    }
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum AstError {
    #[error("parse error: {message}")]
    Parse { message: String, span: Span },
}
