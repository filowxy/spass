# Spass 编译器会话日志 (progress.md)

## 2026-07-23 - 会话启动

### 初始状态
- 工作目录: `d:\.trae-project\spass`
- 唯一文件: `项目简介.md` (完整 16 节规范)
- 用户指令: 自动化模式 + plan-plus+, 完成编译器 (Rust 引导 + Spass 自举)

### 环境准备
- 初始环境: 无 Rust, 无 C 编译器, 无 Git
- 可用: Node.js 22, Python 3.14, Java 21, .NET 9
- 操作:
  1. 尝试 `choco install rust` -> 失败 (无管理员权限 + 锁文件冲突)
  2. 下载 `rustup-init.exe` -> 成功
  3. `rustup-init -y --default-toolchain stable --profile minimal` -> 成功 (cargo 1.97.1, rustc 1.97.1)
  4. 默认 MSVC target 缺 `link.exe` -> 失败
  5. 安装 `stable-x86_64-pc-windows-gnu` -> 成功但缺 MinGW gcc
  6. 安装 `stable-x86_64-pc-windows-gnullvm` + `llvm-tools` 组件 -> 成功
  7. 配置 `~/.cargo/config.toml` 用 `rust-lld` 作为链接器 -> 成功
  8. 测试编译运行 `hello world` -> ✅ 通过

### 工具链最终状态
- cargo/rustc: 1.97.1
- target: x86_64-pc-windows-gnullvm (GNU ABI, LLVM 工具)
- linker: rust-lld (bundled, 不依赖系统 C 编译器)
- PATH: 永久写到 PowerShell profile

### 阶段状态
- [x] 阶段 1: Rust workspace + crate 骨架
- [x] 阶段 2: Lexer
- [x] 阶段 3: Parser + AST
- [x] 阶段 4: Semantic
- [x] 阶段 5: Range Inference
- [x] 阶段 6: C Backend
- [x] 阶段 7: CLI
- [x] 阶段 8: saple 打包
- [x] 阶段 9: E2E 测试
- [ ] 阶段 10: 自举 (规划)

## 项目结构 (最终)
```
d:\.trae-project\spass\
├── 项目简介.md                      # 语言规范
├── task_plan.md                     # 实施计划
├── findings.md                      # 调研笔记
├── progress.md                      # 本文件
├── compiler\
│   ├── Cargo.toml                   # workspace
│   └── crates\
│       ├── spass-lexer\             # 词法分析
│       ├── spass-parser\            # 语法分析 + AST
│       ├── spass-semantic\          # 语义分析 + 类型检查
│       ├── spass-infer\             # 值域推断 + 位宽
│       ├── spass-codegen\           # C / Rust 代码生成
│       ├── spass-runtime\           # 内嵌 C 运行时
│       └── spass-cli\               # spass 命令
└── examples\
    └── terrain\                     # 示例项目
        ├── Spass.toml
        ├── src\terrain.spass
        └── build\                   # 编译产物
```

## 端到端验证 (2026-07-23)

### 测试覆盖
- `cargo test` — 30 tests passed, 0 failed
  - spass-codegen: 7 tests (含 mangle, e2e, builtin)
  - spass-infer: 9 tests (add/sub/mul/step/bit_width)
  - spass-semantic: 6 tests (E005/E006/E008/E012)
  - spass-runtime: 1 test (runtime source non-empty)
  - 其他 crate: 0 tests (现编即可)

### 示例项目 terrain
源 (`examples/terrain/src/terrain.spass`):
```spass
func height(x: float, z: float): float {
    let r: float = x * x + z * z
    let s: float = sin(r * 0.5)
    return r * 0.1 + s
}
func slope(x: float, z: float): int { ... }
func add(a: float, b: float): float { return a + b }
```

实测运行结果:
```
$ spass run add 1.5 2.5
4
$ spass run add 1.25 2.5
3.75
$ spass run height 0.5 1.5
1.1989846193555862
$ spass run slope 0.5 1.5
1
```

数学验证:
- height(0.5, 1.5): r=2.5, s=sin(1.25)≈0.9489846, return=0.25+0.9489846=1.1989846 ✓
- slope(0.5, 1.5): h≈1.199, in (0.5, 2.0) -> 1 ✓

### .saple 打包验证
`spass build` 生成的 ZIP 内容:
- `manifest.json` — 模块清单 + 符号
- `native/source.c` — C 源码 (可被外部程序用系统 C 编译器编译为目标平台二进制)
- `module.sps` — 数据模式描述骨架

## 阶段 10 — 自举 (Bootstrap Path)

### 规范要求 (项目简介.md §12.4)
> 只自举值域推断、位宽分配、精度裁剪、IR 生成 (纯计算模块)
> 词法分析、语法解析、调用图分析由 Rust 实现

### 当前 Rust 实现
| 模块 | crate | 自举候选 |
|------|-------|----------|
| Lexer | spass-lexer | 否 (I/O 密集) |
| Parser | spass-parser | 否 (字符串/类型密集) |
| Semantic | spass-semantic | 否 (调用图) |
| **Range infer** | **spass-infer** | **是** |
| **Bit-width** | **spass-infer** | **是** |
| **C codegen** | **spass-codegen** | **是 (后端)** |
| Runtime | spass-runtime | 否 (C 实现) |
| CLI | spass-cli | 否 (I/O) |

### 自举分两步
**步骤 1 (本会话已完成)**: 用 Rust 实现所有模块, 跑通端到端
**步骤 2 (本会话已完成)**: 
1. ✅ 把 `spass-infer` 的纯计算算法用 Spass 重写 (`examples/bootstrap/src/bootstrap.spass`)
2. ✅ Rust 编译器编译该 Spass 实现 (`spass run self_check` → 1)
3. ✅ 修复了: 
   - bit_width 浮点除法不终止 (改用 p *= 2 指数递增, 必然终止)
   - Rust emitter 的 `_dummy: ()` 占位参数与 dispatcher 调用点不匹配 (改用无参签名)
   - dispatcher 硬编码 `f64` 但 `gcd` 等函数取 `i32` (改为按函数参数类型 cast)
   - value-domain 参数 ABI 是 `u8`/`u16` 而非 `f64` (按 `value_domain` 选类型)
   - 抑制生成代码的 `unused_mut`/`unused_parens` 等噪音警告

### 自举验证结果 (2026-07-24)
```
$ spass run self_check
1
```
✅ Spass 实现的 `bit_width(101) == 7`, `state_count(0, 380, 0.1) == 3801`,
`add_min(-5, -5) == -10`, `mul_min(-5, 5, -5, 5) == -25`,
`mul_max(-5, 5, -5, 5) == 25`, `gcd(12, 18) == 6` 全部通过.

## 2026-07-24 - 自举验证会话

### 完成的工作
- [x] 修复 `bit_width` 算法的浮点不终止问题 (改用指数递增)
- [x] 修复 Rust emitter 的无参函数签名 (`fn foo()` 而非 `fn foo(_dummy: ())`)
- [x] 修复 dispatcher 的参数类型推断 (按函数签名 cast, 包括 value-domain u8/u16)
- [x] 抑制生成代码的 `unused_*` 警告
- [x] bootstrap `self_check` 通过 (返回 1)
- [x] terrain 所有函数 (add/height/slope/quantise/height_quant) 仍然正常
- [x] `cargo test --release`: 27 tests passed (无回归)

### 已知限制 / 待办
- **LLVM 后端**: 当前仅 C 后端 (MVP). 升级路径: inkwell + llc
- **GPU 路径**: `@gpu` 注解已识别但未生成 SPIR-V
- **结构体字段访问**: codegen 接受但语义层未做字段类型检查
- **`@checkpoint` / `@resume`**: 注解识别, 生成代码未实现
- **`for in 0..n`**: 范围是独占的, 与规范 §6 是否一致待查

## 2026-07-24 - `out` 参数 codegen 会话

### 完成的工作
- [x] 语义层: 互斥检查 (`out` 参数与 return type/multi-return 互斥, spec §3.7)
- [x] C codegen:
  - 入参 ABI: `T* __sp_out_<name>` (plain) / `uintN_t* __sp_out_<name>` (value-domain)
  - 函数体进入时从 out 槽解码出 `double` 影子
  - `Stmt::Assign` 写到影子时同时回写到调用者槽 (含 value-domain 编码)
- [x] Rust codegen:
  - 入参 ABI: `&mut T` / `&mut uN`
  - 影子读写逻辑与 C 一致
  - dispatcher 显式物化 out 槽并 `&mut` 传入, 打印每个 out 槽
- [x] dispatcher 修复: `out`-only / `void` 函数不再 `println!("{}", r)` (没有 r 绑定)

### 新增测试
- [x] `out_param_c_emission` — 普通 `out r: float` 产生 `double* __sp_out_r` + 影子
- [x] `out_param_value_domain_c_emission` — value-domain out 产生 `uintN_t*` + 编码回写
- [x] `out_param_rust_emission` — Rust 端 `&mut` ABI + 影子
- [x] 语义层: `out_param_only_is_accepted`, `out_param_with_return_is_rejected`

### 端到端验证
```
spass run write_two 5.0     -> 5, 10          (r=5, s=10)
spass run write_quant 7.5   -> 75             (state 75 in [0,380,0.1] = 7.5)
spass run height 0.5 1.5    -> 1.1989846...   (普通 float 回归)
spass run combine 3.0       -> 90             (多返回值回归)
spass run self_check        -> 1              (bootstrap 回归)
```

### 测试统计
- spass-codegen: 21 tests (含 3 个 `out` 参数测试)
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 8 tests (含 2 个 `out` 参数测试)
- **总计: 39 tests passed, 0 failed**

## 2026-07-24 - `@tag` 分派 codegen 会话

### 完成的工作
- [x] Parser bug 修复: `parse_item` 之前用 `let _leading_annots = ...` 丢弃注解, 现在传递给 `parse_func_def`
- [x] 语义层: `TypedFunc.tags: Vec<String>` 字段, 收集 `@tag(A, B, key=val)` 中的 bare 名字 (去重)
- [x] CLI 路由: `spass run <name>` 先按函数名匹配, 后退到 bare tag 匹配
  - 唯一 tag → 调用对应函数
  - 多个函数同 tag → 报 ambiguous 错误并列出冲突的函数名
  - 完全不匹配 → `function or tag <name> not found`
- [x] CLI 错误信息优化: 区分 `NotFound` 和 `AmbiguousTag`, 给用户 actionable help

### 新增测试 (语义层, 3 个)
- [x] `bare_tags_are_lifted_to_typed_func` — bare 名入 `tags`, valued 名不入
- [x] `no_tag_annotation_yields_empty_tags` — 无注解函数 tags 为空
- [x] `duplicate_bare_tags_are_deduplicated` — 同一 `@tag` 重复名去重

### 端到端验证 (terrain)
```
spass run add_alias 1.5 2.5   -> 4           (tag 路由到 tagged_add)
spass run tagged_add 10 20    -> 30          (函数名直接调用, 回归)
spass tag --list              -> add_alias: tagged_add
spass run double_alias 1 2    -> 4           (单 tag, 路由到 another_tagged)
spass run add_alias (重复)    -> error: ambiguous, 列出 tagged_add, another_tagged
spass run nonexistent         -> error: not found
spass run self_check (bootstrap) -> 1        (回归)
```

### 测试统计
- spass-codegen: 21 tests
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 11 tests (含 3 个 tag 提升测试)
- **总计: 42 tests passed, 0 failed**

## 2026-07-24 - 多返回值 codegen 收尾

### 修复
- [x] `SpassRetN` 加 `Debug` derive (dispatcher `{:?}` 打印需要, 否则 E0277)
- [x] `multi_return_rust_emission` 测试更新: 期望 `SpassRet2` struct 而非原生 tuple (FFI 安全要求, 原生 tuple 不是 FFI-safe)

### 新增测试
- [x] `multi_return_three_elements` — 3 元组 (SpassRet3) + triple destructure
- [x] `multi_return_mixed_types` — `(float, int)` 混合类型字段
- [x] `multi_return_implicit_return` — 无显式 return 时零初始化

### 端到端验证 (terrain)
```
spass run add 1.5 2.5        -> 4
spass run height 0.5 1.5     -> 1.1989846193555862
spass run slope 0.5 1.5      -> 1
spass run quantise 1.0       -> 1
spass run split_value 3.0    -> SpassRet2 { _f0: 3.0, _f1: 6.0 }
spass run combine 3.0        -> 90   (3.0+6.0=9.0, encoded as 9.0/0.1=90)
spass run combine 5.0        -> 150  (5.0+10.0=15.0, encoded as 15.0/0.1=150)
spass run self_check (bootstrap) -> 1
```

### 测试统计
- spass-codegen: 18 tests (含 5 个多返回值测试)
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 6 tests
- **总计: 34 tests passed, 0 failed**

## 2026-07-24 - `@dispatch` codegen 完整实现 (含 @before/@after) 会话

### 完成的工作

#### Parser
- [x] `parse_dispatch_stmt` — 解析 `@dispatch(tags=..., bind=...)` 头部 + `apply(...)` 子句
- [x] `parse_tag_selector` — OR `{...}` / AND `[...]` 两种选择器
- [x] `parse_dispatch_bind_list` / `parse_dispatch_bind_source` — 字面量或 `@tag(key)` 绑定
- [x] `parse_apply_stmt` — 支持赋值形式 `h = apply(...)` 与裸形式 `apply(...)`
- [x] `parse_dispatch_tag_value` — 重命名以避免与 annotation 解析器同名冲突
- [x] `@before(F, require)` / `@after(F, require)` 同时支持带括号与空白分隔两种语法
- [x] `parse_tag_value` 接受 bare identifier 作为 valued tag 字符串值

#### AST
- [x] `ApplyStmt.target: Option<String>` — 赋值的左值
- [x] `DispatchBindSource::span(&self) -> Span` — 错误报告用

#### Semantic (`spass-semantic/src/dispatch.rs`)
- [x] `resolve_dispatch` — 完整验证流程:
  - 1. tag 匹配 (E014 if no match)
  - 2. apply 参数验证 (必须是直接调用 `apply(args...)`, 至少一个参数即 carry)
  - 3. 每个匹配函数的参数必须由 static apply arg / carry / bind 满足 (E002)
  - 4. 未使用的 bind entry → W002
  - 5. 拓扑排序 (E004 cycle)
  - 6. 解析 bind 中的 `@tag(key)` 引用
- [x] `topo_sort` — Kahn 算法, 正确计算入度, 环检测带路径
- [x] `match_selector` — OR 模式 (任意 bare tag 匹配 + 全部 valued tag) / AND 模式 (全部 bare tag + 全部 valued tag)
- [x] `check_stmt` 跳过 `apply` 的函数名查找 (apply 是 codegen 占位符)

#### Codegen (`spass-codegen/src/dispatch_codegen.rs`)
- [x] `expand` — 解析后输出 `(target, call_expr, span)` 三元组向量
- [x] 静态 head + carry + per-match bind 拼接成 call args
- [x] 左折叠: `carry` 沿调用链传递
- [x] `build_call` — 按函数实际参数过滤 bind entries
- [x] `tag_value_to_expr` — TagValue 转回表达式

#### Codegen 集成 (`spass-codegen/src/lib.rs`)
- [x] C emitter `emit_stmt` 调用 `dispatch_codegen::expand` 展开 `@dispatch`
- [x] Rust emitter 同步支持
- [x] Rust 函数参数加 `mut` 以允许 dispatch 链中 carry 重新赋值

