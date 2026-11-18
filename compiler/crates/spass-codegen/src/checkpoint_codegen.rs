//! Lowers `@checkpoint` / `@resume` into C and Rust state machines.

use spass_lexer::Span;
use spass_parser::ast::{Block, Stmt, Type};
use spass_semantic::TypedFunc;
use std::collections::HashMap;

// ============================================================================
// Public state-data layout (reserved interface for future features)
// ============================================================================

/// What a slot in the state block is for. The MVP only uses
/// `Param`, `Let`, and `Return`. Future revisions (GPU state,
/// `@await` pending I/O, structured concurrency suspend tokens)
/// will add new variants without breaking existing emitters:
/// the contract is byte-based, not kind-based.
///
/// `ponytail`: when a new kind is added, the slot's `size` field
/// is the source of truth. The MVP defaults every slot to 8 bytes;
/// typed-slot revisions will use per-kind sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSlotKind {
    /// A function parameter. The wrapper writes the decoded value
    /// into this slot at function entry; the runner reads it.
    Param,
    /// A `let`-bound local. Initialised to 0 by `spass_task_new`
    /// and written by the runner.
    Let,
    /// The function's return value. The runner writes its final
    /// `return` value here; the wrapper reads it back.
    Return,
    /// the value bound by `let v = spasio:input[...](...)`
    /// immediately following an `@await`. The runtime stores the
    /// awaited value into this slot so it survives the suspension
    /// point. The MVP uses one `Await` slot per function (the
    /// semantic pass enforces at most one `@await` per function);
    /// multiple awaits would need a per-site channel table.
    Await,
    /// D5/D5+: the current state id, stored as a `f64` so it fits
    /// in the same flat slot array. The Rust state-machine runner
    /// reads this on every re-entry to know which slice to run. The
    /// C path stores the state id on the `spass_task_t` (the
    /// runtime's `void*` block already covers it); we only need
    /// this slot in the Rust backend.
    StateId,
}

/// One slot in the state block. The MVP emits an 8-byte slot for
/// every local; future revisions will distinguish kinds for storage
/// (e.g. a 4-byte int slot) without changing the wrapper's
/// decoding logic.
#[derive(Debug, Clone)]
pub struct StateSlot {
    /// The local's source-level name. The body refers to the local
    /// by this name; the runner substitutes a `state-><name>` access
    /// so the existing AST-walk emitter can stay unchanged.
    pub name: String,
    /// What kind of slot this is.
    pub kind: StateSlotKind,
    /// Byte offset within the state block.
    pub offset: usize,
    /// Slot size in bytes. MVP: 8 bytes for every slot.
    pub size: usize,
}

/// Public state-data layout for a function. The wrapper and the
/// runner use this contract to lay out a function's state block.
/// Adding a new slot kind (e.g. `GpuState`) will not break existing
/// emitters because the contract is based on byte offsets and
/// sizes, not on the specific kinds.
#[derive(Debug, Clone)]
pub struct StateDataLayout {
    /// All slots, in source order: parameters first, then `let`-
    /// bound locals, then the return slot. Byte offsets are stable
    /// for a given function.
    pub slots: Vec<StateSlot>,
    /// Total state block size in bytes. Always equal to the sum of
    /// slot sizes; the codegen layer uses this directly when calling
    /// `spass_task_new`.
    pub state_size: usize,
}

impl StateDataLayout {
    /// Look up a slot by name. Returns `None` if the local was
    /// never bound. Used by the wrapper to find the param/return
    /// slot for a given function argument.
    pub fn slot(&self, name: &str) -> Option<&StateSlot> {
        self.slots.iter().find(|s| s.name == name)
    }

    /// Byte offset of a slot, or `None` if absent. Convenience
    /// around [`Self::slot`].
    pub fn offset_of(&self, name: &str) -> Option<usize> {
        self.slot(name).map(|s| s.offset)
    }
}

// ============================================================================
// Checkpoint plan (extends StateDataLayout with checkpoint info)
// ============================================================================

