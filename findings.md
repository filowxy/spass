# Spass 编译器研究记录 (findings.md)

## 规范分析

### 核心架构 (第 12 节)
- **前端**: 词法 → 语法 (含 include 展开) → 语义分析
- **中端**: 值域推断、@dispatch 展开、if_present 裁剪、约束图、精度裁剪
- **后端**: CPU 路径走 LLVM, GPU 路径走 SPIR-V, 产物打包为 .saple

### 值域推断是 Spass 的"灵魂" (第 3.5, 12.3 节)
- 核心算法: 伪代码 `InferRange(op, range_a, range_b)`
- ADD/SUB: `step = gcd(step_a, step_b)` 精确
- MUL: 四角枚举 min/max, step 保守
- DIV: 编译期禁止 (仅常量 2^k 右移允许)
- 所有运算在有理数域，避免 IEEE 754 误差

### 编码模型 (第 3.5 节)
- `value = min + step * N`, N 是 uintN
- 即使 min 为负数，N 仍是非负整数
- 消除符号位管理, GPU 用原生 UADD/UMUL
- 例: `float[-5.0, 5.0, 0.1]` → state_count=101, N∈[0,100]

### 名字修饰 (第 2.5 节)
- 格式: `_spass_<函数名>_<参数数量>_<参1编码>_...`
- 解析: 从右向左反向扫描
- 关键: 函数名可包含 `_` 而无歧义
- 编码表: f=float32, d=float64, i=int32, l=int64, u=uint32, v=uint64, b=bool, 2f=vec2 等

### .saple 文件结构 (第 2.4 节)
- ZIP 格式
- 内部: manifest.json + native/<platform>.dll + module.sps
- 外部程序通过 spasio-client 库加载

### spasio-client 信任边界 (第 1.4, 9.5 节)
- 内部计算零运行时检测
- 边界处 (spasio-client) 截断越界值 `N = clamp(N, 0, state_count-1)`
- 防御不违背"零运行时开销"原则

## 错误码体系 (第 14 节)

### 错误 E001-E015
- E001: 值域声明无效
- E002: @dispatch 参数绑定缺失
- E003: 循环包含
- E004: 循环依赖
- E005: 变量未定义
- E006: 类型不匹配
- E007: 传递点重复/未定义
- E008: 递归函数
- E009: @gpu + 传递点冲突
- E010: 位宽超 128 位
- E011: GPU 共享内存超限
- E012: 函数重名
- E013: @gpu + @await 冲突
- E014: @dispatch 有值标签筛选无匹配
- E015: 有值标签键名重复

### 警告 W001-W004
- W001: 精度丢失
- W002: @dispatch 参数未使用
- W003: 变量使用默认值域
- W004: 128 位分配警告

## 设计决策追溯 (第 15 节)

| 决策 | 原因 |
|------|------|
| 定点数用 uintN N 编码 | 消除符号位, GPU 用原生指令 |
| step 限 2/10 负幂 | 有理数精确 |
| 中间值域不受 E001 | 编译期可保守推断 |
| 禁递归 | 传递点替代 |
| 乘法保守策略 | 精确推断 NP 难, O(1) 近似 |
| 128 位上限 | 现代 SIMD 寄存器宽度 |
| 自举限于后端 | 字符串/图算法超出 Spass 边界 |
| 名字修饰反向扫描 | 消除函数名含 `_` 歧义 |
| if_present 编译期 | 零运行时开销 |

### 阶段 11 自举集成发现 (2026-09-05)

- **包循环约束**: `spass-codegen` 主依赖 `spass-infer`（值域编码用 `Range`），因此任何包的 build.rs 都不能引 codegen——除非该包不在 codegen 的依赖闭包里。解法：独立 crate `spass-bootstrap-core`（主依赖 infer，build-deps 引 codegen），infer 定义 `NumericCore` trait + OnceLock 全局槽，CLI 安装。依赖注入而非构建期耦合。
- **build script 栈溢出**: build.rs 内跑 parse→semantic→codegen 在 Windows 默认栈上溢出（STATUS_STACK_OVERFLOW），即便 spass.exe 跑同一管线正常。解法：`std::thread::Builder::stack_size(512MB)` + spawn（rustc 自身同款做法）。
- **include! 的 inner attribute 非法**: 生成码开头的 `#![allow(...)]` 粘贴进 `mod gen { include!(...) }` 会报错，build.rs 需剥离（外层 mod 自带 allow）。
- **int 字面量混算间隙**: Spass 源 `x / 2`（x: float）语义通过，C 后端靠整数提升侥幸可用，Rust 后端发射 `2i32` 导致 `f64 / i32` 编译失败。临时绕开（核心库写 `2.0`），**升级路径：emitter 按上下文把整数字面量强转为 float**（阶段 12 候选）。
- **位宽语义分歧**: Rust 原实现 1 状态 → 1 bit；Spass 数学版 ceil(log2(1)) = 0。桥对 n ≤ 1 跳过（Rust 约定主导），n ≥ 2 时两边位级一致。
- **num/den 桥无损性**: 有理数以 `(numer, denom)` 整数对跨 f64 桥（各分量及中间积 ≤ 2^53 时精确），E001 保证 step 是 2/10 负幂 → 实际项目 100% 覆盖（terrain 123/123 采用，0 fallback）。
- **self_check 未约分预期**: `rat_add_nd(1,10,1,2)` = (12,20) 非 (6,10)——桥设计为不约分，由 Rust 端 `Rational64::new` 规范化。

### 运行器架构决策 (2026-09-06 规范修订)

- **规范与实现脱节的典型案例**：规范 §3.9 写"let 不可变"，但实现（Assign 语句 + `let mut`/C 变量）和规范自己的 §5.2 示例（`h = h - ...`）早已支持重赋值。修订原则确立为**规范对齐实现**：let 可变是 §1.2"参数化差异记录/动态变化"的语言基础，无需独立 set 语法。
- **架构级转折：saple 运行器**。旧 §2.5 的"外部程序 dlopen 库"形态下 Spass 程序没有自己的生命周期，"动态变化"无法成立。新架构：运行器（spass 内置常驻进程，daemon 的完整形态）是 saple 唯一运行入口，外部程序只与运行器沟通——set 变量 → 程序实时生效（§3.9）→ 函数实时计算修改后的值。12e daemon 已是雏形，落地路线登记为阶段 13。
- **@gpu 的正确抽象层级**：用户问"GPU/SPIR-V 能否用 CPU 多线程平替"——答案是 @gpu 声明的是**并行语义而非硬件**。规范改为"执行后端由运行器调度（MVP CPU 线程池 / SPIR-V 未来后端），两后端数值结果一致"。当前实现（codegen 把 @gpu 当普通 CPU 函数发射）事实上就是 CPU 后端，语义检查（E009/E011/E013）保留。SPIR-V 从"必须做的后端"降级为"运行器的未来调度目标"，12f 工程风险大幅下降。
- **钳制时机的诚实措辞**：§3.9 的值域钳制写为"ABI 边界强制（参数/返回值/spasio 通道/运行器 set）"，函数体内重赋值用精度无损的 f64 中间表示不即时钳制——与实现一致（Assign 无即时 clamp，out 参数/返回值路径有 spass_clamp）。