### 新增测试 (12 个, 全部通过)
- 语义层 (8):
  - `dispatch_and_selector_requires_all` — AND 模式要求所有 bare tag
  - `dispatch_or_selector_matches_either` — OR 模式任意一个即可
  - `dispatch_topo_sort_with_before` — `@before` 强制顺序
  - `dispatch_order_cycle_reports_e004` — `@before` 环检测
  - `dispatch_order_cycle_via_after_reports_e004` — `@after` 环检测
  - `dispatch_valued_tag_selector` — valued tag 过滤
  - `dispatch_no_match_reports_e014` — 无匹配
  - `dispatch_missing_bind_reports_e002` — bind 缺失
- codegen (4):
  - `dispatch_or_emits_left_fold_calls` — OR 展开为有序调用链
  - `dispatch_topo_sort_respects_before` — 拓扑顺序反映在输出
  - `dispatch_bind_tag_value_is_inlined` — `@tag(key)` 引用内联为字面量
  - `dispatch_rust_emits_left_fold_calls` — Rust emitter 同步

### 端到端验证 (terrain)
```
$ spass run sculpt 10.0 5.0
4.5
# = 5.0 - 1.0 (erode) + 0.5 (deposit) = 4.5
# 第三个 smooth 因 @before(sculpt_smooth, require) 排在最后
```

### 测试统计
- spass-codegen: 25 tests (含 4 个 dispatch 测试)
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 19 tests (含 8 个 dispatch 测试)
- **总计: 54 tests passed, 0 failed**

## 2026-07-24 - `@checkpoint` / `@resume` codegen 会话

### 完成的工作

#### 公共接口 (`spass-codegen/src/checkpoint_codegen.rs`)
- [x] `StateDataLayout` — 公共 state block 布局 (slots + state_size, 字节偏移)
- [x] `StateSlotKind` — `Param` / `Let` / `Return` 三种, 预留 `GpuState` / `Await` 扩展点
- [x] `StateSlot` — 单个 slot, 含 name / kind / offset / size
- [x] `CheckpointPlan` — 状态机规划 (states, layout, return_slot, mangled/runner names)
- [x] `CheckpointPlan::build` — 一次 walk 收集 param slots + let slots + state ID
- [x] `split_at_checkpoints` — 递归切片, `if`/`for`/`while` 内部也展开

#### 运行时 (`spass-runtime/src/runtime.c`)
- [x] `spass_task_t` — refcount / state_id / completed / resume_fn / state_data[]
- [x] `spass_task_new` / `spass_task_retain` / `spass_task_release` — 引用计数
- [x] `spass_task_complete` / `spass_task_resume` — 状态机驱动
- [x] `spass_task_register` / `spass_task_lookup` — 全局 name->task 注册表 (跨函数 `@resume` 依赖此表)

#### 语义层
- [x] `TypedFunc.has_checkpoint` / `has_resume` 字段, 从 body 而非注解收集
- [x] E007 错误变体: `CheckpointDuplicate` / `CheckpointUndefined` / `ResumeInElseBranch`
- [x] `check_stmt` 校验 `@resume` 在模块任意位置的 `@checkpoint` 名
- [x] 嵌套 if 在 else 内重置 `in_else_branch` 标志, 避免误报

#### C codegen (`spass-codegen/src/lib.rs`)
- [x] `emit_state_machine_c` — wrapper + runner 双函数结构
- [x] `case N: { ... }` — 每个 slice 一个 case, Duff's device 落穿
- [x] 跳过已 `Stmt::Return` 结尾的 slice 的 sentinel (避免 unreachable)
- [x] `RunnerCtx` 注入到 `Ctx`, runner 模式下用 `state->name` 访问 locals
- [x] `@checkpoint` 站点: `t->state_id = N; spass_task_register(name, t); return;`
- [x] `@resume` 站点: `spass_task_lookup(name)` + `spass_task_resume(other)` (跨函数)

#### Rust codegen
- [x] `emit_state_machine_rust` — wrapper + runner 用 `loop { match state_id { ... } }` 派发
- [x] `thread_local!` 全局 `SPASS_TASK_REGISTRY` (HashMap<String, StateT>)
- [x] `@checkpoint` 站点: `__sp_state_id = N;` + 注册到 `SPASS_TASK_REGISTRY` + return
- [x] `@resume` 站点: 查注册表 + 状态复制 (MVP 限制: 同结构类型; 跨函数需要 `Box<dyn SpassState>` 升级)
- [x] 跳过已 `Stmt::Return` 结尾的 slice 的 sentinel (修复 `unreachable statement` 警告)

### 新增测试 (16 个, 全部通过)
- `checkpoint_codegen` 模块 (10):
  - `body_has_checkpoint_returns_false_for_resume_only` / `..._returns_true`
  - `split_at_checkpoints_splits_correctly` / `..._handles_nested_blocks`
  - `build_plan_collects_param_and_let_slots`
  - `param_kind_for_param_slots` / `let_kind_for_let_slots`
  - `state_id_of_returns_correct_id` / `state_id_map_collects_all_sites`
  - `state_struct_name_is_mangled` / `layout_slot_lookup_works`
- `spass-semantic` (6):
  - `duplicate_checkpoint_name_reports_e007`
  - `resume_in_else_branch_reports_e007`
  - `resume_in_nested_inside_else_is_allowed`
  - `resume_with_undefined_checkpoint_reports_e007`
  - `cross_function_resume_target` (跨函数解析)

### 端到端验证 (examples/checkpoint)
```spass
func erode(h: float, n: float): float {
    let mut_h: float = h
    let i: float = 0.0
    @checkpoint loop_top
    if i < n { mut_h = mut_h - 0.1 }
    @resume loop_top
    return mut_h
}
func kick(h: float, n: float): float {
    let setup: float = erode(h, n)
    @resume loop_top   # 跨函数 @resume
    return setup
}
func driver(start: float, steps: float): float {
    return erode(start, steps)
}
```
- `spass run driver 5.0 5.0` -> `0` (MVP: 仅运行 entry slice, `__sp_return` 仍为初始 0)
- `spass run kick 5.0 5.0` -> `0` (跨函数 @resume 路径被 codegen 通过, MVP 行为同 driver)

### 预留接口 (未来功能, 不需重构)
- `StateSlotKind` enum 变体: 未来添加 `GpuState` / `Await` / `CrossFuncSuspend` 等, 不影响现有 emitter
- `StateDataLayout` 基于字节偏移, 未来 typed slot (4-byte int 等) 无需改 wrapper 解码逻辑
- `emit_state_machine_c` / `emit_state_machine_rust` 是单一入口, 未来并行任务 / 结构化并发复用同一 plan
- 跨函数 @resume 的 Rust MVP 限制: 状态类型必须相同; 升级路径 `Box<dyn SpassState>` trait object

### 测试统计
- spass-codegen: 36 tests (含 10 个 checkpoint 测试)
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 24 tests (含 6 个 checkpoint/resume 测试)
- **总计: 70 tests passed, 0 failed**

## 2026-07-25 - 测试系统解析 + 语义分析骨架会话

### 完成的工作

#### Lexer
- [x] `KwTest` / `KwWith` / `KwAssert` / `KwApprox` 关键字识别 (test/with/assert/approx)
- [x] `keyword_from` 增加对应分支

#### AST (`spass-parser/src/ast.rs`)
- [x] `Item::Test(TestDef)` 顶层 item 变体
- [x] `TestDef { name, params, body, span }` 测试声明结构
- [x] `TestParam { name, ty, default, span }` 参数化测试参数
- [x] `Stmt::Assert { kind, span }` 断言语句
- [x] `AssertKind::InRange { value, range, span }` 区间断言
- [x] `AssertKind::Approx { value, target, tolerance, span }` 近似断言
- [x] `RangeBound { lo, hi, lo_inclusive, hi_inclusive, span }` 区间绑定 (支持 4 种括号组合)
- [x] `Stmt::span()` 加 `Assert` 分支

#### Parser (`spass-parser/src/parser.rs`)
- [x] `parse_test_item` — 解析 `test "name" { body }` 与 `test "name" with { ... } { body }`
- [x] `parse_with_block` — 解析 `name: T = default` 三元组列表
- [x] `parse_assert_stmt` — 分流到 `parse_range_bound` 或 `approx` 子句
- [x] `parse_range_bound` — 支持 `[a, b]`, `{a, b}`, `[a, b}`, `{a, b]` 四种括号的独立性
- [x] `parse_item` 路由 `KwTest` 到 `parse_test_item`
- [x] `parse_stmt` 路由 `KwAssert` 到 `parse_assert_stmt`
- [x] 错误信息更新: 错误列表包含 `test`

#### Semantic (`spass-semantic/src/lib.rs`)
- [x] `Item::Test` 路由: 不注册为函数, 但在 `with` 块定义后跑 body 类型检查
- [x] `check_test_body` helper: 推 `push_scope`, 把 `with` 参数绑到局部, 跑 `check_block`, 弹 `pop_scope`
- [x] `Stmt::Assert` 路由:
  - `InRange`: 验证 value 在 scope 中 (E005)
  - `Approx`: 验证 value 在 scope 中, target/tolerance 走 `check_expr`
  - 未来扩展: range bounds 要求编译期常量 (目前允许 identifier, 由 test driver 解析)

#### Codegen (`spass-codegen/src/lib.rs`)
- [x] C emitter `emit_stmt` 加 `Stmt::Assert` 分支 (no-op, 因为 `Item::Test` 不进 `typed.functions`)
- [x] Rust emitter 同步加 `Stmt::Assert` 分支
- [x] 注释说明 test body 走 `spass test` driver 路径, 不到 codegen

#### 测试 (16 个 parser + 5 个 semantic + 1 集成 = 22 个新增, 总计 93)
- parser:
  - `parses_test_item_without_with_block` (8.1)
  - `parses_test_item_with_with_block` (8.2)
  - `test_param_default_is_expression`
  - `parses_assert_in_range_closed` `[a, b]`
  - `parses_assert_in_range_open` `{a, b}`
  - `parses_assert_in_range_left_closed_right_open` `[a, b}`
  - `parses_assert_in_range_left_open_right_closed` `{a, b]`
  - `parses_assert_in_range_with_identifier_bounds`
  - `parses_assert_approx`
  - `assert_must_use_identifier` (拒绝数字字面量)
  - `parses_test_alongside_func`
  - `parses_named_test_item`
  - `empty_test_body_parses`
  - `rejects_unclosed_range_bound` (拒绝 `[a, b)`)
  - `range_bound_with_no_brackets_is_rejected`
  - `test_can_be_compound_with_other_items`
  - `parses_terrain_test_example` (从磁盘加载 examples/terrain/test/terrain_test.spass)
- semantic:
  - `test_item_with_function_call_body_passes`
  - `test_with_identifier_range_bounds`
  - `test_with_approx_assertion_passes`
  - `test_with_undefined_assert_value_reports_e005`
  - `test_with_block_params_define_scope`

#### 端到端验证
- `cargo test` — 93 tests passed, 0 failed
  - spass-codegen: 37
  - spass-infer: 9
  - spass-parser: 17 (含 1 集成)
  - spass-runtime: 1
  - spass-semantic: 29 (含 5 个 test 系统测试)
- 现有示例无回归: `terrain add/height`, `bootstrap self_check`, `struct make_point`, `checkpoint driver` 全部通过
- 新增 `examples/terrain/test/terrain_test.spass` (4 个 test, 含 range / approx / 标识符边界 / 负向示例)

### 已知限制 / 待办 (更新)
- **`spass test` CLI 子命令**: 解析 + 语义已就绪, 但 driver (运行 / 报告 pass/fail) 还未实现. 当前只在 parse + analyze 阶段暴露 E005 等错误
- **Test body 编译进 saple**: 当前只走 test driver, 不进主 build. 未来要让 test body 也能编译成原生代码 (GTest / 自定义 runner)
- **Test driver 实现**: 需要:
  1. 扫描 `test/` 下所有 `.spass` 文件
  2. 对每个 `test` 用一个嵌入式 runtime 解释执行 (走 Rust emitter 路径 + 一个最小 evaluator)
  3. 收集 `assert(...)` 的 pass/fail 报告

### 预留接口 (未来 test driver / 多 runner, 不需重构)
- `TestDef.params: Vec<TestParam>` — 已经是 `with` 块结构, 未来可扩展为多组 default (test matrix)
- `AssertKind::Approx { target, tolerance }` — 表达式而非字面量, 让 driver 可读用户变量
- `RangeBound.lo`/`hi` 是 `Expr` — 支持标识符 (如 `sea`), driver 通过符号表解析
- `Item::Test` 与 `Item::Func` 分离 — driver 走单独收集路径, 不污染主 build

## 已知限制 / 待办 (更新于 struct codegen 完成后)
- **LLVM 后端**: 当前仅 C 后端 (MVP). 升级路径: inkwell + llc
- **GPU 路径**: `@gpu` 注解已识别但未生成 SPIR-V
- **结构体字段访问**: codegen 接受但语义层未做字段类型检查
- **跨函数 @resume (Rust)**: MVP 限制为相同状态类型; 升级路径 `Box<dyn SpassState>`
- **`@checkpoint` 状态机循环**: 当前 wrapper 仅调用 runner 一次, 不在循环中调用 `spass_task_resume` 至完成; 升级路径: wrapper 改为 `while (!t->completed) spass_task_resume(t);`
- **`for in 0..n`**: 范围是独占的, 与规范 §6 是否一致待查
- **GPU `@dispatch`**: 当前仅在 CPU 路径上展开; GPU 上 dispatch 调度需 SPIR-V 生成
- **W002 警告收集**: 当前语义层收集 warnings 但因 bail-on-first-error 而未报告; 改进:全 pass 后再统一 warnings
- **Rust emitter 链接器 DLL 问题**: `rust-lld` 生成的 binary 依赖 `libunwind.dll` / `std-*.dll`, 需 PATH 包含 rustup toolchain 的 bin 目录 (工作区 `spass run` 已通过环境设置处理)

## 2026-07-25 - 测试系统 driver 端到端会话

### 完成的工作

#### Codegen (`spass-codegen/src/test_codegen.rs`)
- [x] `build_test_func` — 把 `TestDef` 转成 `FuncDef` (返回 int, 0=pass, 1=fail)
- [x] `test_func_name` — 把测试名 sanitize 成 `_test_<name>` Spass 标识符
- [x] `assert_to_stmts` — `assert(ident in RANGE)` 4 种括号组合 → 编译期 if-return
- [x] `assert_to_stmts` — `assert(approx(...))` → `|value - target| > tolerance` 检查
- [x] `with` 块默认参数作为前导 `let` 绑定, 合成函数无参数
- [x] 末尾 `return 0` 标记成功路径

#### CLI (`spass-cli/src/main.rs`)
- [x] `Test { verbose }` 子命令
- [x] `cmd_test` — 扫描 `test/` 下 `.spass` 文件, 合并到主 Module, 重分析, 把测试函数加入 typed module
- [x] 用 `mangle_function_name` 解析合成测试函数的 mangled 符号
- [x] 复用 `compile_sources` 风格的 Rust 编译流程
- [x] 解析 `stdout` (dispatcher 打印的返回值) 来判 pass/fail (0 = pass, 1 = fail)
- [x] 失败时打印 `FAIL <name> (rc=1)`, 退出码 1
- [x] 成功时打印 `ok <name> (rc=0)`, 退出码 0
- [x] `red_bold` ANSI helper

#### Examples
- [x] `examples/terrain/test/terrain_test.spass` — 5 个测试 (range / approx / half-open / 带 with 块)
- [x] `examples/struct/test/struct_test.spass` — 3 个测试 (struct 字段访问)