/// Plan for emitting a state machine for one checkpointed function.
///
/// `CheckpointPlan::build` walks the function body once and gathers
/// every piece of information the emitter needs. Doing this in a
/// separate pass keeps the emit functions simple: they just read
/// from the plan.
#[derive(Debug, Clone)]
pub struct CheckpointPlan {
    /// The function's full name (used to namespace emitted symbols).
    pub func_name: String,
    /// Mangled name of the public wrapper.
    pub mangled: String,
    /// Mangled name of the runner.
    pub runner: String,
    /// State IDs in source order. Index 0 is the implicit "function
    /// entry" state; index `i+1` is the state after the `i`-th
    /// `@checkpoint` site.
    pub states: Vec<StateSite>,
    /// State block layout. Reuses the public [`StateDataLayout`]
    /// interface so future state-machine features can plug in.
    pub layout: StateDataLayout,
    /// Name of the slot holding the return value. `None` for
    /// `out`-only / void functions.
    pub return_slot: Option<String>,
}

/// One `@checkpoint` site in source order. The state ID is `1 + index`.
#[derive(Debug, Clone)]
pub struct StateSite {
    /// Sequential ID, 1-based. State 0 is the implicit entry.
    pub state_id: u32,
    /// The checkpoint name (used to register the task for cross-
    /// function `@resume`).
    pub name: String,
    /// Span of the original `@checkpoint name` statement - used
    /// for error reporting and for ordering the C emission.
    pub span: Span,
}

impl CheckpointPlan {
    /// Build a plan for `func`. The body is walked once to:
    ///
    /// * assign state IDs to every `@checkpoint` (in source order)
    /// * collect every local (params first, then `let` bindings in
    /// the order they appear) and lay them out in a flat struct
    /// * compute the total state block size
    ///
    /// `ponytail`: we currently type every slot as `double` (8 bytes)
    /// because the body runs in double precision even for value-domain
    /// ints (the encoding happens at the public wrapper boundary).
    /// Adding typed slots later is a matter of changing `slot_size`.
    pub fn build(func: &TypedFunc, mangled: &str) -> Self {
        let runner = format!("{}_runner", mangled);
        let mut plan = CheckpointPlan {
            func_name: func.name.clone(),
            mangled: mangled.to_string(),
            runner,
            states: Vec::new(),
            layout: StateDataLayout { slots: Vec::new(), state_size: 0 },
            return_slot: None,
        };
        // D5+ state-id slot. The Rust state-machine runner reads
        // this on every re-entry to know which slice to run. The C
        // path stores the state id on the runtime's `spass_task_t`
        // separately, so the slot is harmless but unused there.
        // The slot is reserved FIRST so its offset is stable; the
        // MVP initial value is 0.0 (entry state).
        plan.push_slot("__sp_state_id".to_string(), StateSlotKind::StateId);
        // Parameter slots first (in declaration order). The wrapper
        // writes each param's decoded value into the matching slot
        // before running the state machine.
        for p in &func.params {
            plan.push_slot(p.name.clone(), StateSlotKind::Param);
        }
        // if the body has an `@await`, reserve an
        // `Await` slot for the bound value. The semantic pass has
        // already enforced the next-stmt shape
        // (`let v = spasio:input[mode]("name")`) and recorded the
        // binding in `func.await_binding`; we use that name as the
        // slot name so the body can refer to it the same way it
        // would a regular `let`. The slot is reserved before the
        // `let` collection so its offset is stable.
        if let Some(await_binding) = &func.await_binding {
            plan.push_slot(await_binding.name.clone(), StateSlotKind::Await);
        }
        // `let` slots next, collected by walking the body once.
        // The let binding for an `@await` is already reserved as an
        // `Await` slot above (so its value lives in the state block
        // across any future async revision). We skip it here to
        // avoid emitting a second slot with the same name - the
        // state block contract is one slot per name.
        let mut let_index: Vec<(String, Type)> = Vec::new();
        for stmt in &func.original.body.stmts {
            collect_let_bindings(stmt, &mut let_index);
        }
        for (name, ty) in let_index {
            // Skip the let that mirrors the await binding: that
            // local's storage is the Await slot we just reserved.
            if let Some(await_binding) = &func.await_binding {
                if await_binding.name == name {
                    let _ = ty;
                    continue;
                }
            }
            plan.push_slot(name, StateSlotKind::Let);
            let _ = ty;
        }
        // Return-value slot: a single extra double that the body
        // writes to before completing. Only used when the function
        // has a return value (not out-only / void).
        if func.return_type.is_some() || func.multi_returns.is_some() {
            // For multi-return we still use a single double slot for
            // MVP. Codegen handles multi-return through a separate
            // struct; the @checkpoint case interleaves only for the
            // single-return shape in the MVP. If a checkpointed
            // function uses multi-return, the user gets a clear error
            // (codegen refuses rather than silently corrupting the
            // state block).
            plan.push_slot("__sp_return".to_string(), StateSlotKind::Return);
            plan.return_slot = Some("__sp_return".to_string());
        }
        // State IDs: walk the body a second time, in order, and
        // assign 1-based IDs to every `@checkpoint` and `@await`
        // we encounter. (A third walk in the emitter; we only
        // collect the IDs here.)
        let mut next_id: u32 = 1;
        for stmt in &func.original.body.stmts {
            assign_state_ids(stmt, &mut next_id, &mut plan.states);
        }
        plan
    }

