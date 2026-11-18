# Spass 编译器实现计划 (task_plan.md)

## 目标 (Objective)
为 Spass 语言构建一个完整可工作的编译器。先用 Rust 实现引导编译器（bootstrap），然后用 Spass 重写核心纯计算模块（值域推断、位宽分配、精度裁剪、IR 生成）实现自举（self-hosting）。

## 关键决策 (Key Decisions)

### 决策 1: 后端选择
- **选择**: MVP 阶段使用 C 代码发射 + 系统 C 编译器（cc/clang/MSVC），避免 LLVM 完整构建
- **理由**: LLVM 在 Windows 上构建复杂、依赖重、首次构建 30+ 分钟；C 发射能让编译器在第一次构建时就端到端跑通
- **影响**: 失去 LLVM 跨平台机器码生成的精确性，但 MVP 阶段可接受；后续可替换为 `inkwell` 绑定
- **升级路径**: v0.2 切换至 LLVM IR 发射 → 通过 `llc` 编译为目标机器码

### 决策 2: 有理数精度
- **选择**: 使用 `num-rational` crate 处理 `min/max/step` 的精确分数运算
- **理由**: 规范第 3.5、12.3 节明确要求"不受 IEEE 754 浮点误差影响"
- **影响**: 性能略低于原生浮点，但编译期 O(1) 复杂度

### 决策 3: 自举范围
- **选择**: 严格按第 12.4 节 — 只自举值域推断、位宽分配、精度裁剪、IR 生成（纯计算模块）
- **理由**: 规范明确"词法分析、语法解析、调用图分析由 Rust 实现"
- **影响**: 自举覆盖编译器核心数值模块，但前端/系统 I/O 仍在 Rust

### 决策 4: 特性分阶段
- **选择**: MVP v0.1 仅实现子集：基础类型、值域、加减乘、C 后端、CLI
- **理由**: 完整 16 章节规范是多人年工程；先做端到端可跑通的最小内核
- **影响**: 后续阶段扩展：结构体、向量、标签、传递点、GPU、spasio 协议
- **当前范围**: 第 1-4 节 + 第 12.3 节值域推断 + 第 2.4-2.5 节 saple 打包 + 第 11 节 CLI 子集

### 决策 5: 自举集成方式 = FFI 双核心（方案 A）
- **选择**: `spass-infer` 的 build.rs 用自家 codegen 把 `bootstrap.spass` 编译为 Rust 函数（`OUT_DIR` 生成 + `include!`），推断阶段的数值核心（state_count / bit_width / add_sub / mul / step-gcd）改为调用 Spass 产物；Rust `Rational64` 同步计算并对拍，对拍失败即 panic（编译器 invariant 被破坏）
- **桥的无损性**: 有理数以 `(num, den)` 整数对跨越 f64 桥（|值| ≤ 2^53 时 f64 精确表示整数）；超出精确域（或 Rust 结果为 None）时回退 Rust 纯计算并计数
- **理由**: 无 crate 依赖循环（codegen 只出现在 build-dependencies）、无运行时编译/DLL 加载、精度不受损（方案 B 全量 float 切换被否决）
- **影响**: 编译期计算跑两遍（Spass + Rust 对拍），O(2x) 编译期开销可接受（决策 2 已接受）

## 阶段 (Phases)

### 阶段 1: 项目骨架 (Project Skeleton)
- [x] Rust workspace 创建
- [x] crate 划分: `spass-lexer`, `spass-parser`, `spass-ast`, `spass-semantic`, `spass-infer`, `spass-codegen`, `spass-cli`, `spass-runtime`
- [x] 依赖锁定: `num-rational`, `zip`, `clap`, `serde`, `anyhow`
- **状态**: completed