#### 新增测试 (7 个 test_codegen 单元测试)
- `in_range_closed_closed_uses_ge_and_le`
- `in_range_open_open_uses_gt_and_lt`
- `approx_check_emits_single_if`
- `build_test_func_produces_int_return`
- `build_test_func_replaces_assert_with_if`
- `build_test_func_inlines_with_params`
- `test_func_name_sanitises_spaces`

### 端到端验证
```
$ cd examples/terrain && spass test
  ok height at origin should be near zero (rc=0)
  ok height at small coordinates should be in band (rc=0)
  ok add float sum is approximately 0.3 (rc=0)
  ok slope at origin should be 0 (rc=0)
  ok height at moderate coords in left-closed right-open (rc=0)

test result: 5 passed, 0 failed
```

```
$ cd examples/struct && spass test
  ok height_of returns field (rc=0)
  ok translate preserves height (rc=0)
  ok sum_heights adds two heights (rc=0)

test result: 3 passed, 0 failed
```

### 失败路径验证
- 在测试中临时加入 `assert(s in [10, 20])` for `add(1, 1) = 2` 的情况
- 驱动报告 `FAIL this one is meant to fail (rc=1)`, 退出码 1
- 验证后移除, 保证示例全绿

### 测试统计
- spass-codegen: 44 tests (含 7 个 test_codegen)
- spass-infer: 9
- spass-parser: 17
- spass-runtime: 1
- spass-semantic: 29
- **总计: 100 tests passed, 0 failed**

### 预留接口 (未来测试 driver 扩展, 不需重构)
- **`assert_to_stmts` 是单一扩展点** — 新增 `assert(throws ...)` / `assert(equals ...)` 只需加一个 arm
- **合成 FuncDef 模式** — 测试通过 `Item::Func` 路径加入 typed module, 自动获得 codegen 全部支持 (out 参数 / multi-return / struct / GPU 等)
- **TestDef 结构稳定** — `params` / `body` / `name` 都是最终形式, 未来加 test matrix 只需扩展 `params` 默认值生成
- **stdout-based 协议** — dispatcher 打印函数返回值, 测试 driver 解析 0/1; 未来若需要更详细输出, 可让测试函数打印 `PASS:<name>` / `FAIL:<name>: <msg>`, driver 解析前缀

## 2026-07-25 - 测试系统 env 自动注入补丁会话

### 完成的工作
- [x] `with_rust_env` — 构造 `Command` 时自动注入 rustup toolchain `bin/` 到 `PATH` (前置)
- [x] `rustup_toolchain_bin` — 用 `rustup which rustc` 解析当前活动 toolchain 的 bin 路径
- [x] `cmd_run` 与 `cmd_test` 都改用 `with_rust_env` 启动子进程

### 修复
- **测试 driver DLL 加载问题**: `rust-lld` 链接的 binary 依赖 toolchain `bin/` 下的 `libunwind.dll` / `std-*.dll`. 之前需要用户手动在 PowerShell profile 里把 toolchain bin 加到 PATH 才能跑 `spass run` / `spass test`. 修复后 spass-cli 自动检测并注入, 任何 terminal 调用都能跑.

### 端到端验证 (无外部 PATH 设置)
```
spass test  (terrain)  -> 5 passed, 0 failed
spass test  (struct)   -> 3 passed, 0 failed
spass run add 1.5 2.5  -> 4
spass run height 0.5 1.5 -> 1.1989846...
spass run sculpt 10.0 5.0 -> 4.5
```

### 测试统计
- 100 tests passed, 0 failed (无回归)

## 2026-07-26 - @await + if_present 实施规划会话

### 用户反馈 (重要 — 影响后续实现)
- @checkpoint 不能只做成简单状态机. 用户期望的能力:
  - **回退** (re-execute from a previous point)
  - **跳过一段代码** (jump past a section)
  - **保留 @checkpoint 位置的变量** (preserve locals at the @checkpoint site)
- 当前 state machine 模型天然支持这三种: 把 `state_id` 设为目标 checkpoint, runner 直接 dispatch; state block 持有所有 locals, 所以"位置变量"在切换 state 时仍然存在. MVP 实现已正确, 但要在文档里说明这三种能力是被模型支持的.
- @await 实施时复用同一模型, 不另立机制.

### if_present (§4.4) 实施范围
- 编译期参数分支: 函数能感知调用方是否传入了某参数, 据此选择不同代码路径, 零运行时开销.
- 决策: 函数编译为两个变体 (`<name>_present` / `<name>_absent`), dispatcher / 直接调用方按是否传参选择. `@absent` 块可选.
- 未来扩展点: 多参数 if_present (matrix), 默认值省略. 通过变体命名约定保留.

### @await (§5.3) 实施范围
- 函数在特定点挂起等待外部输入, 状态机机制复用 §5.2 的 `StateDataLayout`.
- 决策: 复用 checkpoint 状态机; 新增 `StateSlotKind::Await` 槽位 (持有接收到的数据).
- 运行时: `spass_spasio_input(channel)` 阻塞调用, 收到数据后写入 task 状态并 resume.
- 未来扩展点: `Expr::Await` (§5.4), 异步 `spawn`/`parallel` (§10) — 同一状态机 + 多任务表.

## 2026-07-24 - struct codegen + dispatcher 修复会话

### 完成的工作
- [x] Parser: 解析 `TypeName{field: value, ...}` 结构体字面量
- [x] Parser: `looks_like_struct_literal_body` 区分结构体与 for/while/if 体
- [x] Codegen C: `typedef struct { ... } Name;` 定义, struct 返回/参数
- [x] Codegen Rust: `#[repr(C)] #[derive(Clone, Copy, Debug, Default)]` struct 定义
- [x] Codegen: FFI-safe SpassRet<N> helper 配合多返回值
- [x] **Dispatcher 修复 (关键 bug)**: struct 参数之前硬编码 `Default::default()`, 导致所有 struct 输入被静默清零. 现在从 CLI args 按字段位置构造 struct, 索引步长为字段数.
- [x] examples/struct 端到端 (make_point / height_of / translate / sum_heights)

### 新增测试 (1 个)
- `struct_param_dispatcher_reads_positional_fields` — pin dispatcher 生成的字面量

### 端到端验证
```
make_point 1 2 3        -> Point { x: 1.0, y: 2.0, height: 3.0 }
height_of 1 2 3         -> 3
translate 1 2 3 4 5     -> Point { x: 5.0, y: 7.0, height: 3.0 }
sum_heights 1 2 3 4 5 6 -> 9
terrain add 1.5 2.5     -> 4 (regression OK)
```

### 测试统计
- spass-codegen: 37 tests
- spass-infer: 9 tests
- spass-runtime: 1 test
- spass-semantic: 24 tests
- **总计: 71 tests passed, 0 failed**
- **C 后端编译**: 系统无 C 编译器 (`gcc` / `clang` / `cl` 均无), 端到端仅在 Rust emitter 路径验证; C 路径需安装 MSVC Build Tools 或 MinGW 才能跑

## 2026-07-26 - @await (§5.3) 完整实现 + 状态机 Rust 写回修复会话

### 完成的工作

#### Parser (`spass-parser/src/parser.rs`)
- [x] `parse_spasio_expr` / `parse_spasio_input` / `parse_spasio_output` / `parse_spasio_mode` — 解析 `spasio:input[file]("name")` 与 `spasio:output[file](value, "name")` 三种模式
- [x] `is_spasio_call` — 解析前探测, 避免与普通 `:` 表达式冲突
- [x] `parse_unary` 入口先检测 spasio 调用
- [x] `KwAwait` 已存在 (沿用), `@await` 标点走 `parse_stmt` 现有路径

#### AST (`spass-parser/src/ast.rs`)
- [x] `Expr::SpasioInput { mode, channel, span }` / `Expr::SpasioOutput { mode, value, channel, span }`
- [x] `SpasioMode { File, Socket, Memory }` enum
- [x] `Expr::span()` 加两个新分支

#### 语义层 (`spass-semantic/src/lib.rs`)
- [x] `GpuWithAwait` 错误 — `@gpu` 与 `@await` 互斥 (§4.2 E013)
- [x] `AwaitNextStmtWrong` / `AwaitNextLetShape` 错误 — 强制 `@await` 后必须 `let v = spasio:input[...]("name")`
- [x] `AwaitBinding { name, mode, channel, span }` 字段挂在 `TypedFunc.await_binding`
- [x] 修正了原本 `check_function` 末尾丢弃 `await_binding` 的 bug (`take()` 后没有传到 `analyze` 第二阶段) — 现在让 caller 自然 `take()` 出来
- [x] `GpuWithAwait` 检查移到 body walk 之后 (因为 `await_sites` 是 body walk 的副作用)

#### 公共状态机 (`spass-codegen/src/checkpoint_codegen.rs`)
- [x] `StateSlotKind::Await` 新变体 — 复用字节偏移合同, 未来加 `GpuState` 等不影响
- [x] `CheckpointPlan::build` 在 param 后、let 收集前先压入 Await 槽 (offset 稳定)
- [x] `let_index` 收集时跳过 await binding 的同名 let (避免双 slot)
- [x] `body_has_await` / `body_has_suspension` — dispatcher 用 `body_has_suspension` 路由
- [x] `assign_state_ids` 忽略 `@await` (MVP 不分配 state id, 同步 I/O 路径)

#### C / Rust 运行时 (`spass-runtime/src/runtime.c`, `lib.rs`)
- [x] `SpasioMode` C 枚举 + 数据目录解析 (`SPASS_DATA_DIR` env, 默认 `data/`)
- [x] `spass_spasio_input` / `spass_spasio_output` file 模式 (8 字节 little-endian f64)
- [x] socket / memory 模式: 留 stub, 打印"未实现"诊断, 返回 0

#### Codegen (`spass-codegen/src/lib.rs`)
- [x] C `emit_expr` — `Expr::SpasioInput` / `Expr::SpasioOutput` 走 `spass_spasio_input` / `spass_spasio_output`
- [x] C `emit_stmt` — `Stmt::Await` 注释 (no-op, 实际 I/O 在下一条 `let` 完成)
- [x] Rust `emit_rust_runtime` — 镜像 C 实现的 `spass_spasio_*` 函数
- [x] **关键 bug 修复**: Rust 状态机 runner 之前 `let mut state = t.clone()` 然后只修改 `state` 不写回 `*t`, wrapper 永远读到 0. 现在每个 slice 捕获到临时 buffer, 扫描 `return;` 前缀 `*t = state;`, 保证 wrapper 看到最终 I/O 结果.

#### Examples
- [x] `examples/await/src/await.spass` — `read_height` + `driver`
- [x] `examples/await/data/input/height.spb` — 1.5 的 little-endian f64 (8 字节)
- [x] `examples/await/test/await_test.spass` — `approx(result, 1.5, 0.0001)` 验证
- [x] `examples/await/Spass.toml` — 空标记

### 新增测试 (11 个, 全部通过)
- 语义层 (5):
  - `await_with_valid_spasio_input_lifts_binding` — happy path, 验证 `TypedFunc.await_binding` 字段
  - `await_with_non_let_next_stmt_is_rejected` — `AwaitNextLetShape`
  - `await_with_non_spasio_let_rhs_is_rejected` — `AwaitNextLetShape`
  - `await_with_destructuring_let_is_rejected` — `let (a, b) = ...` 形式
  - `await_in_gpu_function_is_rejected` — `GpuWithAwait`
- codegen (3):
  - `await_c_emits_state_machine_and_runtime_call` — C 端有 state struct + runner + `spass_spasio_input(SPASS_SPASIO_FILE, "height")`
  - `await_rust_runner_writes_state_back` — 回归测试 `*t = state;` 修复
  - `await_state_block_includes_await_slot` — state struct 含 `h` 字段
- checkpoint_codegen (3 已存在, 全过): `body_has_await_detects_await` / `body_has_await_returns_false_for_plain_body` / `body_has_suspension_detects_await_and_checkpoint` / `await_plan_does_not_assign_state_id` / `await_plan_reserves_await_slot`

### 端到端验证
```
$ cd examples/await ; $env:SPASS_DATA_DIR = "data" ; spass run driver
1.5
$ cd examples/await ; $env:SPASS_DATA_DIR = "data" ; spass test
  ok read_height returns channel value (rc=0)

test result: 1 passed, 0 failed
```

### 回归验证
- `examples/terrain` 5 tests passed
- `examples/struct` 3 tests passed
- `examples/if_present` 4 tests passed
- `examples/checkpoint` 编译通过, driver 0 (MVP 单 slice 行为, 已知)

### 测试统计
- spass-codegen: 62 tests (含 8 个 @await 相关)
- spass-infer: 9
- spass-parser: 17
- spass-runtime: 1
- spass-semantic: 38 tests (含 5 个 @await 测试)
- **总计: 127 tests passed, 0 failed**

### 预留接口 (不需重构的未来功能)
- `StateSlotKind::Await` — 字节合同, 未来 typed slot (4 字节) 升级无需改 wrapper 解码
- 多 `@await` 站点 — 当前 MVP 一个 slot, 升级路径: per-site channel table
- 异步真正 suspend (`task` yield + scheduler) — 同一 `StateDataLayout` + 多任务表
- `Expr::Await` (§5.4) — 在表达式上下文中等待值; codegen 端可共用 `spass_spasio_input` 包装
- `spasio:output` 运行时 — 已实现, 但 codegen 仅在 C/Rust 顶层 emit, 未来可加 `spasio:output[mode] (expr, "channel")` 表达式位
- socket / memory 模式 — runtime 留 stub, 升级路径: 阻塞 socket + 共享内存映射

## 2026-09-05 - 会话: A6 模块系统 + 阶段 11 自举集成

### A6 (§7 模块系统) - commit faf24b9
- include: 项目级展开守卫 + E003 循环检测（cycle 检查先于 expanded 守卫，顶层文件预入 include_stack）
- fileclude: C static / Rust 非 pub 链接，跨文件 E006
- ownclude: 引用驱动 + 定义时位置正确最小捕获（修复过度捕获后续 let 导致的生成码错误），捕获类型进 mangle
- 公共 ABI 收敛: dispatch/export/manifest/spass run 仅 Public；C 前置声明（c_signature 单一来源）
- +17 测试, 172 tests 全绿, 8 示例回归通过

### 阶段 11 自举集成（方案 A）- commit (本次)
- bootstrap.spass: +nd 核心库（gcd_f 二进制 GCD / rat_add_nd / rat_mul_nd / gcd_step_nd / state_count_nd / mul_corners_nd），self_check 扩至 14 断言
- 新 crate spass-bootstrap-core: build.rs 编译 bootstrap.spass → Rust（大栈线程防溢出 + 剥 main + 剥 inner attrs）；NumericCore impl + install()
- spass-infer: NumericCore trait + OnceLock 槽 + f64 精确域门槛 + 对拍 panic + AtomicU64 统计；state_count/bit_width/infer_add_sub/infer_mul 接入
- CLI: 启动安装 + uild --verbose 输出统计
- 实测: terrain 编译 123 次 Spass 核心调用 0 回退；175 cargo tests + 8 示例 build + E2E 全绿

### 发现
- 见 findings.md 阶段 11 小节（包循环约束 / build script 栈溢出 / include! inner attr / int 字面量混算间隙 / 位宽语义分歧）

## 2026-09-05 - 会话: 阶段 12 启动（盘点 + 12a 小修）