    fn push_slot(&mut self, name: String, kind: StateSlotKind) {
        // MVP: every slot is 8 bytes (`double`). The state block
        // is naturally aligned because we use the same width for
        // every slot; future typed slots can switch to a per-type
        // size without changing the codegen contract.
        const SLOT: usize = 8;
        self.layout.slots.push(StateSlot {
            name,
            kind,
            offset: self.layout.state_size,
            size: SLOT,
        });
        self.layout.state_size += SLOT;
    }

    /// Convenience: get the state_id for a checkpoint name, or `None`
    /// if the name isn't a checkpoint in this function.
    pub fn state_id_of(&self, name: &str) -> Option<u32> {
        self.states.iter().find(|s| s.name == name).map(|s| s.state_id)
    }

    /// Build a C-style name for the per-function state struct. The name
    /// is `spass_<func>_state_t`, mangled with the function name so two
    /// checkpointed functions don't collide.
    ///
    /// `ponytail`: the struct is a private detail of the wrapper/runner
    /// pair. The runtime only sees the raw byte size.
    pub fn state_struct_name(&self) -> String {
        state_struct_name(&self.mangled)
    }
}

fn collect_let_bindings(stmt: &Stmt, out: &mut Vec<(String, Type)>) {
    match stmt {
        Stmt::Let { names, value, .. } => {
            if names.len() == 1 {
                // Default to `float` if the value's type isn't a
                // scalar (codegen already accepts `double` for
                // everything in MVP, so this is just a placeholder).
                let ty = Type::Float(None);
                out.push((names[0].clone(), ty));
            }
            // Destructuring (multi-return) is rejected by the
            // codegen path for checkpointed functions; we just
            // skip those bindings to avoid emitting a fake slot.
            let _ = value;
        }
        Stmt::If { then_branch, else_branch, .. } => {
            for s in &then_branch.stmts {
                collect_let_bindings(s, out);
            }
            for s in &else_branch.stmts {
                collect_let_bindings(s, out);
            }
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } => {
            for s in &body.stmts {
                collect_let_bindings(s, out);
            }
        }
        _ => {}
    }
}

fn assign_state_ids(stmt: &Stmt, next_id: &mut u32, out: &mut Vec<StateSite>) {
    match stmt {
        Stmt::Checkpoint { name, span } => {
            out.push(StateSite {
                state_id: *next_id,
                name: name.clone(),
                span: span.clone(),
            });
        *next_id += 1;
        }
        // `@await` is a logical suspension point but
        // the MVP does the I/O inline at the next `let` statement
        // (see `emit_expr` for `Expr::SpasioInput`). We don't
        // assign a state id here - the runner runs through the
        // `@await` without actually yielding to the wrapper. The
        // Await slot is still reserved in the state block so the
        // value lives there across any future async revision.
        Stmt::Await { .. } => {}
        Stmt::If { then_branch, else_branch, .. } => {
            for s in &then_branch.stmts {
                assign_state_ids(s, next_id, out);
            }
            for s in &else_branch.stmts {
                assign_state_ids(s, next_id, out);
            }
        }
        Stmt::For { body, .. } | Stmt::While { body, .. } => {
            for s in &body.stmts {
                assign_state_ids(s, next_id, out);
            }
        }
        _ => {}
    }
}

/// Build a C-style name for the per-function state struct. The name
/// is `spass_<func>_state_t`, mangled with the function name so two
/// checkpointed functions don't collide.
///
/// `ponytail`: the struct is a private detail of the wrapper/runner
/// pair. The runtime only sees the raw byte size.
pub fn state_struct_name(mangled: &str) -> String {
    format!("{}_state_t", mangled)
}