### 阶段 2: 词法分析器 (Lexer)
- [x] Token 类型定义 (keywords, identifiers, literals, operators, punctuation)
- [x] 关键字表 (第 13.1 节)
- [x] 数字字面量解析 (int/float + 指数 + 负号)
- [x] 字符串字面量 (含转义 \n \t \\ \")
- [x] 注释处理 (# 到行尾)
- [x] 位置追踪 (行号、列号、文件源)
- **状态**: completed

### 阶段 3: 语法分析器 (Parser)
- [x] 递归下降 parser
- [x] AST 数据结构 (Item, Stmt, Expr, Type, Pattern)
- [x] 函数声明 `func` 解析（含 `@gpu` 注解、`out` 参数、多返回值、if_present）
- [x] 结构体 `struct` 解析
- [x] 表达式解析 (优先级: `**` > `*,/` > `+,-` > 比较 > 逻辑)
- [x] 块语句、if/else、for/while、return、let
- [x] `include` 展开 (全局文本)
- [x] `@checkpoint`/`@resume`/`@await` 解析
- [x] `@tag`、`@dispatch` 解析
- [x] 错误恢复
- **状态**: completed

### 阶段 4: 语义分析 (Semantic Analysis)
- [x] 符号表 (函数/变量/标签/结构体注册)
- [x] 类型检查
- [x] 值域声明合法性 (E001)
- [x] 函数名唯一性 (E012)
- [x] 传递点名称唯一 (E007)
- [x] 标签键名唯一 (E015)
- [x] 循环依赖检测 (E004)
- [x] 递归检测 (E008)
- [x] `@gpu` 冲突检测 (E009, E013)
- [x] 内建函数注册 (sin, cos, sqrt, min, max, ...)
- **状态**: completed

### 阶段 5: 值域推断与位宽计算 (Range Inference)
- [x] Range 类型 `(min, max, step)` (有理数)
- [x] 二元运算 (ADD/SUB/MUL) 推断算法 (第 12.3 节)
- [x] 字面量 Range
- [x] let 绑定自动推断
- [x] 函数参数/返回值 Range 校验
- [x] 位宽计算 `ceil(log2(state_count))`
- [x] 128 位上限检查 (E010)
- **状态**: completed

### 阶段 6: 代码生成 (C Backend)
- [x] IR 设计 (精简的函数级 IR — 直接从 AST 翻译)
- [x] C 代码发射 (默认 `double` 精度)
- [x] 名字修饰 (第 2.5 节从右向左反向扫描)
- [x] Rust emitter (供 `spass run` 使用, 绕过 C 编译器)
- [x] 内建函数映射 (sin→spass_sin, etc.)
- [ ] LLVM IR 发射 (升级路径, inkwell + llc)
- **状态**: completed (MVP, 升级到 LLVM 待办)

### 阶段 7: CLI 工具
- [x] `spass build` (扫描 src/，编译，生成 saple)
- [x] `spass build --check`
- [x] `spass run <func> [args]`
- [x] `spass clean`
- [x] `spass tag [--list]`
- [x] `spass test` (扫描 test/，编译 + 运行所有 test，报告 pass/fail)
- [x] `spass help`
- [ ] 错误信息彩色输出 (绿/红/黄) — 已加基础 bold-green/red
- **状态**: completed (MVP)

### 阶段 8: saple 打包
- [x] manifest.json 生成
- [x] module.sps 模式描述生成 (骨架)
- [x] ZIP 结构 (Deflate 压缩)
- [ ] 各平台 native/<plat>.<so/.dll/.dylib> (MVP 仅打包 source.c)
- **状态**: completed (MVP, 跨平台 binary 待办)

### 阶段 9: 端到端测试
- [x] 示例项目 `examples/terrain/`
- [x] 编译 + 运行 + 验证输出 (height, slope, add 全部跑通)
- [x] 错误码覆盖测试 (E005/E006/E008/E012)
- [x] 测试 driver (`spass test`) 端到端 (terrain 5 / struct 3 测试通过)
- **状态**: completed

### 阶段 10: 自举 (Self-Hosting)
- [x] 提取值域推断算法至 Spass (`examples/bootstrap/src/bootstrap.spass`)
- [x] 在 `src/` 写 Spass 版本
- [x] Rust 编译器编译该 Spass 实现 (`spass run self_check` → 1)
- [x] self_check 验证 6 项算法 (`bit_width`, `state_count`, `add_min/max`, `mul_min/max`, `gcd`)
- [ ] 记录 bootstrap.md (位级一致哈希对比 — 可选验证)
- **状态**: completed (MVP)

### 阶段 11: 自举集成 — FFI 双核心 (方案 A, 决策 5)
- [x] `bootstrap.spass` 新增 num/den 整数对核心库 (`gcd_f` 二进制 GCD、`rat_add_nd`、`rat_mul_nd`、`gcd_step_nd`、`state_count_nd`、`mul_corners_nd`)，self_check 扩展至 14 项断言
- [x] 新建 `spass-bootstrap-core` crate：build.rs 用自家 parser/semantic/codegen 把 bootstrap.spass 编译为 Rust（保留 runtime prelude、剥 dispatch main、大栈线程防溢出），`NumericCore` trait 实现 + `install()`
- [x] `spass-infer/src/bootstrap.rs`: `NumericCore` trait + OnceLock 全局槽 + f64 精确域门槛 + 对拍（mismatch → panic）+ AtomicU64 统计；`state_count` / `bit_width` / `infer_add_sub` / `infer_mul` 全部接入
- [x] CLI `main` 安装核心；`spass build --verbose` 输出 `bootstrap core: N Spass call(s) adopted, M Rust fallback(s)`
- [x] 依赖设计：codegen 只出现在 build-dependencies，无包循环（infer 定义 trait，bootstrap-core 实现，CLI 安装）
- [x] 测试：175 cargo tests（3 个新桥测试：adopt/fallback/rational-step）+ 全示例 build + E2E（compute_b=77、self_check=1 等）
- [x] 实测 terrain 编译：**123 次 Spass 核心调用被采用，0 次回退**
- **状态**: completed

### 阶段 12: 规范剩余章节 (自举完成后继续)

#### 12a 小修与清欠
- [x] `for i in 0..n` 语义确认 — codegen 生成 `var < end`（左闭右开）且支持 `step`，符合规范 §5.1，无需改码
- [x] W002 警告 CLI 报告 — compile_sources / cmd_test 改用 `analyze_full`，按 §11.5 黄色粗体 `warning:` 输出
- **状态**: completed

#### 12b spasio socket 模式 (§9.2)
- [x] runtime 实现 TCP 帧协议（4 字节大端长度前缀 + 小端数据体 + 通道名路由），C (`runtime.c` WinSock/POSIX) 与 Rust prelude 双侧一致
- [x] E2E: `examples/socket`（echo_height input 方向 + send_height output 方向，外部对端）
- [x] 实测: input 收 1.5 ✓，output 发 height=7.5 且 Python 对端解码一致 ✓；单元测试 85 通过
- [ ] `ponytail` 升级路径: 常驻监听线程 + 每通道队列（当前单帧阻塞语义）；memory 模式仍为 stub
- **状态**: completed (MVP)

#### 12c 异步 spawn/parallel/await(task) (§5.4)
- [x] parser: `spawn f(args)` / `parallel f(args...)` / `await(expr)` 解析（ident 前瞻探测，含 `@tracked`/`@independent` 注解）—— AST 变体此前从未被构造
- [x] 类型系统: `Type::Task` 变体（spawn→task<返回类型>，parallel→task<list<返回类型>>；await 按 payload 还原 int/uint/bool/float/list；mangle 编码 `v`，C 类型 `uint64_t`）
- [x] codegen: 每个 spawn 兼容函数（纯 float/int 参数+返回）发射统一 `(const double[], double*)` thunk；站点发射 `spass_spawn(...)`/`spass_task_await(...)`；C 与 Rust 双侧
- [x] runtime: C（win32 `_beginthreadex`+Interlocked / POSIX pthread+`__atomic`，1ms 轮询）+ Rust prelude（thread+channel，无轮询）
- [x] CLI main 改用 512MB 大栈线程（bootstrap.spass 深递归编译在默认栈溢出；与 build.rs 同款修复）
- [x] `parallel` 批量启动（2026-11-14）：list 参数逐元素展开、标量重复，返回 task，await 得到 list<结果>；list 长度不等运行时终止；`if_present` 函数仍排除出 spawn 面（变体语义未定义）
- [x] E2E: examples/async `driver 3 4` → 10 ✓；178 tests 全绿；全部示例回归通过
- [ ] `ponytail` 升级路径: C 侧 condvar 替代轮询；未 await 的 task 泄漏 slot（升级 refcount）；vec/struct 参数的 typed arg-pack
- **状态**: completed (MVP)

#### 12d sequence 多序列 (§10.3)
- [x] parser: 顶层 `sequence name { body }` + `send(target, msg)` / `broadcast(msg)` / `receive [Pattern{cond,...}]`（ident 前瞻探测；`@await` 标记接受）
- [x] semantic: 序列注册为合成零参无返回函数（复用整条函数检查管线），body 常规类型检查；消息必须是 struct 值且字段 ≤8 个 float/int（f64 槽打包限制）
- [x] runtime: 每序列一条 FIFO 队列（编译期 id）；Rust prelude `Mutex<Vec<VecDeque>>` + yield 轮询 recv；C 侧环形缓冲 + 全局锁 + 1ms 轮询；`spass_seq_spawn/wait_all` 线程管理
- [x] codegen: dispatcher main 启动全部序列线程 + Drop guard join（arm 提前 return 也保证等序列终止）；per-struct pack/unpack helper；C 的 receive 为 let-RHS 专用语句级发射
- [x] E2E: examples/sequence — producer 发 3 条 Ping → consumer 阻塞接收求和 → spasio:output 写 result.spb = **30.0** ✓；179 tests 全绿；全部示例回归通过
- [ ] `ponytail` 升级路径: 每队列 condvar 替代轮询；非阻塞 receive（返回 Option）；@checkpoint 序列复用状态机；>8 字段/vec 字段消息需 typed 打包协议
- **状态**: completed (MVP)

#### 12e CLI daemon (§11.6)
- [x] `spass daemon` 监听 127.0.0.1:9100（`SPASS_SOCKET_HOST/PORT` 可覆盖），帧协议复用 §9.2 头（4 字节大端 + JSON body）
- [x] 模块缓存：每项目缓存编译产物 + TypedModule + src mtime 列表，未变化跳过重编译
- [x] `spass run` 默认走 daemon；未运行则 detached 自动启动（Windows `DETACHED_PROCESS|CREATE_NO_WINDOW`）；启动失败降级 `run_direct`（daemon 永不阻塞普通使用）；`--direct` 保持直跑
- [x] `spass daemon --status / --stop`（规范 11.6）
- [x] E2E: daemon 手动启动 → run add=4 → status 1 cached → stop；自动启动路径 ✓；--direct 回归 ✓；179 tests 全绿
- [ ] `ponytail` 升级路径: 请求并发 worker 池 + LRU 缓存淘汰；跨项目共享 daemon
- **状态**: completed (MVP)

#### 12f GPU / SPIR-V (§10.1)
- [x] 规范修订（2026-09-06）：@gpu 改为后端可插拔措辞——执行后端由运行器调度，MVP 为 CPU 线程池（复用 spawn/parallel 基础设施），SPIR-V 为未来后端；两后端数值结果一致
- [ ] SPIR-V 着色器生成 + 双后端一致性验证（见阶段 13e）
- **状态**: 规范完成，实现随阶段 13e

### 阶段 13: saple 运行器架构（规范已先行定义 §1.4/§2.5，2026-09-06）

架构决策：saple 由 spass 内置的 **saple 运行器**（常驻进程，daemon 的完整形态）运行；外部程序**永不** dlopen 库，只连接运行器交换数据——外部 set 变量 → 程序实时生效 → 函数实时计算修改后的值（§1.2 参数化差异记录落地）。规范 §1.3/§1.4/§2.5/§3.9/§9/§11.6/§15/§16 已完成修订。

- [x] 13a module.sps 槽位清单：semantic 收集程序内全部 spasio 通道引用（mode/channel/方向，去重）→ TypedModule.channels → `channel: <mode> <dir> <name>` 段写入 module.sps；运行器与外部程序据此知道哪些通道可 set/get
- [x] 13c set/get 协议升级为 **spasio 全面运行器化**（2026-09-06 第二轮）：运行器持有全部通道状态（file=磁盘槽位代理读写、socket/memory=内存槽位表 Mutex+Condvar）；程序内三模式统一为客户端 RPC（帧：opcode+mode+project+channel+f64，短连接）；程序进程永不碰文件系统、不监听端口；daemon per-connection 线程 + AtomicBool 优雅停止；外部 JSON set/get 支持可选 mode 字段对接同一状态。E2E 动态闭环 ✓（阻塞中的程序实时收到外部 set；程序 push → 外部 get；程序进程无监听端口）
- [x] 13b 常驻世界（最小形态，随游戏落地）：codegen main 加 `__sp_forever` 模式（sequences 运行至全部终止后退出）；daemon `start` 命令（编译缓存 + detached 启动常驻世界 + 记 PID，重复 start 重启世界）；daemon `stop` 收割世界进程。E2E：游戏 sequence 常驻运行 ✓。完全形态（多世界托管/资源监控）待做
- [x] 13d spasio-client 重定位：规范措辞已完成；实体客户端以外部脚本客户端与 Java（TreasureGame.java 手写帧协议）演示
- [ ] 13e @gpu CPU 后端调度：基础形态已达成（@gpu 按 CPU 发射 + E009/E011/E013 语义检查）；工作组→线程池映射与归约/广播原语待做（§16-10）
- **状态**: 13a/13b(最小)/13c/13d(演示) completed；13e 部分

### 阶段 14: 运行器收尾 + 语言欠账（2026-11-14）

- [x] `.saple` 运行器：cdylib 新增 `spass_module_wait`（join sequence 线程），`spass_runner::Module::wait`；`spass runner` 先确保 runner、再 start/call/wait，sequence 不再被当函数调用；参数严格数字解析；修复 sequence 包退出时的 `0xC0000005`
- [x] `module.sps` 确定性 schema：函数（symbol/param/result + domain/k/states/bits）、结构体字段、channel 段；`build/manifest.json` 与 `.saple` manifest 共用 `manifest_base`
- [x] list §3.2：`for x in xs` 解析、`.length`、索引、C `SpassList` 字节视图、Rust `Vec<T>`、字面量/空列表/嵌套列表；新增 examples/list
- [x] random §4.3：值模式与 `random(f,g)(args)` 函数模式；C 函数指针 compound literal / Rust if 链；xorshift64 运行时；新增 examples/random
- [x] parallel §5.4：list 参数批处理、标量重复、长度检查、await 收集为 list；新增 examples/parallel
- [x] bool 返回值的模块入口修复（playground cdylib 构建失败）
- **状态**: completed；list 作为函数局部值（不能返回/公开参数/struct 字段）

#### 其他已知欠账（低优先级）
- [ ] LLVM 后端 (inkwell + llc, 决策 1 升级路径)
- [ ] `spass build --platform=all`（当前只生成当前平台机器码）
- [ ] `@gpu` CPU 线程池调度（13e 的工作组映射/归约/广播）
- [ ] checkpoint wrapper 循环 resume 至完成（C 侧已循环；Rust 侧待核对）
- **状态**: pending

## 风险与缓解 (Risks)

| 风险 | 缓解 |
|------|------|
| LLVM 编译耗时 | 改用 C 发射后端 (决策 1) |
| 有理数性能 | 仅编译期使用，可接受 |
| 完整规范范围过大 | MVP v0.1 仅核心子集，分阶段扩展 |
| Windows MSVC 不可用 | 探测可用编译器（gcc/clang/MSVC），降级提示 |
| 自举位级一致难 | 比对生成的 C 中间表示哈希 |