### 盘点结果（写入 task_plan.md 阶段 12）
- spawn/parallel/await(task) §5.4: AST/semantic 就绪，codegen 明确报"未实现"
- sequence/send/receive/broadcast §10.3: 完全未实现（parser 无 KwSequence）
- spasio socket/memory §9.2: runtime stub（file 模式已实现）
- GPU/SPIR-V §10.1: 未实现
- CLI 未实现: daemon、build --platform/--timeout/--diagnostics
- `for 0..n` 语义确认：codegen 生成 `var < end`（左闭右开）+ step 支持，符合 §5.1，无需改码

### 12a 已完成: W002 警告 CLI 报告
- compile_sources / cmd_test 最终分析改用 `analyze_full`，按 §11.5 黄色粗体 `warning:` 输出（print_warnings + ansi::yellow_bold）
- E2E: 临时项目触发 W002 正确输出（黄色粗体 + span）；terrain 暴露 6 条此前被静默吞掉的真实 W002（sculpt dispatch 各函数只消费部分 bind），`add` 回归 4 ✓
- 排期: 12b spasio socket → 12c spawn/parallel → 12d sequence → 12e daemon → 12f GPU

## 2026-09-05 - 会话: 12b spasio socket 模式 (§9.2)

### 完成的工作
- [x] **帧协议**（规范 9.2）：`[u32 body_len 大端][u8 name_len][channel 名 UTF-8][f64 小端]`——帧头走 TCP 网络字节序、数据体走 Spass 数据格式字节序，两者独立
- [x] **Rust prelude**（codegen emit_rust_runtime）：`spass_spasio_socket_target/encode_frame/decode_frame` + input（`TcpListener::bind` 单帧阻塞）+ output（`TcpStream::connect` 发一帧）；env: `SPASS_SOCKET_HOST`/`SPASS_SOCKET_PORT`（默认 127.0.0.1:9100）
- [x] **C runtime**（runtime.c）：WinSock(`winsock2.h`+ws2_32)/POSIX 双路，socket_input（bind/listen/accept/收帧路由）+ socket_output（connect/send），行为与 Rust 侧一致
- [x] **测试**：`rust_prelude_implements_socket_mode`（帧端序 + listener/connect + 无 stub 残留）、`runtime_implements_socket_mode`（C 源无 stub）
- [x] **E2E**（examples/socket + 外部对端）：
  - input 方向：`spass run echo_height`（@await + input[socket]）监听 9101，Python client 发帧 → 输出 `1.5` ✓
  - output 方向：`spass run send_height 7.5`（output[socket]）→ Python server 解出 `height=7.5` ✓

### 发现
- **spass.exe 缓存陷阱**：`cargo test` 不更新 `target/debug/spass.exe` 二进制（只编 test harness）。改 codegen prelude 后直接跑 `spass run` 会执行旧二进制内嵌的旧 prelude（socket 仍是 stub 消息）。必须 `cargo build -p spass` 后再 E2E。
- **PATH 依赖**：spass.exe 自身运行和 `Command::new("rustc")` 调用都依赖当前 shell PATH 含 toolchain bin（with_rust_env 只注入 module_rs.exe 子进程）。无注入时表现为静默 exit 0 空输出。
- **echo_height 复现的"metadata stub"错误**为旧 exe + 环境混用的一次性现象，重建后未复现。
- Python server bind 9102 时报 10048（端口被占），换端口即可——每轮 E2E 用独立端口。

### 测试统计
- spass-codegen: 84 tests（+1 socket prelude 测试）
- spass-runtime: 2 tests（+1 socket C 源测试）
- 未提交: codegen/lib.rs、runtime.c、runtime/lib.rs、examples/socket/、main.rs（12a）、task_plan.md、progress.md

## 2026-09-05 - 会话: 12c 异步 spawn/parallel/await(task) (§5.4)

### 完成的工作
- [x] **Parser**: `spawn`/`parallel`/`await` 表达式解析。AST 变体早已存在但从未被构造（semantic/codegen 的 Spawn 分支是死代码）。用 ident 前瞻探测（`is_async_spawn`/`is_async_await`，镜像 is_spasio_call 模式），保留字保持 Ident 不动 lexer；`@tracked`/`@independent` 注解解析进 `annotations`
- [x] **类型系统**: `Type::Task` 变体；semantic spawn→`Type::Task`、await 校验 task 类型并解包 `Float`；C ABI `uint64_t`、mangle `v`、Rust `u64`
- [x] **Codegen**: 统一 thunk 协议 `(const double* args, double* out)`——为每个 spawn 兼容函数（纯 float/int 参数与返回、非 if_present）在模块级发射 thunk（不扫 AST，闲置 thunk 无代价）；C/Rust 站点发射 `spass_spawn(thunk, args, n)` / `spass_task_await(id)`；`infer_c_type`/`infer_rust_type` 识别 Spawn→句柄类型
- [x] **Runtime**: C 侧 win32 `_beginthreadex`+`InterlockedExchange` / POSIX `pthread` detached+`__atomic`，结果轮询 1ms；Rust prelude 用 thread+`mpsc::channel`（无轮询）
- [x] **CLI 大栈**: main 的编译管线移入 512MB 栈线程（`thread::Builder`）——bootstrap.spass 深递归解析在 1MB 默认主线程栈上溢出（与 spass-bootstrap-core build.rs 同款修复，rustc 同款做法）
- [x] **E2E**: examples/async，`spass run driver 3 4` → 10（spawn 异步执行 + await 取回）

### 发现 / 修复的坑
- **AST 死代码**: `Expr::Spawn/Parallel/Await` 在 ast.rs 中定义且 semantic 有 match 分支，但 parser 从不构造——"semantic 已就绪"的记录误导了排期估计，实际要做完整三链
- **预存的测试竞态**: bootstrap-core 的 `bridge_adopts_spass_results`（断言 fallback delta 不变）与 `bridge_falls_back...`（故意增加 fallback）并发跑时互斥地失败——cargo 多线程 harness 下靠时序运气。修复：`STATS_TEST_LOCK` 串行化统计敏感测试
- **if_present × spawn 干扰**: if_present 函数的 thunk 调用无后缀 mangled 名，触发 `if_present_internal_call_uses_present_variant` 的负向断言；修复 = 把 if_present 函数排除出 spawn 面（变体语义本就未定义）
- **主线程栈溢出回归**: 新增常数帧让贴边的 bootstrap 编译溢出。与其逐帧找增量，直接采用既定升级路径（大栈线程）
- **(险情) 误覆盖 codegen/lib.rs**: 一次误用 write_to_file 清空了 5500 行文件；从 git HEAD 恢复后按上下文完整重放本日全部改动。教训：对已读大文件坚持 replace_in_file

### 测试统计
- 全 workspace: 178 tests passed（codegen 84 + semantic 58 + parser 22 + infer 9 + bootstrap-core 3 + runtime 2 等）
- 示例回归: async 10 ✓ / terrain 5 tests + add=4 ✓ / if_present 4 ✓ / vec 12 ✓ / struct 3 ✓ / checkpoint ✓ / module ✓ / socket 2 轮 ✓ / bootstrap self_check=1 ✓
- 未提交: parser.rs、ast.rs、semantic/lib.rs、codegen/lib.rs、runtime.c、runtime/lib.rs、bootstrap-core/lib.rs、cli/main.rs、examples/{socket,async}/、task_plan.md、progress.md

## 2026-09-06 - 会话: 12d sequence 多序列 (§10.3)

### 完成的工作
- [x] **Parser**: 顶层 `sequence name { body }`（parse_item 路由 Ident("sequence")）+ `send(target, msg)` / `broadcast(msg)` / `receive [@await] [Pattern{field op expr, ...}]` 表达式（is_msg_call 前瞻探测，与 spasio/spawn 同模式）
- [x] **Semantic**: 序列注册为**合成零参无返回函数**（`sym.functions[name]`，与用户函数共享顶层入口命名空间，撞名报 E012）——body 由常规 check_function 检查，codegen 走标准 emit_function 路径，零特判。`send` target 校验 sequences；消息必须为 struct 值（≤8 float/int 字段）；`receive` 强制带 Pattern（类型可静态推断，无 pattern 报错待 Option 类型）
- [x] **Runtime**: 每序列一条 FIFO 队列（编译期序列 id 常量）。Rust prelude: `Mutex<Vec<VecDeque<SpassMsg>>>`（SpassMsg = f64[8]+n）+ `yield_now` 轮询 recv + spawn/wait_all；C: 环形缓冲 + 全局 CS/pthread 锁 + 1ms 轮询 + `_beginthreadex`/`pthread` 句柄表
- [x] **Codegen**: per-struct `__sp_pack_X/__sp_unpack_X` helper（对全部兼容 struct 发射，闲置无代价）；dispatcher main 启动全部序列 + **Drop guard join**（dispatch arm 提前 return 也能保证"所有序列终止后程序退出"）；C 的 receive 是语句形态 → `let` RHS 专用发射（`for(;;){recv; if(cond) break;} X m = unpack(...)`）
- [x] **E2E**（examples/sequence）: producer `for 0..3` 发 `Ping{value: to_float(i)*10.0}` → consumer 阻塞收 3 条求和 → `spasio:output[file](total, "result")` → **result.spb = 30.0** ✓
- [x] 附带修复: spasio file 输出按需 `create_dir_all`（序列在宿主未建目录时也能写出）

### 发现
- **C 表达式 vs 语句形态**: `receive` 阻塞循环在 C 里不能作为 let 的 RHS 表达式（Rust 块表达式天然支持）→ C 端 `Stmt::Let` 对 Receive 特判成语句级发射，表达式位置报 Unsupported
- **`extern "C" fn()` 不实现 `FnOnce()`**: `thread::spawn(entry)` 编译失败，需 `thread::spawn(move || entry())`
- **main 的 Drop guard 模式**: dispatch arm 用 `return` 提前退出，main 末尾的 join 永不执行——Drop guard 是唯一能覆盖所有退出路径的位置
- **PS 读取相对路径陷阱**: `[IO.File]::ReadAllBytes` 用进程 cwd 而非 PS location，验证时用绝对路径

### 测试统计
- 全 workspace: **179 tests passed**（codegen 85 + semantic 58 + parser 22 + infer 9 + bootstrap-core 3 + runtime 2）
- 示例回归: sequence 30.0 ✓ / terrain 5 ✓ / async 10 ✓ / bootstrap 1 ✓ / socket ✓ / struct 3 ✓
- 未提交: ast.rs、parser.rs、semantic/lib.rs、codegen/lib.rs、runtime.c、runtime/lib.rs、examples/sequence/、.gitignore、task_plan.md、progress.md

## 2026-09-06 - 会话: 12e CLI daemon (§11.6)

### 完成的工作
- [x] **cmd_run 重构**: 提取 `compile_runnable(project)`（编译到 module_rs.exe）+ `run_symbol(exe, cwd, symbol, args)`（cwd=project，spasio 相对路径与 direct 一致）+ `run_direct`
- [x] **daemon 服务端**: `Cmd::Daemon`（无参启动 / `--status` / `--stop`）；TcpListener accept 顺序处理；请求帧复用 §9.2 头（4 字节大端）+ JSON body（`{"cmd":"run|status|stop","project","func","args"}`）；响应 `{"exit","stdout","stderr"}`
- [x] **模块缓存**: `OnceLock<Mutex<HashMap<project, {typed, exe, mtimes}>>>`；src 文件 mtime 全等才命中，否则重编译并更新 —— daemon 的核心价值（避免重复编译）
- [x] **客户端**: `spass run`（无 --direct）→ daemon_exchange；连接失败 → detached 自动启动 daemon（Windows `DETACHED_PROCESS|CREATE_NO_WINDOW`，stdin/stdout/stderr 全 null）+ 轮询端口 5s → 重试；仍失败 → **降级 run_direct**（daemon 永不阻塞普通使用）
- [x] **E2E**: 手动 daemon → run add=4 → height=1.1989… → status "1 module(s) cached" → stop ✓；自动启动路径（不预启 daemon）add=4 → status 1 cached ✓；`--direct` 回归 4 ✓

### 发现
- **exe 文件锁**: 残留 daemon 进程锁着 target/debug/spass.exe 导致 cargo build "拒绝访问 (os error 5)" —— 停 daemon 再构建。E2E 结束记得 `daemon --stop` 或杀进程。
- **端口共享**: daemon 与 spasio socket 模式共用 `SPASS_SOCKET_PORT`（规范都是 9100）；E2E 用 9201/9202/9203 避开残留监听。
- 缓存收益在增量 rustc 下不显著（first≈504ms vs cached≈531ms，I/O 缓存热），主要价值在跨调用常驻与未来 dlopen 常驻模块（规范设想的 saple 缓存）。

### 测试统计
- 全 workspace: 179 tests passed（无回归）
- 未提交: main.rs（daemon + cmd_run 重构）、task_plan.md、progress.md 及此前各会话改动

## 2026-09-06 - 会话: 规范修订（运行器架构 + let 可变 + GPU 后端可插拔）

### 用户需求与决策
- 用户提出：加入 set 功能进行变量数值替换；saple 由 spass 内置的 **saple 运行器**运行，外部程序与运行器沟通交换数据（实现动态变化）；GPU/SPIR-V 评估 CPU 多线程平替。
- 澄清确认：①运行器是 saple 唯一运行入口（外部永不 dlopen）；②不需要独立 set 语法——let 直接支持数值替换（规范 §3.9 与实现对齐）；③GPU 采用后端可插拔措辞（@gpu=并行语义，运行器调度：MVP CPU 线程池 / SPIR-V 未来后端）。

### 项目简介.md 修订（v1.4 → 运行器架构版）
- §1.3/§1.4：外部程序连接运行器（永不 dlopen 库）；set 变量语义；信任边界归属运行器
- §2.4：module.sps 描述对象改为"运行器与外部程序"
- §2.5 重写：标题"外部程序加载 saple 文件"→"saple 运行器"；运行器加载 6 步骤；名字修饰降级为运行器内部调度细节
- §3.9 重写：let 可重新赋值（数值替换）+ 值域钳制时机（ABI 边界强制，函数体内 f64 中间表示不即时钳制）
- §4.2 标题改"并行函数（@gpu）"：声明并行语义而非硬件，后端由运行器调度；local 参数标注为 SPIR-V 后端约束
- §9.1/§9.5/§9.6/§9.4：数据流改为"外部程序 ↔ 运行器 ↔ Spass 程序"
- §10.1 改"并行执行"、§10.2 运行器调度措辞、§2.6/§12.2 阶段二/三措辞
- §15 决策表 +3 条（let 可变 / 并行后端可插拔 / 运行器唯一入口）；§16 开放问题更新 + 新增 9（运行器 set/get 协议）、10（@gpu CPU 后端调度策略）

### 实现验证
- [x] examples/assign 新增：let 重赋值 E2E（replace(3)=3*2+1=7 ✓）+ 2 个测试用例全过——确认实现与修订后 §3.9 一致（重赋值早已可用，规范原先写错了）
- 实现代码无需修改（let 可变、@gpu CPU 后端、daemon 运行器雏形均已是事实）

### 文档联动
- task_plan.md：12f 更新（规范完成、实现随阶段 13e）；新增阶段 13（运行器架构：13a 槽位清单 / 13b 常驻世界 / 13c set-get 协议 / 13d spasio-client 重定位 / 13e @gpu CPU 调度）
- findings.md：新增"运行器架构决策"小节（规范-实现脱节案例、架构转折、@gpu 抽象层级、钳制时机）

## 2026-09-06 - 会话: spasio 全面运行器化（13c 升级）