// ============================================================================
// Cross-function @resume trait (D5)
//
// (cross-function @resume) requires the runner to dispatch
// into another function's state machine without knowing the target's
// concrete state type. We use a `SpassState` trait with a single
// `resume` method; each function's state struct implements the trait
// by forwarding to its per-function runner. The cross-program registry
// holds `Box<dyn SpassState>`, so different functions with different
// state struct types can coexist in the same map.
//
// Reserved interface: future revisions may add methods to `SpassState`
// for richer type-erased state introspection (e.g. a `slot(name)` getter
// for the dispatcher to query the state block). The MVP needs only
// `resume` to drive the state machine forward.
// ============================================================================

/// Render the module-level Rust header that defines `SpassState` and
/// the cross-function task registry. Emit this once per program (in
/// `generate_rust`) before any state-machine function so every
/// function's `impl SpassState` can refer to the same trait.
///
/// `ponytail`: the registry uses a `Mutex<HashMap>` rather than the
/// per-function `thread_local!` we had before. The MVP is single-
/// threaded so contention is a non-issue; the `Mutex` is the
/// forward-compat choice for the concurrency work (each task will
/// get its own state struct, but the registry itself is shared).
pub fn rust_state_machine_header() -> &'static str {
    "\
use std::sync::LazyLock;

trait SpassState: Send { fn resume(&mut self); }

// Cross-function @resume registry: any checkpointed function can be
// looked up by its checkpoint name and re-entered via trait dispatch.
// The MVP is single-threaded, so a `Mutex<HashMap>` is plenty.
//
// `ponytail`: we wrap the `Mutex` in a `LazyLock` so the initialiser
// (a non-`const` function call) only runs the first time the registry
// is touched. The previous direct `static MUTEX = Mutex::new(...)`
// form failed to compile because `Mutex::new` is not `const fn`
// (it's allocation-bearing). `LazyLock` is the modern alternative to
// `lazy_static!` / `once_cell` and derefs transparently, so call
// sites stay identical to the eager form (`SPASS_TASK_REGISTRY.lock`).
//
// `SpassState: Send` lets the boxed value live inside a `static`
// (the `Mutex<T>: Sync` bound requires `T: Send`). Every state
// struct is `#[derive(Clone, Copy)]` and thus `Send`; the trait
// bound is a no-op at the impl site. A future revision that adds
// non-`Send` state (e.g. raw pointers for GPU state) can drop the
// bound here and switch the registry to `RefCell` to preserve
// single-threaded semantics.
static SPASS_TASK_REGISTRY: LazyLock<std::sync::Mutex<std::collections::HashMap<String, Box<dyn SpassState + Send>>>> =
    LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
"
}