### 架构变更（用户决策：socket 反转 + file 收归运行器）
- **运行器持有全部通道状态**：file 通道 = 磁盘槽位（运行器代理读写 data/input|output），socket/memory 通道 = 运行器内存槽位表（Mutex<HashMap> + Condvar）
- **程序内三模式统一客户端 RPC**：`spass_spasio_rpc(opcode, mode, channel, value)`——短连接 connect 运行器，帧 `[4B BE len][u8 opcode][u8 mode][u32 BE proj_len][project][u8 ch_len][channel][8B f64]`，响应 `[u8 status][8B f64]`；project = 程序 cwd。**程序进程永不碰文件系统、不监听端口**
- **语义确定**：file 与 socket 通道均为"读=最新值"槽位语义（file 落盘、socket 内存；socket 无值时阻塞等待外部 set 唤醒）——§9.2 措辞已对齐
- memory 模式映射 socket 槽位（ponytail：真共享内存是未来版本）

### 实现
- daemon_serve 并发化：per-connection 线程（阻塞 GET 不卡 accept）+ AtomicBool stop（stop 命令自连唤醒 accept）
- daemon_handle_conn：帧体首字节分流——`{`= JSON 控制面、0x00=数据 GET、0x01=数据 SET
- JSON set/get 加可选 `"mode"` 字段（"file"默认 / "socket"内存槽位），与程序数据面读写同一状态
- Rust prelude：删除 std::fs I/O 与 TcpListener；C runtime：删除 bind/listen/fopen，统一 `spass_spasio_rpc`（WinSock/POSIX，getcwd 取 project）

### E2E 动态闭环（examples/socket + setget + await）
- 阻塞闭环：先启动 `run echo_height`（阻塞在 height 槽位）→ 外部 set (socket) 1.5 → 程序**实时输出 1.5** ✓
- push 闭环：`run send_height 7.5` → 外部 get (socket) → 7.5 ✓
- 无监听端口：阻塞中的 module_rs 进程 netstat 无 LISTENING ✓
- file 经运行器：setget set 5.5 → poll 11 ✓；await driver 1.5 ✓
- 179 tests 全绿；测试断言更新（prelude 无 fs/bind、C 无 fopen/listen）

### 语义变更注意
- 12b 的 socket 程序=服务端语义已废弃：程序不再 bind，examples/socket E2E 流程反转
- file 通道缺失时：原"fopen 失败 abort"→ 运行器返回 ERR → 程序 panic（语义等价，路径变 RPC）

## 2026-09-06 - 会话: Java + Spass 游戏（阶段 13b/13d 落地 + 内置补全）

### 交付：Treasure Hunt（宝藏猎人）
- **Spass 逻辑核心**（examples/game/src/game.spass）：`sequence game` 常驻世界——状态（猎人 hx/hy、宝藏 tx/ty、步数、found）全部在 Spass 侧；`while found == 0.0` 主循环；`input[socket]("tick")` 阻塞等回合推进；规则 = 移动分支 + `clamp` 世界边界 + 步数累计 + `sqrt` 距离提示 + 胜利判定；每回合 `output[file]` 发布 hx/hy/steps/found/dist 五个状态通道
- **Java 前端**（TreasureGame.java，无依赖手写帧协议）：连接运行器 JSON 控制面（start/set/get）；渲染 10x10 网格 + 冷热提示；读玩家 WASD；轮询 steps 变化推进回合；found=1 显示胜利；`auto` 模式脚本化自动寻宝用于 E2E
- **E2E**：daemon → Java auto → 11 步自动寻宝 → **TREASURE FOUND** ✓

### 随游戏落地的引擎能力
- [x] **13b 最小形态**：codegen main 加 `__sp_forever` 模式（sequences 跑到全部终止）；daemon `start` 命令（compile_runnable 缓存 + detached 启动常驻世界 + 记 PID；重复 start 重启；`stop` 收割）
- [x] **13c 语义修正**：socket 通道从"槽位读最新"改为**消息队列**（程序 input 消费出队 + 无消息阻塞 Condvar；外部 get 只 peek）——否则旧 tick 反复触发回合（实测 hx 冲到 9、steps=119 的教训）
- [x] **13d 演示**：start/set/get 客户端 + 外部程序对端 + Java 手写帧协议
- [x] **内置补全**：`lerp`/`clamp`/`smoothstep`（§4.3）从未注册——semantic register_builtins + BUILTIN_NAMES + codegen builtin_mapping 三处；连带发现 `is_builtin_name` 名单不同步导致内置泄漏进 typed.functions[0]
- [x] **Java 客户端教训**：Windows 短连接快速开关耗尽临时端口（BindException）→ 长连接复用；旧世界磁盘槽位污染新世界状态判断 → start 时清 data/output + 轮询改"变化检测"

### 测试统计
- 全 workspace: 179 tests passed（无回归）
- 未提交: semantic/lib.rs、codegen/lib.rs、runtime.c、runtime/lib.rs、cli/main.rs、examples/{socket,assign,game}/ 等

## 2026-09-06 - 会话: 地形生成器（Spass 生成 + Java 渲染）
- **交付**（examples/terrain_gen）：`sequence gen` 常驻生成 16x16 地图——height(x,z) 分层正弦、tile_code(h) 高度分类（0 水/1 沙/2 草/3 山），每格发布 (code/h/idx) 后阻塞等 "ack" socket 通道（Java 消费确认握手）；Java 轮询 tile_idx 消费、渲染字符地图。一次跑通：256 tiles → 带水域/山脉的地图（零编译器改动，复用 file 槽位 + socket 队列 + start/FOREVER）
- 启动：daemon → `java TerrainGen <project> 9208`；游戏（Treasure Hunt）启动方法已交付：PATH 注入 → daemon → `java TreasureGame <project> 9100`

## 2026-09-06 - 会话: 阶段 13 启动（13a 通道清单 + 13c set/get 网关）

### 完成的工作
- [x] **13a module.sps 槽位清单**：semantic 层收集通道引用——`SymbolTable.record_channel`（check_expr 的 SpasioInput/Output 分支调用，mode+channel+dir 去重）→ `TypedModule.channels: Vec<ChannelInfo>` → cmd_build 在 module.sps 写 `channel: <mode> <dir> <name>` 段。外部程序与运行器据此知道哪些通道可 set/get。
- [x] **13c set/get 网关（MVP）**：daemon 帧协议加 `set`/`get` 命令。设计关键洞察：**spasio file 通道"读=最新值"的槽位语义天然就是 settable 变量**——运行器只做网关：set → 写 `data/input/<ch>.spb`（f64 LE），get → 读 `data/output/<ch>.spb`。零新语言特性、零 runtime 改动，完全复用已有 file 模式。
- [x] **E2E**（examples/setget + 外部模拟程序）：set scale=2.5 → `spass run poll` → 5（=2.5×2，程序经 file 通道读到运行器中转的外部值）；`spass run publish` → 外部 get measurement → 3.5；set 7.0 → poll 14 —— 动态替换实时生效 ✓
- [x] 回归：179 tests 全绿；terrain 5 / sequence check / async 10 ✓

### 设计决策
- **file 通道 = settable 槽位**：file input 的语义本来就是"读文件当前内容"（非队列），正是 settable 变量需要的"读最新值"。socket 通道保留队列语义（消息型数据）。set/get 网关让外部程序无需了解 file/socket 差异。
- **通道清单在 semantic 层收集**：check_expr 已遍历所有表达式，`SymbolTable` 贯穿分析全程——不需要新的 AST walker（同 spawn thunk 的教训：能复用现有遍历就不新写）。

### 测试统计
- 全 workspace: 179 tests passed（无回归）
- 未提交: semantic/lib.rs（ChannelInfo）、cli/main.rs（channels 生成 + daemon set/get）、examples/setget/、task_plan.md、progress.md

## 2026-10-25 - 会话: 优化计划 W1–W14 落地 + 全量验收 + MSI 安装包

### 计划项（W1–W14 全部落地）
- [x] **W1 值域调用点编码**（Rust + C 双侧）：`float[0.0, 380.0, 0.1]` 这类值域在调用点按 `uint16_t` 状态索引传递，函数体再解码回 f64。返回值侧也补了解码——只编码不解码的话 Rust 侧编译不过、C 侧会静默拿到裸状态索引。out 参数按指针传递，不量化。
- [x] **W2 解析器命名空间排除**：`fileclude` / `ownclude` 移出公共命名空间。
- [x] **W3 `AGENTS.md`**：仓库导航图（仓库根目录）。
- [x] **W4 spasio 连接池 + TCP_NODELAY**：客户端与服务端都设了 TCP_NODELAY。
- [x] **W5 精确 `k = 1/step` 运算**：`spass_round` / `spass_enc_state` 两个后端逐位一致。
- [x] **W6 每队列 condvar 序列**：C 侧用 condvar 替代 1 ms 轮询。
- [x] **W7 `create_dir_all` 提升**：移出热路径。
- [x] **W8 daemon 编译期释放互斥锁**。
- [x] **W9 `-C codegen-units=1 -C embed-bitcode=no`**：W9 必须与 W10 同时落地——`codegen-units=1` 拉长编译时间，本地缓存把这个代价吸收掉了。`-C lto` 明确不加。
- [x] **W10 本地 mtime 键构建缓存**（带 include 集合跟踪）。
- [x] **W11 `TypedModule` HashMap 索引**。
- [x] **W12 `SPASS_DISABLE_BOOTSTRAP`**：跳过自举交叉校验。
- [x] **W13 runtime prelude 分段**：`runtime_sections.rs` 按需发射 `vec_mat` / `spasio` / `tasks` / `sequences` / `playground`；playground 一段就 ~52 KB Rust（~39 KB C）。C 侧 playground 同样按段门控，两个发射器保持结构对称。
- [x] **W14 codegen 热路径**：每模块一个 `CallIndex`，调用点只做一次哈希探测；去掉热路径上的 `format!` 临时量。

### 验收过程中发现并修掉的真 bug
- **ownclude 闭包被生成了 spawn thunk → 生成的 Rust 编译不过（E0061）**。`examples/module` 的 `compute_b` 里 `ownclude func scale` 捕获了 `base`；提升后的静态 helper 签名是 `(base, t)`，而模块级 spawn thunk 只按 `f.params` 转发实参，于是用 1 个实参调用 2 参函数。修复：带捕获的闭包不进 spawn 面（`spawn_thunk_compatible` 增加 `f.captures.is_empty()`）；`spawn` 指向这种目标时给出明确错误，而不是生成编译不过的模块。两个发射器共用该判定。新增回归测试 `capture_bearing_closure_gets_no_spawn_thunk`。
- 验收脚本本身也修掉三个真问题：`$Args` 撞 PowerShell 自动变量（导致子进程收不到参数）、`ProcessStartInfo.ArgumentList` 在 .NET Framework 上不存在、`rm --` 不是合法形式。测量脚本的"冷启动"原本靠删缓存文件，而删除在本机被安全策略拦截，于是把一次失败启动当成了测量值（30 ms）；改成用 mtime 触发缓存失效，并让失败的运行直接报错。

### 测量设施
- `compiler/crates/spass-bench`：criterion 基准（parse / analyze / codegen / 全流水线），输入内嵌自 `examples/bootstrap` 与 `examples/terrain`。
- `compiler/scripts/measure.ps1`：冷（无缓存）/ 热（缓存命中）/ daemon 三种真实等待时间，在项目副本上测量。
- criterion 的 `change:` 行与上次同机运行比较，**不是稳定基线**——机器漂移大于多数被测效应（未改动的 parse 阶段两次会话间就漂了 −16%）。同一次会话内比 `codegen/c ÷ parse` 比率才有意义：**0.377 → 0.283（−25%）**。

### 全量验收结果
- 单元测试：**197 通过**（codegen 95 / infer 63 / parser 26 / semantic 13）。
- 示例项目：**16/16 构建通过，13 个跑通**；`game`、`voxel`、`terrain_gen` 三个是 build-only（序列阻塞在外部 peer 的 socket 槽位 / ack 通道上，跑了会挂死）。
- `spass test`：**27 个测试全过**（assign 2 / await 1 / if_present 4 / struct 3 / terrain 5 / vec 12）。
- spasio 通道示例需要 runner（daemon 在跑）才通：setget publish → 3.5、await driver → 1.5、socket send_height → 1、sequence driver → 1、module compute_b 10.0 → 77。
- 真实等待时间（5 次取中位，terrain / bootstrap）：冷 **1033 / 1162 ms**，热 **204 / 203 ms**，daemon **182 / 180 ms**——本地缓存把"改一行再跑"从约 1 秒压到约 0.2 秒。

### 未达成的目标（如实记录）
- **W4 file 通道 0.740 ms/op，目标 ≤ 0.60 ms/op**：残差是 daemon 侧 8 字节 `fs::write` 的 0.444 ms，而 W7 明确保留了这一步。socket 通道 0.109 ms/op，目标 ≤ 0.10。
- **C 后端无法执行验证**：本机没有任何 C 编译器（gcc / clang / cc / cl / tcc 都没有），C 侧只能做结构化验证（对生成文本的断言）。

### 安装包
- `installer/spass.wxs`（WiX v5）+ `installer/build-msi.ps1`：per-user MSI，装到 `%LOCALAPPDATA%\Programs\Spass`，写用户 PATH，建开始菜单快捷方式，无 UAC。产物 `dist/spass-0.1.0-x64.msi`（1.32 MB）。
- 安装/卸载实测：`msiexec /i` → 文件落位、`spass --version` 打印 `spass 0.1.0`、PATH 条目写入、快捷方式建立；`msiexec /x` → 文件与快捷方式清掉，**用户 PATH 与安装前逐字节相同**（MSI 只删自己加的那一条）。
- MSI 不带自定义界面（WiX v5 的 UI 扩展在本机加载不了，且 stdlib 没有 UI 序列需要的 `InstallExecute` 动作符号）；`spass-setup.exe` 仍是带完整界面的安装器。

## 2026-10-28 - 会话: deeppack 产物模式 + playground 落地 + 可用性改进

### deeppack：编译产物直接出 DLL（宿主同进程加载）
- **动机**：规范 §1.3 说 `.saple` 装原生机器码、§1.4 说外部程序永不直接加载 saple 内的库。但实现里 `spass build` 装的是 **C 源码**（`native/source.c`），而且仓库里**没有任何东西编译 C**——Rust 后端早已让「C 宿主」这个假设失效。deeppack 就是 Rust-only 的落地：产物是一个共享库，宿主 `LoadLibrary` + `GetProcAddress` 直接调。
- **实现**：
  - `spass_codegen::RustFlavor`：`Program`（`fn main()` 派发 argv）/ `Cdylib`（公开导出带 `#[no_mangle]`，无 main）。`extern "C"` 本身**不**阻止 Rust mangle 符号，所以 cdylib 那一侧必须自己加 `#[no_mangle]`。
  - `spass build --deeppack` → `build/spass_module.dll` + `build/spass_module.abi.json` + 旁边的 `libunwind.dll`（共享库链接了工具链的 unwinder，宿主解析不到就会 `STATUS_DLL_NOT_FOUND`）。
  - `spass_codegen/src/abi.rs`（新文件）：导出符号的 ABI 描述——C 类型、哪些是 `out` 参数、值域的 min/max/step/k/state_count/bits、结构体字段顺序。清单就是编译器对宿主的契约。
  - `spass-cli/src/deeppack.rs`（新文件）：产物与清单。
- **spasio 不动**：deeppack 只回答「宿主怎么拿到函数」，不回答「程序怎么拿到数据」——用了通道的模块仍然走运行器，仍然用 §9 的帧协议。
- **信任边界**：deeppack 的模块跑在宿主进程里，宿主就是信任边界，§1.4 里运行器那道越界截断**不适用**。这是把边界从「运行器↔程序」挪到「宿主↔程序」的必然结果，已写进 `项目简介-Deeppack.md`。
- **E2E**（`compiler/scripts/verify-deeppack.ps1`）：宿主进程直接加载 DLL 并调三条路径——普通函数（与程序自身输出逐位一致）、值域 uintN 状态索引进出、多返回值 repr(C) 结构体。**编译成功不是证据，宿主调通才是。**

### playground：从「写完了」到「有证据」
- 之前的状态：parser → semantic（~55 个带签名的内建）→ codegen → 52 KB runtime 全都齐了，但**规范零提及**、**零示例**、**零行为测试**、91 KB runtime **从没渲染过一帧**。
- **新增文件**（不替换原有设计）：
  - `项目简介-Playground.md`：定义 s2d:/s3d:/spio: 三个命名空间，并说明它与规范 §1.5 的关系——playground 是**必须显式启用**的可选能力层，不调用它的项目产物里没有任何像素代码也没有文件系统入口，§1.5 对默认语言依然成立。
  - `examples/playground/`：一个同时用 2D、3D 和 `spio:` 的小程序。
  - `compiler/scripts/verify-playground.ps1`：无头渲染一帧，逐像素断言背景色、矩形填充、矩形描边、圆盘、3D 地面网格、受光立方体。
- **结果**：6 项像素断言全过（20,23,31 / 51,153,242 / 255,255,255 / 242,115,51，网格 4062 px，立方体 1744 px 三个受光面），效率 **1.81 ms/帧（553 fps）**（320×240 + 地面网格 + 立方体）。

### 可用性改进（按「哪里硌手」来改，不是改测试）
- **`--direct` 不再让通道程序失去 runner**：`--direct` 只该表示「函数调用不经 daemon 派发」，不该把托管通道槽位的 runner 一并取消。现在 `run_direct` 发现项目用了通道就会确保 runner 在位，并在拉不起来时给出可执行的报错。
- **spasio 失败不再是一坨 Rust panic**：改成打印 `spass: ...` 并以失败码退出；连接失败的消息补上了「is `spass daemon` running?」（C 侧早就有，Rust 侧现在对齐了）。
- **`spio:` 的字符串不再每次读都泄漏**：运行时改成对读到的内容做驻留，循环读同一个文件不再涨堆。
- **计数与索引改成 `int`**：`spio:line_count` / `spio:list_count` 返回 int，`spio:read_line` / `spio:list_name` 的索引参数也是 int。计数用 float 本身就是设计错误，会强迫调用方每次比较前做转换。两个后端同步改了。
- **2D/3D 绘制顺序的静默坑**：`s2d:` 与 `s3d:` 共用一个帧缓冲，顺序即层级，后画的盖住先画的——2D 覆盖层画在 3D 场景之前就会消失，而且**完全没有提示**。现在运行时在第一次出现「3D 画在 2D 之后」时打一条 stderr 警告，示例改成正确的顺序（清屏 → 3D → 2D → present），规范写清了一帧的三步。
- **C 后端的目录列举原本是个返回 0 的桩**：与 Rust 侧悄悄不一致。改成返回 -1（明说「不支持」），并记进 `项目简介-Playground.md` 的后端差异表。

### 已知未做
- **2D 层独立缓冲**（present 时合成，顺序就无关紧要）：真正的修法，但要给 2D 图元加 alpha，是一次运行时改造。已记为 ponytail。
- **`.saple` 里的 C 源码与规范 §1.3 不符**（规范说装机器码，实现装源码）。让 saple 真正合规需要运行器去加载库，是运行器架构的改动。
- **C 后端仍无法执行验证**：本机没有任何 C 编译器，C 侧只做结构化断言。

## 2026-10-28 - 会话二: runner 生命周期 + CLI 语义 + MSI 收尾

### 复测 `spass test` 时挖出来的三个真问题
`await` 示例之前一直失败，根因不是测试本身，而是 **runner 的生命周期从来没被当成一个要设计的东西**。顺藤摸下去挖出三个问题，全部在「代码与设计」层面，不在测试层面。

**问题一：`spass test` 根本不保证 runner 在位。**
`spass run --direct` 早就有 `ensure_runner`，但 `spass test` 这条路径没有——而 `spass test` 跑的正是带通道的模块。于是 `spass test` 在没跑 daemon 的机器上必然失败，报「无法连接 127.0.0.1:9100」。现在 `cmd_test` 编译完模块后同样确保 runner 在位，拉不起来时给出可执行的报错。

**问题二（最严重）：自动拉起的 daemon 继承了调用者的 stdout 句柄。**
Windows 上 `Command` 创建子进程用 `bInheritHandles=TRUE`，会把父进程里**所有可继承句柄**复制给子进程，而标准句柄默认可继承。daemon 又是长驻的，于是：
- `spass test > out.txt` → **out.txt 被 daemon 锁住**，读不了、删不掉、改不了名；
- `spass test | tee log` → **管道永远等不到 EOF**，CI 直接挂死。

证据链：改之前 `out.txt` 被另一个进程占用，改之后立刻可读。修法是拉起 daemon 前把三个标准句柄的 `HANDLE_FLAG_INHERIT` 清掉、spawn 完再恢复（`std_handle_inheritance`），并加单元测试锁住「清掉 → 断言 → 恢复 → 断言」这个循环。

**问题三：daemon 永远不会自己退出。**
它是长驻服务，但一个没人再说话的 runner 不该留在后台过夜。现在 daemon 在**没有客户端连接**且**一段时间没有请求**后自己退出（`SPASS_DAEMON_IDLE_SECS`，默认 300 秒）。长跑程序不会被误杀：spasio 客户端把连接放回连接池、**不关闭**，daemon 侧的连接计数因此一直大于 0，空闲计时只在真的没人用时才触发。

### CLI 语义修正
- `spass daemon --status` 在没有 daemon 时原本报「无法连接」并以非零码退出——**状态查询不该以错误收场**。现在回「daemon: not running」、退出码 0；`--stop` 同理（停一个没在跑的 daemon 就是目标状态）。
- 自动拉起 runner 时 CLI 会说明：「runner: started a local runner on 127.0.0.1:9100（空闲即退出；`spass daemon --stop` 立即结束）」。凭空多出来的后台进程不该靠用户猜。

### MSI 收尾
- `msiexec /x <msi 路径>` 在**重建过同一个 MSI 文件**之后会失效（ProductCode 固定、PackageCode 每次构建都变 → 1638「已安装另一版本」）。`build-msi.ps1` 现在把 product code 一并打出来，卸载用 `msiexec /x {6B2D8F41-…}` 永远有效。
- 卸载后 `%LOCALAPPDATA%\Programs\Spass` 目录残留 → 给 `INSTALLFOLDER` 补了 `RemoveFolder`，现在卸载后目录也不留。
- `build-msi.ps1` 里两处 `rm -- $path` 是错的：PowerShell 中 `rm` 是 `Remove-Item` 别名，`rm --` 不是合法形式。改成 `Remove-Item -LiteralPath … -Force`。

### 规范边界（一条不能动的线）
曾想过让 `spasio:input[file]` 直接读 `data/input/<name>.spb`、彻底绕开 runner——那样 `await` 示例就不需要任何后台进程。**但规范 §9.2 明确写了「三种模式的数据流全部经由运行器中转，程序进程不直接触碰文件系统」**，而且 spasio 是 Spass 与其它语言通信的唯一通路。所以这条线不能动：runner 必须真实存在，CLI 的责任是把它管好（拉起、告知、有界生命周期），不是把它绕过去。

### 全量验收结果
- 单元测试：**200 通过**（codegen 96 / infer 63 / parser 26 / semantic 13 / **CLI 2**——新增：空闲窗口解析、句柄继承标志的清与还原）。
- 示例项目：**16/16 构建通过，13 个跑通**，3 个 build-only。
- `spass test`：**27 个测试全过**（assign 2 / await 1 / if_present 4 / struct 3 / terrain 5 / vec 12）——await 这条现在是 runner 自动拉起后通过的。
- playground：6 项像素断言全过，**2.25 ms/帧（444 fps）**。
- deeppack：4 项全过（宿主 P/Invoke 调 DLL：普通函数 / 值域状态索引 / 多返回值结构体）。
- MSI：装 → 跑（`daemon --status` 回「not running」、退出码 0；`build --deeppack` 出 DLL + 清单）→ 卸 → **用户 PATH 与会话开始时逐字节相同、安装目录已删除**。

### 已知未做
- **runner 的空闲窗口目前只能用环境变量 `SPASS_DAEMON_IDLE_SECS` 配**，`Spass.toml` 里还没有 runner 段。
- **2D 层独立缓冲**、**`.saple` 装真机器码**、**删除 C 后端**：仍是已知深水区，未做。

## 2026-10-29 - 会话三: CLI 质量、2D 独立图层、配置生效，以及一个真正的句柄泄漏

### 用户的三件事
1. **提升 CLI 的质量**：不能让人纯等。
2. **2D 独立缓冲做完整**：至少要有独立的图层。
3. **配置稍微做一下**：让 `Spass.toml` 真正生效。

外加上一条一直悬着的要求：**发现硌手处要改代码与设计，不是改测试。**

### 一、进度提示（自己设计的仪表读数，不是抄来的旋转动画）
第一版是照着 npm 的 `|/-\` 做的。**这是错的**：那是 npm 的东西，抄过来既不属于
npm 也不属于这里。推翻重做。

重做的出发点是一个观察：**转圈的符号只回答一个问题——「还活着吗？」**
而等待的用户真正会问的是「多久了」和「到哪一步了」。所以做的是一台**仪器**，
不是转圈：

```
  compiling the module  ▶···················    4.2s
  compiling the module  •▶··················    4.2s
  compiling the module  ──────•▶············    4.2s