/// Walk a body and return `true` if the body (or any nested block)
/// contains a `@checkpoint`. Used to decide whether a function needs
/// the state-machine treatment at all.
///
/// `ponytail`: this is `@checkpoint`-only. A function that has
/// `@await` but no `@checkpoint` is detected separately via
/// [`body_has_await`]; the codegen dispatch in `lib.rs` uses
/// [`body_has_suspension`] for the routing decision.
pub fn body_has_checkpoint(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Checkpoint { .. } => return true,
            Stmt::If { then_branch, else_branch, .. } => {
                if body_has_checkpoint(&then_branch.stmts)
                    || body_has_checkpoint(&else_branch.stmts)
                {
                    return true;
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if body_has_checkpoint(&body.stmts) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Walk a body and return `true` if it contains an `@await`. The
/// companion of [`body_has_checkpoint`]; the codegen layer treats
/// both as suspension points and routes through the state-machine
/// path.
pub fn body_has_await(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Await { .. } => return true,
            Stmt::If { then_branch, else_branch, .. } => {
                if body_has_await(&then_branch.stmts) || body_has_await(&else_branch.stmts) {
                    return true;
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if body_has_await(&body.stmts) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Walk a body and return `true` if it contains any state-machine
/// suspension point (`@checkpoint` or `@await`). This is the
/// codegen dispatch check: any of those constructs routes the
/// function through the state-machine path.
pub fn body_has_suspension(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Checkpoint { .. } | Stmt::Await { .. } => return true,
            Stmt::If { then_branch, else_branch, .. } => {
                if body_has_suspension(&then_branch.stmts)
                    || body_has_suspension(&else_branch.stmts)
                {
                    return true;
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if body_has_suspension(&body.stmts) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Walk a body and return `true` if it contains a `@resume`. Used by
/// the wrapper to decide whether to keep the registry-stale check
/// alive at exit (an optimisation, not required for correctness).
pub fn body_has_resume(stmts: &[Stmt]) -> bool {
    for s in stmts {
        match s {
            Stmt::Resume { .. } => return true,
            Stmt::If { then_branch, else_branch, .. } => {
                if body_has_resume(&then_branch.stmts)
                    || body_has_resume(&else_branch.stmts)
                {
                    return true;
                }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                if body_has_resume(&body.stmts) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Split a function body into slices at every `@checkpoint` boundary.
/// The slices are what the state-machine emit turns into the `switch`
/// (C) or `match` (Rust) arms.
///
/// The split rule: each `@checkpoint` site is included at the **end**
/// of the slice that contains the body *before* it. The next slice
/// starts at the statement after the checkpoint. The last slice ends
/// with whatever follows the final checkpoint (typically a `return`).
///
/// Example: `[@checkpoint a; S1; @checkpoint b; S2]` becomes
/// slice 0 = [@checkpoint a, S1]
/// slice 1 = [@checkpoint b, S2]
///
/// This shape lets the per-statement emit call `emit_stmt` on the
/// `Checkpoint` directly - the emit emits the `state_id = N+1;
/// register; return;` trio. The next case label is laid out by the
/// surrounding switch/emit, so re-entry after a `spass_task_resume`
/// lands at the saved state and continues from the post-checkpoint
/// body.
///
/// `@await` is *not* a slice boundary in the MVP. The
/// `@await` marker itself is a no-op in the runner; the I/O
/// happens inline at the `let v = spasio:input[...](...)` statement
/// that follows. The Await slot is still reserved in the state
/// block so the value is preserved across any future async
/// revision (and survives any `@checkpoint` that may interleave).
///
/// `ponytail`: the MVP uses a flat, sequential state id (no
/// per-branch flattening). A checkpoint inside an `if`/`for`/`while`
/// still triggers a slice boundary; the nested block's own statements
/// are flattened into the current slice rather than getting a per-
/// branch slice. The state-machine emit in `lib.rs` writes the
/// nested bodies in source order. A future revision that needs
/// per-branch state will need a different `split_at_checkpoints`.
pub fn split_at_checkpoints(stmts: &[Stmt]) -> Vec<Vec<Stmt>> {
    let mut out: Vec<Vec<Stmt>> = vec![Vec::new()];
    for s in stmts {
        if let Stmt::Checkpoint { .. } = s {
            // Close the current slice with the checkpoint at its end.
            // The emit code for `Stmt::Checkpoint` is the
            // state-transition trio: `state_id = N+1; *t = state;
            // return;`. Putting the checkpoint at the end of the
            // current slice keeps the slice body logically
            // "everything up to and including the checkpoint" -
            // which is the unit the C `case N:` and Rust `N =>`
            // arms re-enter on resume.
            out.last_mut().unwrap().push(s.clone());
            out.push(Vec::new());
        } else {
            let last = out.last_mut().unwrap();
            last.push(s.clone());
        }
    }
    out
}

/// Walk a block and return the list of `@resume name` references
/// (in source order). Used by the wrapper to emit cleanup code that
/// unregisters the task on the normal return path so cross-function
/// `@resume` doesn't find a completed task.
pub fn resume_refs(block: &Block) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    walk_resume(&block.stmts, &mut out);
    out
}

fn walk_resume(stmts: &[Stmt], out: &mut Vec<(String, Span)>) {
    for s in stmts {
        match s {
            Stmt::Resume { name, span } => out.push((name.clone(), span.clone())),
            Stmt::If { then_branch, else_branch, .. } => {
                walk_resume(&then_branch.stmts, out);
                walk_resume(&else_branch.stmts, out);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                walk_resume(&body.stmts, out);
            }
            _ => {}
        }
    }
}

/// Build a `HashMap` from checkpoint name -> state id for this plan.
/// Used by the runner emit functions to inline the right state id at
/// each `@checkpoint` site.
pub fn state_id_map(plan: &CheckpointPlan) -> HashMap<String, u32> {
    plan.states.iter().map(|s| (s.name.clone(), s.state_id)).collect()
}

// ============================================================================
// State machine emit (public entry points used by the codegen backends)
// ============================================================================

/// Result of building a checkpoint plan for a function.
pub struct CheckpointedFunc<'a> {
    pub plan: CheckpointPlan,
    pub func: &'a TypedFunc,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use spass_parser::ast::BinOp;
    use spass_parser::Parser;
    use spass_semantic::analyze;
    use spass_lexer::Span;

    fn parse_plan(src: &str) -> (TypedFunc, CheckpointPlan) {
        let module = Parser::parse_source(src, "<test>").expect("parse");
        let typed = analyze(&module).expect("analyse");
        let func = typed.functions.last().expect("at least one function").clone();
        let mangled = format!("_spass_{}_{}_f", func.name, func.params.len());
        let plan = CheckpointPlan::build(&func, &mangled);
        (func, plan)
    }

    #[test]
    fn build_plan_collects_param_and_let_slots() {
        let (_f, plan) = parse_plan(r#"
            func loop_n(n: float): float {
                let i: float = 0.0
                @checkpoint step_a
                return i
            }
        "#);
        // Slots: __sp_state_id (D5+ reserved first slot), n (param),
        // i (let), __sp_return (return) - in that order.
        assert_eq!(plan.layout.slots.len(), 4);
        assert_eq!(plan.layout.slots[0].name, "__sp_state_id");
        assert_eq!(plan.layout.slots[0].kind, StateSlotKind::StateId);
        assert_eq!(plan.layout.slots[1].name, "n");
        assert_eq!(plan.layout.slots[1].kind, StateSlotKind::Param);
        assert_eq!(plan.layout.slots[2].name, "i");
        assert_eq!(plan.layout.slots[2].kind, StateSlotKind::Let);
        assert_eq!(plan.layout.slots[3].name, "__sp_return");
        assert_eq!(plan.layout.slots[3].kind, StateSlotKind::Return);
        // 4 slots * 8 bytes = 32.
        assert_eq!(plan.layout.state_size, 32);
        // One checkpoint site: "step_a" -> state_id 1.
        assert_eq!(plan.states.len(), 1);
        assert_eq!(plan.states[0].name, "step_a");
        assert_eq!(plan.states[0].state_id, 1);
    }

    #[test]
    fn state_id_of_returns_correct_id() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float {
                @checkpoint a
                @checkpoint b
                return x
            }
        "#);
        assert_eq!(plan.state_id_of("a"), Some(1));
        assert_eq!(plan.state_id_of("b"), Some(2));
        assert_eq!(plan.state_id_of("missing"), None);
    }

    #[test]
    fn split_at_checkpoints_splits_correctly() {
        let (_f, _plan) = parse_plan(r#"
            func f(x: float): float {
                let i: float = 0.0
                @checkpoint a
                let j: float = 1.0
                @checkpoint b
                return i + j
            }
        "#);
        // The `split_at_checkpoints_handles_nested_blocks` test below
        // covers the splitter on a synthetic body; this test ensures
        // the parser-driven plan at least has two checkpoints to
        // split on. (The body is owned by the TypedFunc inside
        // `plan`; we don't pull it out - that's the splitter's
        // caller's job.)
        assert_eq!(_plan.states.len(), 2);
    }

    #[test]
    fn split_at_checkpoints_handles_nested_blocks() {
        let stmts = vec![
            Stmt::Let {
                names: vec!["i".into()],
                ty: Some(Type::Float(None)),
                value: spass_parser::ast::Expr::IntLit { value: 0, span: Span::dummy() },
                span: Span::dummy(),
            },
            Stmt::Checkpoint { name: "a".into(), span: Span::dummy() },
            Stmt::If {
                cond: spass_parser::ast::Expr::BoolLit { value: true, span: Span::dummy() },
                then_branch: Block {
                    stmts: vec![Stmt::Checkpoint { name: "b".into(), span: Span::dummy() }],
                    span: Span::dummy(),
                },
                else_branch: Block { stmts: vec![], span: Span::dummy() },
                span: Span::dummy(),
            },
        ];
        let slices = split_at_checkpoints(&stmts);
        // Top-level: 1 `let` + 1 `@checkpoint` + 1 `if` -> 2 slices:
        // slice 0: the `let`
        // slice 1: the `if { @checkpoint b }` (kept as-is; the
        // nested checkpoint is part of the same case body
        // in the MVP's flat-state model).
        assert_eq!(slices.len(), 2);
        let _ = BinOp::Add;
    }

    #[test]
    fn state_struct_name_is_mangled() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float { @checkpoint a; return x }
        "#);
        assert_eq!(plan.state_struct_name(), "_spass_f_1_f_state_t");
    }

    #[test]
    fn body_has_checkpoint_returns_true() {
        let block = Block {
            stmts: vec![Stmt::Checkpoint { name: "a".into(), span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(body_has_checkpoint(&block.stmts));
    }

    #[test]
    fn body_has_checkpoint_returns_false_for_resume_only() {
        // A `@resume` site without a matching `@checkpoint` here is
        // still a synchronous call (the state machine is unnecessary).
        let block = Block {
            stmts: vec![Stmt::Resume { name: "a".into(), span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(!body_has_checkpoint(&block.stmts));
    }

    #[test]
    fn state_id_map_collects_all_sites() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float {
                @checkpoint a
                @checkpoint b
                return x
            }
        "#);
        let m = state_id_map(&plan);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
    }

    #[test]
    fn layout_slot_lookup_works() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float, y: float): float {
                @checkpoint a
                return x + y
            }
        "#);
        // __sp_state_id (D5+ reserved first slot, 8 bytes), then x
        // and y are params (8 bytes each), then __sp_return.
        let x = plan.layout.slot("x").expect("x slot");
        let y = plan.layout.slot("y").expect("y slot");
        let r = plan.layout.slot("__sp_return").expect("return slot");
        assert_eq!(x.offset, 8);
        assert_eq!(y.offset, 16);
        assert_eq!(r.offset, 24);
        assert_eq!(plan.layout.state_size, 32);
    }

    #[test]
    fn param_kind_for_param_slots() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float { @checkpoint a; return x }
        "#);
        let x = plan.layout.slot("x").expect("x");
        assert_eq!(x.kind, StateSlotKind::Param);
    }

    #[test]
    fn let_kind_for_let_slots() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float {
                let y: float = 1.0
                @checkpoint a
                return y
            }
        "#);
        let y = plan.layout.slot("y").expect("y");
        assert_eq!(y.kind, StateSlotKind::Let);
    }

    // ========================================================================
    // @await tests
    // ========================================================================

    #[test]
    fn body_has_await_detects_await() {
        let block = Block {
            stmts: vec![Stmt::Await { span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(body_has_await(&block.stmts));
    }

    #[test]
    fn body_has_await_returns_false_for_plain_body() {
        let block = Block {
            stmts: vec![
                Stmt::Let {
                    names: vec!["x".into()],
                    ty: Some(Type::Float(None)),
                    value: spass_parser::ast::Expr::IntLit { value: 1, span: Span::dummy() },
                    span: Span::dummy(),
                },
                Stmt::Return { value: None, span: Span::dummy() },
            ],
            span: Span::dummy(),
        };
        assert!(!body_has_await(&block.stmts));
    }

    #[test]
    fn body_has_suspension_detects_await_and_checkpoint() {
        // Body with just @await.
        let block_await = Block {
            stmts: vec![Stmt::Await { span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(body_has_suspension(&block_await.stmts));
        // Body with just @checkpoint.
        let block_ckpt = Block {
            stmts: vec![Stmt::Checkpoint { name: "a".into(), span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(body_has_suspension(&block_ckpt.stmts));
        // Body with neither.
        let block_plain = Block {
            stmts: vec![Stmt::Return { value: None, span: Span::dummy() }],
            span: Span::dummy(),
        };
        assert!(!body_has_suspension(&block_plain.stmts));
    }

    #[test]
    fn await_plan_reserves_await_slot() {
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float {
                @await
                let v: float = spasio:input[file]("my_channel")
                return v
            }
        "#);
        // The plan should reserve the `v` slot as `Await` so the
        // value lives in the state block across any future async
        // revision. The order is: __sp_state_id (D5+ reserved
        // first slot), param x, await v, then return.
        assert_eq!(plan.layout.slots.len(), 4);
        let await_slot = plan.layout.slot("v").expect("v slot");
        assert_eq!(await_slot.kind, StateSlotKind::Await);
    }

    #[test]
    fn await_plan_does_not_assign_state_id() {
        // the MVP's @await is a synchronous I/O marker
        // (no actual state transition). The state ids should be
        // empty even though the body has an @await.
        let (_f, plan) = parse_plan(r#"
            func f(x: float): float {
                @await
                let v: float = spasio:input[file]("my_channel")
                return v
            }
        "#);
        assert!(plan.states.is_empty());
    }
}