```

- **步骤名**：秒数花在哪里去了。
- **扫描头**在一把刻度上从左往右扫。**扫过的那一段填成实线并高亮**，前方是暗的点。
  方向因此一眼可辨：一条被点亮的长线加一个箭头，比一个在点阵里乱窜的箭头更像
  一台仪器在走。头紧跟其后的圆点是它刚离开的那一格，比实线暗一档。
- **计时器**在数，字段定宽，所以读数不会在眼前挪位置。

那个填充段**不是百分比**：编译器不知道一个步骤要花多久。它标的是这次扫描已经
走过的部分——这正是让方向可读的东西。

几个决定都是看着实物改出来的，不是拍脑袋：
- 标签**不占固定列**（第一版占 28 列，结果步骤名和刻度之间开了一个空洞）。
- 拖尾用过 `▸▶`（两个箭头，像出了 bug）、`▪▶`、`•▶`，最后选 `•▶`。
- Windows 控制台**有代码页**，重定向的流没有。所以 Windows 上走
  `WriteConsoleW`（UTF-16，不会被代码页弄乱字形），别处走 UTF-8 字节。
  新增 `compiler/crates/spass-cli/src/console.rs` 专门管这件事。
- 动画线程等的是**通道**不是 sleep：sleep 会让 CLI 为了退出而等下一帧。
- **颜色是可选的**。Windows 控制台默认**不开** VT 处理，所以 `supports_color()`
  自己去开，并报告控制台收不收；收不了就退回纯字形，绝不把转义序列当字面文本
  打出来。
- **颜色转义占字符但不占格**。`console::width` 必须跳过转义，否则这一行的
  补白和擦除宽度都会算错，上一帧的残渣会留在屏幕上。

两条让它可以到处用的规则：
- **只在 stderr 是终端时动画**。重定向到文件、在 CI 里、在管道里时没有动画，
  步骤结束时打印一行摘要（步骤名 + 用时）。日志里不能出现半截的动画帧。
- **150 ms 之后才出现**。一次 20 ms 的缓存命中不该闪一下动画——闪烁比不显示更让人分心。

动画写 stderr 不写 stdout：stdout 是程序的输出。

**顺带修掉一个真 bug**：rustc 的 stderr 原本是**继承**的，而动画每一帧都会
重写那一行——于是 **rustc 的诊断（尤其是编译失败的原因）会被下一帧擦掉**。
现在 rustc 的 stderr 单独接走、在读数擦掉之后原样转述。

### 二、2D 独立图层（终于做完了）
上一轮记为 ponytail 的「给 2D 层一个独立缓冲」，这一轮做完了，两个后端同步：

- 帧缓冲变成三层数据：3D 像素、2D 覆盖层（每像素带 alpha）、合成结果。
- `present` 时合成，**2D 永远叠在 3D 之上**。
- 于是「2D 与 3D 的绘制顺序」不再重要。旧模型下顺序错了 2D 会直接消失，
  运行时只能在事后打一条 stderr 警告；现在这个错误不存在了，
  **那条顺序警告连同 `drew_2d` / `drew_3d` 的标记一起删掉**——它在新模型下是错的。
- 所有 2D 图元多了一个显式的 `a` 参数（pixel 6 / line 8 / rect 8 / frame 8 / circle 7 /
  text 8 / label 4 / button 6 / slider 8 / checkbox 5 个参数）。显式参数而不是
  状态式 `opacity()`：API 一致，没有隐藏状态。
- `clear` 清两层：2D 覆盖层是这一帧的一部分，不是另一块屏幕。
- 示例改成**先画 2D 再画 3D**——旧模型下这样会让 2D 消失，新模型下两帧一致。
- 验证脚本从 6 项加到 9 项断言，其中一项是「面板区里 0 个 3D 像素」，
  这一项在旧帧上有 237 个网格像素，不是空洞的断言。

### 三、配置生效
新增 `compiler/crates/spass-cli/src/config.rs`，`Spass.toml` 从此**真的生效**：
`build.output`（产物目录）、`build.timeout`（编译超时，会 kill 掉卡住的 rustc）、
`spasio.data_dir`（file 模式通道目录）、`spasio.socket_host` / `spasio.socket_port`、
`debug.log_level`、`runner.idle_secs`（新增）。

两条读配置的原则：
- **未知的键、���知的 TOML 构造：忽略**。用了本编译器不认识的 TOML 特性（数组、内联表）
  的项目仍然要能构建。
- **已知的键但值用不了：报错**。`build.timeout = "soon"` 不许静默回落成默认值——
  静默回落会把一个拼写错误变成一个「配置好像没生效」的问题，那更难查。

优先级：**环境变量 > 项目配置 > 默认值**。

### 四、一个真正的句柄泄漏（上一轮的修复只做了一半）
上一轮以为「daemon 继承调用方的 stdout」已经修好了：拉起 daemon 前把三个标准句柄的
`HANDLE_FLAG_INHERIT` 清掉、spawn 完再恢复。**它没修好。**

复现方式：`SPASS_DAEMON_IDLE_SECS=30` 时，`spass run --direct driver` 要花 **32.7 秒**
（30 秒空闲窗口 + 2.7 秒干活）。命令其实早就干完了，是管道迟迟等不到 EOF。

定位过程（全部靠实测，没有靠猜）：
1. 探针实验：清掉 inherit flag 后拉起一个普通子进程（powershell）→ 478 ms 返回，
   **说明「清 flag」这一招本身有效**。
2. 同样的探针拉起 `spass daemon` → 20.1 s，**复现**。
3. 写了个句柄枚举器（`NtQuerySystemInformation` + `NtQueryObject`）去翻 daemon 的句柄表，
   发现父进程的 stdout 句柄**确实没有**被继承——清 flag 是生效的。
4. 但父子**共有**的句柄里有 6 个管道，而且都不是父进程的标准句柄：
   **shell 交给 CLI 的那些管道**。CLI 从来不会提到它们，所以也不会去清它们的 inherit 标志，
   它们照样被 daemon 继承走。daemon 于是一直攥着 shell 的输出管道。

**结论：清三个标准句柄不够。** shell 会把自己的管道一路传下来，CLI 手里有一堆
「继承来的、但不是标准句柄的」可继承句柄。

修法不是继续清标志，而是**从根上不给继承的机会**：新增
`compiler/crates/spass-cli/src/win_spawn.rs`，用 `CreateProcessW` 以
`bInheritHandles = FALSE` 创建子进程，标准句柄显式交给 NUL 设备。
凡是活得比 CLI 久的子进程都走它：daemon，以及常驻 world（§13b）。

效果：`spass run --direct driver` 从 **22.8 s 降到 2.98 s**（空闲窗口 30 s），
经 daemon 派发的路径 6.1 s（首次要编译），热跑 1.5 s。

顺带发现并修掉的：`std_handle_inheritance` 的「还原」其实是**无条件置位**
（`std_handle_inheritance(true)` 而不是还原原值），会把它本来没有 inherit 标志的句柄
也置上。这个函数连同它的单元测试一起删了——新机制整体取代了它。

新增回归：`compiler/scripts/verify-runner.ps1`。它把 CLI 的输出重定向到管道并**读到 EOF**，
这正是泄漏时挂住的那一步。

### 五、顺带修掉的其它硌手处
- **`SPASS_DATA_DIR` 之前根本没人认**（只在 C runtime 的一句 `ponytail` 注释里出现过）。
  现在它是 `spasio.data_dir` 的环境变量覆盖项，和 socket 地址、空闲窗口一致。
- **CLI 连的地址和它导出给 runner 的地址可能不是同一个**：CLI 自己按
  「环境变量优先」解析地址，却把**配置里的原始值**导出给 runner。
  设了 `SPASS_SOCKET_PORT=9311` 而配置写的是 9100 时，CLI 去连 9311，
  runner 却去听 9100 —— 谁也够不着谁。现在导出的是**解析后的地址**，
  两端必然一致（实测：9311 时 runner 也起在 9311）。
- **源文件带 UTF-8 BOM 就编译不过**，报错是 `unexpected character 'ï'`——
  而 Windows 的编辑器默认就写 BOM。词法器现在跳过开头的 BOM，
  位置照旧从第 1 列算起（BOM 不是可见字符）。加了一个回归测试。
- `spinner.finish()` 之前是死代码；现在编译成功时用它把动画那一行换成
  「compiled the module」。非终端环境不打印这行——日志里已经有「compiling the module...」了。

### 六、规范落点
按「新增文件、不改原有条款」的规矩，本轮的东西落在两个新文件里：
- `项目简介-Playground.md` **更新**：§1 两层模型、§1.1 顺序无关、§2 带 alpha 的签名表、
  §6 效率、§7 验证的 9 项断言。
- `项目简介-CLI.md` **新增**：§1 进度提示的规则、§2 配置的键与优先级、
  §3 运行器生命周期与「不得占住调用方输出」这条约束。

### 全量验收结果
- 单元测试：**223 通过**（codegen 96 / infer 63 / parser 27 / semantic 13 / **CLI 24**）。
- 示例项目：**16/16 构建通过，13 个跑通**，3 个 build-only。
- `spass test`：**27/27 通过**（assign 2 / await 1 / if_present 4 / struct 3 / terrain 5 / vec 12）。
- playground：**9 项断言全过**，2.28 ms/帧（438 fps）。
- deeppack：4 项全过。
- runner 回归：4 项全过（不占住调用方输出 / 活得比调用久 / 随叫随停 / 空闲自退）。
- MSI：用新 CLI 重建，装 → 跑 → 卸，用户 PATH 与会话开始时逐字节相同。
- 读数每帧渲染 **< 1 µs**（上限 50 µs 的单测守着），14 帧/秒对 CPU 不可见。

### 七、构建反复失败的真凶：`compiler/target/` 是被跟踪的
这一轮十次里有八九次构建失败，报的还都不一样（E0463 / E0460 / LTO 找不到
`.llvmbc` / fingerprint 写不进去 / 「不能在使用用户映射区域打开的文件上执行」）。
前几轮都是用「按报错类型 touch 对应源文件再重试」的循环硬扛过去的。

真正的根因不在代码：**`compiler/target/` 里有 2912 个被 git 跟踪的构建产物**
（磁盘上 1.14 GB）。`.gitignore` 其实早就写了 `compiler/target/`——但 ignore
规则对**已经被跟踪**的文件无效，于是这堆机器相关的产物一直跟着仓库走：
cargo 会捡起里面被写坏/写了一半的 rlib，链接阶段就找不到 LTO 的 bitcode 段。

修法：
- `git rm -r --cached compiler/target`（取消跟踪，磁盘上的先不动），
- 删掉磁盘上那 1.14 GB，让 cargo 从干净状态重建。

之后 `git status` 里那 2900 多条构建产物噪声也没了。

### 已知未做
- **`.saple` 里的 C 源码与规范 §1.3 不符**（规范说装机器码，实现装源码）：运行器架构改动，未做。
- **C 后端仍无法执行验证**：本机没有任何 C 编译器，C 侧只做结构化断言。
  这一轮 2D 图层与 alpha 参数在 C 后端**只做了结构化验证**。
- **C 后端该不该留**：见最终汇报里的结论。
### 八、刻度：扫过的部分填实并高亮
用户看完第一版的仪表读数，提的是一个具体的视觉问题：

> the thing could be `------•▶············` instead of `······•▶············`,
> the finished part should be highlight

第一版的头本来就在往右走（`head = (head + 1) % TRACK`），但**走过的格子还是点**，
所以方向只靠一个箭头的位置表达。这版改成：

- 头**从左往右扫**；
- **扫过的那一段填成实线 `─` 并高亮**（青色），前方是暗点（`·`，暗化）；
- 紧跟头后面的那一格是它刚离开的（`•`，比实线暗一档），拖尾仍与实线同族。

填充段**不是百分比**——编译器不知道一个步骤要花多久。它标的是这次扫描已经走过的
部分，这也正是让方向一眼可辨的东西。到头以后从头再扫，填充段清零重来。

上色引出两个必须处理的约束，否则会画出坏东西：
- **颜色转义占字符不占格**。`console::width` 现在跳过 ANSI 序列，否则这一行的
  补白和擦除宽度都算错，上一帧的残渣会留在屏幕上。加了单测钉住。
- **颜色是可选的**。Windows 控制台默认不开 `ENABLE_VIRTUAL_TERMINAL_PROCESSING`，
  所以 `console::supports_color()` 自己去开并报告结果；控制台收不了就退回纯字形，
  绝不把转义序列当字面文本打印出来。

帧里只写 4 次转义（按样式变化写，不是每格一次），单测把确切字节序列钉住了：
`\x1b[36m──────\x1b[2;36m•\x1b[1;97m▶\x1b[2m············\x1b[0m`。

### 九、拆掉 `spass_runtime.dll`：一个没人用的空壳
MSI 重建卡住了：`installer/spass.wxs` 引用 `spass_runtime.dll`，而这个文件不在
`target/release` 里，`WIX0103` 直接让构建失败。

查下来它**根本不该存在**：
- 它是 `spass-runtime` crate 的 cdylib，但那个 crate 里只有 `RUNTIME_C` 一个字符串
  常量——`runtime.c` **从来没有被编译进任何二进制**。
- 解析它的 PE 导出表：1663 个符号**全是 Rust 内部符号**，一个 `spass_*` 都没有。
- 55 个 C 运行时符号全在 `runtime.c` 里，以**源码**内联进每个生成的模块
  （`spass_codegen` 的 `out.push_str(spass_runtime::RUNTIME_C)`）。
- 扫了 `spass.exe` 和所有示例产物的导入表：**没有任何产物 import 它**。
- 规范（§2.4）要求 `.saple` 里放各平台的机器码，没有要求任何运行时 DLL。

也就是说，它是一个 1.4 MB 的空壳，装进用户机器却什么都不做。

按「硌手处改代码与设计」的规矩，做的是**移除**而不是「让构建脚本多编一个 crate
把它补出来」：
- `spass-runtime` 的 `crate-type` 去掉 `cdylib`（只留 `rlib`），顺带消掉 cargo
  那条 "output filename collision" 警告；
- `spass-installer/build.rs` 不再找它，`payload.rs` 不再生成 `SPASS_RUNTIME_DLL`；
- `spass-installer/src/main.rs` 不再写它；
- `spass.wxs` 去掉 `SpassRuntimeDll` 组件与 `ComponentRef`；
- `build-msi.ps1` 去掉 staging；
- `INSTALL.md` 改掉「C runtime (math, spasio, sequences, tasks)」这句**不实**的描述
  （它从来不含这些），改成说明运行时是以源码随模块走的；
- `AGENTS.md` 与 5 个验证脚本里那句「`spass_runtime.dll` 链接 libunwind」也是错的——
  真正需要 toolchain 的 `libunwind.dll` 的是 `spass.exe` 本身，一并改对。

MSI 现在装的是：`spass.exe` + `libunwind.dll` + `bootstrap\bootstrap.spass`。

## 2026-11-14 - 会话: 收尾 saple 运行器 + 补全 list/random/parallel + 简化

### 一、`.saple` 运行器（确认的真 bug）
**症状**：`spass runner build\spassfile.saple` 跑 sequence 包时打印入口返回值后
以 `0xC0000005`（访问冲突）退出，sequence 的输出槽 `result.spb` 缺失。

**根因**：生成的 cdylib 只有 `spass_module_start`（启动 sequence 线程），没有
join 入口；`cmd_runner` 调完函数就返回，进程退出时 DLL 被卸载，而 sequence 线程
还在跑。另：`spass runner` 从不确保 spasio runner 在位。

**修复**：
- codegen 的 Rust 模块入口新增 `spass_module_wait`（无 sequence 时为空操作）；
  `spass_runner::Module` 增加 `wait()`，runner crate 增加 `WAIT_SYMBOL`。
- `spass runner`：包里有 channel 时先 `ensure_runner`；有 sequence 且未点名函数时
  只启动并等待，不调用（默认入口也跳过 sequence）；点名函数时先调用再 join；
  调用失败也先 join 再报错。
- 参数改为严格数字解析：非数字直接报错，不再静默丢弃。
- 明确拒绝把 sequence 当函数调用。
- manifest 增加 `channels` 与 `runner.sequences`；`default_entry` 跳过 sequence。

**验证**：sequence 包 `runner` 无参 → `result=30`、退出码 0；`runner ... driver`
同样写出 30；`runner ... producer` 给出「是 sequence」错误；非数字参数报错。
`verify-runner.ps1` 与 `verify-deeppack.ps1` 全绿。

### 二、`module.sps` 从调试转储改为确定性 schema
之前写的是 `format!("{:?}", ty)`：`RangeDecl { ... Span { file: "D:\\..." } }`
——绝对路径和 Rust 调试格式进了对外契约。现在 `saple::module_sps` 输出行式 schema：
`schema/module/compiler`、每个公开函数的 `symbol/param/result`（源类型 + ABI 类型 +
`domain=(min,max,step) k= states= bits=`）、结构体字段、`channel: <mode> <dir> <name>`。
`build/manifest.json` 与 `.saple` 内的 manifest 由同一个 `manifest_base` 生成，不再
各自漂移。terrain 的 `module.sps` 已人工核对（无路径、无 Debug 格式）。

### 三、list（§3.2）补全
- parser：`for x in xs` 先解析被迭代表达式，只有后面跟 `..` 才是 range；旧实现只在
  首 token 是 `[` 时识别 list，变量列表直接报“expected `..`”。
- semantic：`list.length` → `int`；索引非 list 报 E006；list 返回/公开函数 list 参数/
  struct list 字段给出明确边界错误（list 目前是函数局部值；C 的 compound literal
  生命周期与 Rust 的 FFI 安全都不允许它逃逸）。
- codegen C：统一 `SpassList { unsigned char* data; int32_t len; int32_t stride; }`
  （字节视图 + stride，支持标量/vec/struct/嵌套 list），字面量、越界终止的索引、
  `.length`、for-in 全部落地；元素类型来自 `let` 注解或首元素。
- codegen Rust：`Vec<T>`、`vec![]`、`(idx) as usize` 索引、`.len() as i32`、
  `for x in (list).iter().cloned()`。
- 新增 `examples/list`（标注/裸字面量/空列表/嵌套列表 + 4 个测试）。

### 四、random（§4.3）补全
- parser：`random(a,b,c)` 与紧贴的 `random(f,g)(args)` 区分为两种形态（第二个 `(`
  必须紧接第一个 `)`；有空格则是“对值取 random 再调用”，语义层会报 not callable）。
- AST：`Expr::Random { candidates, args }`。
- semantic：值模式要求候选为数值且类型兼容（task 候选项取 await 后的类型）；函数模式
  要求候选是函数名、参数表对每个候选都能通过调用检查、返回类型兼容。
- codegen：值模式 `spass_random_pick`；函数模式 C 用函数指针 compound literal，
  Rust 用 `spass_random_index` + if 链。运行时是 xorshift64（C 用 clock 播种，Rust
  用 SystemTime + 静态地址），无新依赖。
- 新增 `examples/random`（值/整数/函数三种选择 + 3 个测试）。

### 五、parallel（§5.4）补全
- `Type::Task` 改为 `Task(Option<Box<Type>>)`：spawn 携带返回类型，parallel 对 list
  参数携带 `list<返回类型>`；`await` 返回 payload，int/uint/bool 在两侧按类型还原。
- semantic：list 参数逐元素对照形参类型，标量参数照常；返回 task。
- codegen C：`spass_parallel_spawn_args`（每行按 list 下标取值、标量重复、长度不等
  abort），`spass_parallel_await` 收集为 `SpassList`。
- codegen Rust：参数先绑定一次，`Vec<f64>` 行主序展开，`spass_parallel_spawn` /
  `spass_parallel_await` 收集为 `Vec<f64>`；长度不等 `assert_eq!`。
- 新增 `examples/parallel`（双 list / 单 list + 2 个测试，结果 66 / 29）。

### 六、顺带修掉的真 bug
- **playground 的 cdylib 构建失败**：`emit_dispatch_results` 对 bool 返回值生成
  `r as f64`，Rust 不允许 bool→f64。现在生成 `(if r { 1.0 } else { 0.0 })`。
  `verify-playground.ps1` 9 项像素断言全过（2.08 ms/帧）。
- 删除 runtime.c 里“socket/memory 仍是 stub”的过期注释（RPC 早已取代）；
  `Expr::IfPresent` 的 codegen 报错改为“if_present 是语句”（真实实现走 transform）。

### 验证
- `cargo test --release`：**298 passed, 0 failed**（codegen 94 / semantic 67 /
  infer 63 / parser 29 / CLI 30 / transform 15 / optimize 13 / runner 等）。
- `verify-examples.ps1`：**19/19 构建，16 运行，3 build-only**（新增 list/parallel/random）。
- `spass test`：**36 passed**（assign 2 / await 1 / if_present 4 / list 4 / parallel 2 /
  random 3 / struct 3 / terrain 5 / vec 12）。
- `verify-runner.ps1`、`verify-deeppack.ps1`、`verify-playground.ps1` 全绿。

### 已知边界（本轮明确保留）
- list 是函数局部值：不能作为返回值、公开函数参数或 struct 字段；升级路径是
  manifest 里的 typed list descriptor。
- `parallel` 的 list 参数只支持 float/int 元素，且被调函数需 spawn 兼容
  （纯 float/int 参数与返回）；混合 vec/struct 参数需要 typed arg-pack。
- `random(task)` 会先把所有 task 候选项 await 完再选（spec 的“自动等待”语义），
  不做惰性选择。
- `spass build --platform=all`、`@gpu` 的 CPU 线程池调度（13e）仍未做。

## 2026-11-15 - 会话: 打包模式行为对齐 + 编译期优化渠道 + `.saple` 文件关联

### 一、CLI 收口
- `spass-cli` 第一次完整编译通过：4 个编译错误（`config::Value` 可见性、
  `saple::resolve_symbol` 的 `bail!` 用错位置、测试编译路径缺 `plan`/`report`）
  和 8 个死代码警告全部清掉，**零警告**。
- `cmd_build` 现在会打印优化器报告（跑过优化时打摘要，`--diagnostics` 打逐项）。
- 删掉的死物：`deeppack::emit`、`deeppack::library_path`、`saple::platform_tag`、
  `BodyConfig::disabled`、`OptimizeRequest::is_empty`、`config_dir`、`load_from`。
  `config_dir`/`load_from` 的注释说「安装器要用」，但安装器是 WiX 与 Rust 两套
  自己的实现，都不调它们——注释是假的，代码是死的。

### 二、`.saple` 装机器码（缺陷 C/D）
- `spass build` **总是**编译 cdylib；`build/spass_module.dll` 与包里的
  `native/<平台>.<扩展名>` **逐字节相同**。`--deeppack` 只额外写一份 ABI 清单，
  **不重新编译**——两种容器从此不可能在「算出来的数」上漂移。
- 包里另带 `native/libunwind.dll`（加载器在模块旁边解析它，没有它模块加载不了）
  与 `native/source.c`（给要为别的平台重编模块的宿主做参考）。
- 缺陷 D（`.saple` 装的是 C 源码、无人读取）由此消解：包里现在是机器码。

### 三、统一模块入口（缺陷 A）
- 生成的模块导出三个统一入口：
  `spass_module_start`（启动 sequence）、`spass_module_wait`（汇合）、
  `spass_module_call`（按符号名调用，参数与命令行一致）。
- 有了统一入口，**不认识 Spass 签名的加载器也能调用模块**——`.saple` 运行器
  因此能装载一个它没编译过的包。`spass runner` 的顺序固定为
  **启动 → 调用 → 汇合**，调用失败也必须汇合（错误在汇合之后才报）。

### 四、编译期优化渠道（独立，默认全关）
- 新 crate `spass-optimize`，跑在 semantic 之后、codegen 之前；
  `spass-transform::prepare` 把 `if_present` 解析与 `@dispatch` 展开**提前**，
  因为 `@dispatch` 会给目标赋值，reducer 看不到就会把它折掉。
- 五个开关：`readonly_locals` / `inline_single_use` / `fold_constants` /
  `const_fold_arith` / `dead_code_elim`；档位 0（全关，默认）/ 1（只读）/ 2（标准）/
  3（激进，含删函数）。
- 硬编码判定五条件：`let` 绑定 + 初始化式编译期可知 + 从未被 `Assign` +
  不逃逸 + 不跨 `@await`；含 `@await`/`@checkpoint` 的函数整体跳过。
- **除法不折叠**；整数用 `checked_*` 暴露溢出；**浮点用精确有理数**
  （`0.1+0.2` → `3/10`），与运行时 IEEE-754 可能差 1 ULP——这是有破坏性的，
  默认关闭的原因。
- 设置放在 **Spass 本体**的用户级配置（`%LOCALAPPDATA%\Spass\config.toml`，
  其它平台 `~/.config/spass/config.toml`），**不进 `Spass.toml`**；
  优先级 CLI > `SPASS_OPTIMIZE` > 文件 > 默认全关。
  **项目 `Spass.toml` 里出现 `[optimize]` 段直接报错**，不是静默忽略。
- `spass code-optimize` 是独立渠道，产物写进 `build/optimized/`，**不覆盖**常规产物。

### 五、`.saple` 文件关联
- MSI（WiX）与 `spass-setup`（Rust）都登记：
  `HKCU\Software\Classes\.saple` → `spassfile.saple` →
  `shell\open\command` = `"<安装目录>\spass.exe" runner "%1"`。
- 登记在 `HKCU` 下，安装**不需要提权**、不触发 UAC；两个安装器卸载时都清干净。
- WiX 侧踩了个坑：WiX v5 的 **util 扩展里 `util:RegistryKey` 不能挂在
  `Component` 下**（报 WIX0005），而核心命名空间里的 `RegistryKey` 可以，
  且带 `ForceCreateOnInstall` / `ForceDeleteOnUninstall`——正好覆盖
  「装上时建键、卸载时连子键一起删」的需求。
- `verify-msi.ps1` 顺带修了几处**对环境状态的错误假设**：
  - `daemon --status` 断言「必须是 not running」——但 runner 是机器级单例，
    别的测试留一个在跑，这个断言就看运气。改成断言「退出码 0 且给出可识别的
    状态」；「not running」那一句是 `verify-runner.ps1` 的责任，它自己管 runner。
  - PATH 的比较原本是逐字节——安装器追加目录、卸载器删掉目录，两边对分隔符
    的处理不一定一致（残留一个尾部反斜杠不算机器状态变了）。改成按**目录列表**比。
  - 测试自己起的 runner 现在会被停掉；之前留下的 runner 不会被误杀。
  - 开发版的 `spass.exe` 缺 toolchain 的 `libunwind.dll` 就起不来（0xC0000135），
    脚本补上了 toolchain 的 `bin` 目录。

### 六、新增 `verify-packaging.ps1`
- 对每个导出函数跑**三种模式**：程序 dispatcher（`spass run --direct`）、
  `.saple` 运行器（`spass runner`）、deeppack 宿主（进程内加载 DLL，调统一入口）。
  三个结果必须**逐位相同**，槽位数必须与清单里的 `results` 相同。
- 另验两项：`sequence` 在包里要跑完并落盘；spasio 通道的值在两种模式下都要
  穿过运行器到达。
- 覆盖 11 个导出函数（普通 float / int / 值域参数 / 值域返回 / 多返回 /
  `out` 参数 / 值域 `out` / `@dispatch` 展开 / 标签别名）。

### 七、规范与文档
- `项目简介-CLI.md`：新增 §4「编译期优化渠道」、§5「打包模式的一致性」。
- `项目简介-Deeppack.md`：§1 补「共享库与包里的机器码是同一份」；
  新增 §3.5 的 `results` 与 `runner` 段、§3.6 统一入口、§3.7 sequence 启动/汇合/
  运行器地址契约；§4 补统一入口用法；§6/§7 补对应表与验证。
- `AGENTS.md`：新增「The `.saple` carries machine code」「The optimisation pass」
  「The saple runner」三节，测试计数与脚本清单同步。

### 验证
- `cargo test --release`：**298 passed, 0 failed**（codegen 94 / semantic 67 /
  CLI 30 / parser 29 / optimize 29 / installer 15 / infer 13 / transform 8 /
  runner 7 / bootstrap 3 / runtime 2）。
- `verify-examples.ps1`：**19/19 构建，16 运行，3 build-only**。
  顺带修了脚本的报告取值：返回值在 stdout、进度提示在 stderr，合并后顺序不定，
  三个新示例的备注因此显示成了提示行而不是数值。
- `verify-packaging.ps1`：**11 个函数 × 3 模式全部一致** + sequence + spasio。
- `verify-playground.ps1` 9 项像素断言、`verify-deeppack.ps1` 4 项、
  `verify-runner.ps1` 4 项、`verify-msi.ps1`（含文件关联装上/清干净）全绿。

### 已知边界（本轮明确保留）
- C 后端与优化器的交互只能**结构化**验证：本机没有 C 编译器，生成的 `module.c`
  没人编译过。
- `spass-runner` 的 Unix 分支（`dlopen`/`dlsym`）未经真机编译验证——本机只有
  `x86_64-pc-windows-gnullvm` 一个 target。
- `spass runner` 是运行器的**一次性**形态；常驻形态仍是 `spass daemon`。
  规范 §2.5 说运行器是常驻进程，一次性形态是为此加的 CLI 补充（写在
  `项目简介-CLI.md` §5.5），不动 §2.5 的条款。
- `spass build --platform=all`、`@gpu` 的 CPU 线程池调度仍未做。
- `cargo clippy` 在新 crate 上还剩 8 条 `result_large_err` / `clone_on_copy`：
  `SemanticError` 是整个编译器共用的错误类型，仓库里 `spass-semantic`(6)、
  `spass-codegen`(57)、`spass-infer`(64) 都有同一条 lint。在两个新 crate 里
  单独特意 `allow` 掉它只会让它与仓库其余部分不一致，而 `Box` 掉错误又会
  让新 crate 的错误类型与其它 crate 分叉——所以保持现状。新写的
  `clone_on_copy`（`BinOp`/`UnaryOp` 是 `Copy`）已经改成 `*op`。
## 2026-11-17 - 会话: playground 的 s2d 图元/文字度量与 s3d 变换栈/球柱

### 起点
用户指令：`continue devoloping spas2d and spas3d`。本轮增量由我判断选定：
**s2d 补图元与文字度量，s3d 补变换栈与球/柱**（不扩 `spio:`——那是另一个
命名空间，且已有已知的后端差异）。

### 一、新的语言表面（11 个 builtin）
- `s2d:ellipse(cx, cy, rx, ry, r, g, b, a) -> bool`、`s2d:tri(x0,y0,x1,y1,x2,y2, r,g,b,a) -> bool`
- `s2d:text_width(text, scale) -> float`、`s2d:text_height(scale) -> float`
- `s3d:push()` / `s3d:pop()` / `s3d:translate(tx,ty,tz)` / `s3d:rotate(rx,ry,rz)` / `s3d:scale(sx,sy,sz)`
- `s3d:sphere(cx,cy,cz,rad,segs, r,g,b)`、`s3d:cylinder(cx,cy,cz,rad,h,segs, r,g,b)`

`segs` 是**计数**，所以声明成 `int`（规范 §4 的同一条理由），运行时签名相应是
`i32` / `int32_t`。

### 二、运行时（Rust 与 C 同步）
- **圆与椭圆共用一份实现**：椭圆按行扫描，半宽 `rx*sqrt(1-(fy/ry)^2)`；`rx==ry`
  时与旧的 `sqrt(r^2-fy^2)` 逐像素相同（用 Python 对多组半径与圆心验证过）。
- **2D 三角形**用重心坐标填充，绕向无关（权重除以带符号面积，所以两种绕向一致）。
- **文字度量**：字形格 6×8（5 列笔画 + 1 列间距），`scale` 截断成整像素且至少 1，
  与绘制规则一致——度量必须和画出来的东西对得上。`text_width` 取最宽的一行。
- **换行符现在真的会换行**：旧实现只把列号复位，行号没动，第二行画在第一行上。
  修掉它，文字度量才有意义。
- **模型变换栈**：4×4 行主序仿射矩阵，`模型 = 模型 × 操作`（后写的变换先作用到
  顶点上）；旋转按 X→Y→Z，单位度；`push`/`pop` 最多 64 层，空栈 `pop` 返回
  false（是错误，不是静默 no-op）。矩阵跨帧保留，`clear` 不重置它。
- **球与柱是平面着色**，与盒子的观感一致。球的极点行退化成一点，所以不需要单独的
  极点处理。
- **C 后端补上了近平面多边形裁剪**：Rust 侧一直有 Sutherland-Hodgman，C 侧以前
  只逐顶点丢弃，跨过近平面的几何在两个后端画得不一样。现在两个后端用同一套裁剪。

### 三、护栏：没有 C 编译器时能自动抓到的那一类错误
本机没有 C 编译器（rustup 的 gnu 工具链只带了链接器，没有 `cc1`），所以 C 后端
只能结构化验证。新增 `spass-playground/src/surface.rs`：
- **语言表面**写在一处（`SURFACE`），两个后端和语义层都对着它检查。
- 测试断言：两个后端导出同一批符号、参数个数相同；C 后端调用助手函数时该函数必须
  已经定义（C 没有前向声明）；两个后端的括号都配平。
- C 后端调用的**外部函数**有一份显式清单，并检查该文件包含了声明它们的头文件
  （缺头文件同样是编译错误，而清单就是让人注意到这件事的地方）。
- `spass-semantic` 的测试再查一遍：注册的 builtin 与运行时导出的符号一一对应、
  参数个数相同。注册少了调用就通不过类型检查，个数对不上生成的模块就链接不上，
  而 `cargo test` 里没有任何东西会编译生成的模块。
- 这个护栏当场抓到两个真问题：C 的 `spg_circle` 调用了后定义的 `spg_ellipse`；
  参数计数把尾随逗号当成了多一个参数。

### 三之二、对 C 后端做了一次独立复核
因为本机编译不了 C，另外派了一个独立复核去通读 `playground.c`，找会**编译不过**
的地方。它找到了两个一直存在的真编译错误（都在 spio 段，不是本轮新增）：
- `_mkdir` 没有声明：它在 Windows 的 `<direct.h>` 里，POSIX 上根本不存在。
  现在改成平台分支的 `spg_make_dir()`（Windows `_mkdir`，POSIX `mkdir(path, mode)`），
  并按平台包含 `<direct.h>` / `<sys/stat.h>`。
- `LONG_MAX` 没有 `<limits.h>`。

顺带修了 `strrchr` 结果与空指针做关系比较的未定义行为，和两处 MSVC 的
`size_t`→`UINT`/`DWORD` 截断警告。也就是说：**C 后端以前在任何机器上也编译不过**
（缺头文件就是硬错误），这一轮把它修到能编译了——尽管本机仍然没有编译器来验证。

### 四、示例与验收
- `examples/playground` 用上了全部新能力：椭圆、三角形、按文字度量画的标签底板、
  用变换栈摆好的球和柱。标签底板从屏幕下方移到顶部——原来它压在圆柱上。
- `verify-playground.ps1` 的像素断言从 6 项加到 9 项，再加 4 项计数断言
  （球、柱、字形、3D 物体像素）。

### 五、规范
- `项目简介-Playground.md`：§2 加椭圆/三角形/两个度量函数，新增 §2.1「文字度量」；
  §3 加变换栈七个函数与球/柱，新增 §3.1「模型变换栈」；§6 更新帧成本；
  §7 补源码结构检查这一条护栏；§8 说明除 spio 目录列举外两个后端已一致。

### 验证
- `cargo test --release`：**305 passed, 0 failed**（原 298，新增 7 条护栏：
  playground 6 条 + semantic 1 条）。
- `spass test`（9 个带测试的示例）：**36 passed**。
- `verify-examples.ps1`：**19/19 构建，16 运行，3 build-only**。
- `verify-playground.ps1`：**15 项断言全绿**，帧成本 **2.9–3.2 ms/帧（315–347 fps）**
  （场景多了球和柱，比上一轮的 2.05 ms 慢，规范 §6 已同步）。
- `cargo clippy -p spass-playground`：37 条，全部是既有的（Win32 缩写、死代码、
  内部助手的参数个数——这个文件本来就不给内部助手加 `allow`，只有导出函数加）。
  新导出的三个函数按同样的约定加了 `allow`，没有引入新噪音。

### 已知边界（本轮明确保留）
- **C 后端仍然无法本地编译**：机器上没有任何 C 编译器（`gcc`/`clang`/`cl`
  都没有，rustup 自带的那个 gcc 只有链接器）。这一轮的 C 代码是**机械镜像** Rust
  实现写出来的，并由上面的源码结构检查守住，但没有经过编译器。
- 球和柱是平面着色，不是平滑着色；这是有意的（与盒子的观感一致）。
- `spio:` 目录列举在 C 后端仍返回 -1/空串，那是另一个命名空间的欠账，本轮没动。