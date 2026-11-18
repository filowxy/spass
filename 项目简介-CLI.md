# Spass 命令行工具补充规范 v1.0

> 本文件是《项目简介》(v1.4) 的**附加扩展**。它不修改《项目简介》里的任何条款，
> 只补充**命令行工具链**（§11）的行为细节：进度提示、项目配置、运行器生命周期。
> 语言本身、spasio 协议（§9）、值域零运行时开销（§1.2）完全不变。

## 0. 与《项目简介》的关系

《项目简介》§2.2 已经规定了 `Spass.toml` 的键表，§11 规定了命令行工具链的子命令。
本文件回答的是那几节没有回答的问题：

- 编译要花几秒时，**用户在终端上看到什么**；
- `Spass.toml` 里的键**到底怎么生效**、冲突时听谁的；
- 运行器（守护进程）**什么时候在、什么时候不在**；
- **编译期优化**由谁开关、开关放在哪里、它会不会改变程序算出来的数；
- `.saple` 与 deeppack 两种产物形态**凭什么说它们是同一份程序**。

## 1. 进度提示

编译器有三个可能让用户等待的地方：调用 `rustc` 编译模块、通过运行器派发调用、
链接 deeppack 动态库。

等待的时候，终端上出现的是一行**仪表读数**，不是一个转圈的符号：

```
  compiling the module  ──────•▶············    4.2s
```

三个部分回答等待时用户真正会问的三个问题：

- **步骤名**：秒数花在哪里去了。步骤名不占固定列——每一步都是自己的一行，
  固定列只会在步骤名和刻度之间开一个空洞。
- **扫描头**：它在一把刻度上从左往右扫。**扫过的那一段填成实线并高亮**，
  前方是暗的点。方向因此一眼可辨：一条被点亮的长线加一个箭头，
  比一个在点阵里乱窜的箭头更像一台仪器在走。
  头紧跟其后的那个圆点是它刚离开的那一格，比实线暗一档——拖尾用同一族
  字形画出来才读作「回声」；换成另一种形状就成了「第二个箭头」。
- **计时器**：已经过去多久。字段是定宽的，所以读数不会在眼前挪位置。

那个填充段**不是百分比**：编译器不知道一个步骤要花多久。它标的是这次
扫描已经走过的部分，这也正是让方向可读的东西。

规则：

- **只在 stderr 是终端时动画**。重定向到文件、在 CI 里、在管道里时没有动画，
  步骤结束时打印一行摘要（步骤名 + 用时）。日志里不该有半截的动画帧。
- **150 ms 之后才出现**。一次 20 ms 的缓存命中不该闪一行——闪烁比安静更让人分心。
- **有颜色时才上色**。终端不认识 ANSI 转义时（Windows 控制台默认关闭
  VT 处理）读数退回纯字形版本，绝不把转义序列当字面文本打印出来。
- 提示写 **stderr**，不写 stdout：stdout 是程序的输出。
- 步骤结束时那一行被擦掉，换成 `✓ 步骤名  用时`（失败时是 `✗`）。
- **`rustc` 自己的诊断不会被动画吃掉**：它的 stderr 被单独接走，
  在读数擦掉之后原样转述。编译失败时用户必须看得到 rustc 说了什么。

`ponytail`：目前只有一档速度，没有进度百分比。编译进度是可以估的
（`rustc` 会报已完成百分比），但那要解析 `rustc` 的输出格式，脆。

## 2. 项目配置

`Spass.toml` 在**当前工作目录**（项目根）里被读取一次。

### 2.1 生效的键

| 键 | 作用 | 默认 |
|---|---|---|
| `build.output` | 产物目录（相对项目根） | `build` |
| `build.timeout` | 编译超时（秒） | `120` |
| `spasio.data_dir` | file 模式通道目录（相对项目根） | `data` |
| `spasio.socket_host` | 运行器地址 | `127.0.0.1` |
| `spasio.socket_port` | 运行器端口 | `9100` |
| `debug.log_level` | 编译器日志级别（`error`/`warn`/`info`/`debug`） | `warn` |
| `runner.idle_secs` | 运行器空闲多久后自己退出（秒） | `300` |

《项目简介》§2.2 的键表里还有 `build.platform`、`spasio.mode`、`spasio.error_policy`、
`gpu.*`、`debug.max_checkpoint_size`。它们**读到了但目前不产生行为**
（GPU 调度与检查点大小还没有实现），保留它们是为了文件本身合法。

### 2.2 优先级

**环境变量 > 项目配置 > 默认值。**

| 环境变量 | 覆盖 |
|---|---|
| `SPASS_SOCKET_HOST` / `SPASS_SOCKET_PORT` | `spasio.socket_host` / `spasio.socket_port` |
| `SPASS_DAEMON_IDLE_SECS` | `runner.idle_secs` |
| `SPASS_DATA_DIR` | `spasio.data_dir` |

### 2.3 读不懂怎么办

- **未知的键、未知的 TOML 构造**：忽略。用了本编译器不认识的 TOML 特性
  （数组、内联表、多行字符串）的项目仍然要能构建。
- **已知的键、但值用不了**：`build.timeout = "soon"`、`spasio.socket_port = 99999`、
  `debug.log_level = "loud"` —— **报错，不静默回落**。静默回落会把一个拼写错误
  变成一个「配置好像没生效」的问题，而那更难查。
- 文件不存在：不是错误，默认值就是一份合法配置。

`debug.log_level = "info"`（或 `"debug"`）等价于每次都带 `--verbose`：
`info` 打印构建摘要，`debug` 额外打印生效的配置。

## 3. 运行器的生命周期

运行器是 §9.2 的运行器的命令行形态：程序把通道请求交给它，它持有槽位。

- `spass run` / `spass test` 在**没有运行器在听**时会自己拉起一个，并打印
  `runner: started a local runner on 127.0.0.1:9100 (…)`。
- 运行器**比命令活得久**：它要服务后续的调用，也可能是外部程序的对接端。
- 运行器**空闲 `runner.idle_secs` 秒后自己退出**，有客户端连着时不退出。
- `spass daemon --status` 报告状态；**没有运行器在跑时这不是错误**，
  它回答「not running」并以 0 退出。
- `spass daemon --stop` 立刻结束它。

### 3.1 运行器不得占住调用方的输出

运行器是 CLI 启动的、**比 CLI 活得久**的子进程。Windows 会把父进程里每一个
「可继承的句柄」复制给子进程，而 shell 会把自己的管道交给 CLI——CLI 从来不会
提到那些句柄，于是它们照样被继承。

结果：`spass run … | tee log` 里的那条管道**一直被运行器攥着**，
`tee` 永远等不到 EOF，整条命令看起来像是挂住了，直到运行器空闲退出。
实测：一个 45 秒的空闲窗口会让命令多等 45 秒。

因此：**运行器只继承自己的标准句柄（NUL 设备），其余一个句柄都不继承。**
实现见 `compiler/crates/spass-cli/src/win_spawn.rs`：用 `CreateProcessW` 以
`bInheritHandles = FALSE` 创建子进程，标准句柄显式交给 NUL 设备。

只清掉三个标准句柄上的「可继承」标志**不够**：shell 交给 CLI 的那些管道
不在其中。

回归验证：`compiler/scripts/verify-runner.ps1`。

## 4. 编译期优化渠道

优化是一条**独立的渠道**，默认**全关**。它改变的是编译器**算出什么**，
不是它跑得多快——所以「关」必须等于「一个字都不变」。

### 4.1 两条入口

```powershell
spass build --optimize                  # 打开优化，走标准产物
spass build --no-optimize               # 强制关闭
spass build --optimize-level 3          # 指定档位
spass build --diagnostics               # 逐项列出被改了什么
spass code-optimize --optimize-level 2  # 独立渠道：产物写到 build/optimized/
```

`spass code-optimize` **不覆盖**常规产物。它把优化后的 `module.c`、
`module.rs`、编译好的 `module_rs.exe` 和一份 `optimize-report.json`
写进 `build/optimized/`，常规构建的产物原封不动。

理由是优化**有破坏性**（§4.5）：它是拿「编译器算的数」换「运行时算的数」。
两条渠道并列，用户才能把两份产物放在一起比。

### 4.2 五个开关与四个档位

| 开关 | 作用 |
|---|---|
| `readonly_locals` | 从未被重新赋值的 `let` 绑定不再声明为可变 |
| `inline_single_use` | 只用一次的常量绑定被搬进使用点 |
| `fold_constants` | 初始化式在编译期可知的绑定被替换成字面量 |
| `const_fold_arith` | 编译期可知的算术表达式被折叠成常量 |
| `dead_code_elim` | 没有任何引用、也不导出的函数不被发射 |

| 档位 | 打开的开关 |
|---|---|
| 0 | 无（**默认**） |
| 1 | `readonly_locals` |
| 2 | 1 + `inline_single_use` + `fold_constants` + `const_fold_arith` |
| 3 | 2 + `dead_code_elim` |

`dead_code_elim` 只在档位 3，因为它**会让一个符号消失**：
按名字查符号的宿主会查不到。

### 4.3 什么算「声明后不再变」

一个绑定被硬编码成常量，要同时满足五条：

1. 它是 `let` 绑定（不是参数、不是 `var` 之类的其它形式）；
2. 它的初始化式**在编译期可知**；
3. 它**从未被 `Assign`**——名字级别判定：同名绑定里只要有一个被赋值，
   这个名字就不算只读；
4. 它**不逃逸**——没有被按引用交出去、没有进通道、没有进结构体；
5. 它**不跨 `@await`**。

含 `@await` 或 `@checkpoint` 的函数**整体跳过**：它们的控制流在
`@await` 处分叉，编译期看不到后面会读哪个值。

### 4.4 配置放在哪里

优化器**不是项目的事**，所以它的开关**不在 `Spass.toml` 里**——
它放在 Spass 自己的、每用户一份的配置文件里：

| 平台 | 路径 |
|---|---|
| Windows | `%LOCALAPPDATA%\Spass\config.toml` |
| 其它 | `$XDG_CONFIG_HOME/spass/config.toml`，或 `~/.config/spass/config.toml` |

```toml
[optimize]
level = 2
max_fold_depth = 8
diagnostics = false

# 或者逐个开关写；它们比档位优先。
readonly_locals = true
inline_single_use = true
fold_constants = true
const_fold_arith = false
dead_code_elim = false
```

优先级：**命令行 > 环境变量 > 本体配置文件 > 默认（全关）**。
环境变量 `SPASS_OPTIMIZE` 收一个档位（`0`..`3`）或一串开关名
（`readonly_locals,fold_constants`）。

**项目里的 `Spass.toml` 出现 `[optimize]` 段是错误**，不是静默忽略。
一个用户以为优化已经打开而它没有，比报错难查得多。

### 4.5 为什么默认全关

优化器**可能改变程序算出来的数**。

- 整数运算用 `checked_*`，溢出是**暴露出来的**，不是被绕过去的；
- 除法**不折叠**；
- 浮点折叠用的是**精确有理数**：`0.1 + 0.2` 折成 `3/10`，
  而运行时按 IEEE-754 算出来的是 `0.30000000000000004`。
  两者**可能差 1 ULP**。

这不是缺陷，是取舍：编译期折叠换来的确定性，代价是它不再是
运行时那条路径的逐位复制。所以它默认关，打开时**必须可见**：
`--diagnostics` 逐项列出被折叠、被内联、被改成只读、被删掉的每个名字。

## 5. 打包模式的一致性

`.saple`（§2.4）与 deeppack（见《项目简介-Deeppack》）是**同一份机器码的两个容器**。

### 5.1 两种容器，一份机器码

`spass build` **总是**编译出机器码：

```
build/
  spassfile.saple            ZIP 容器
  spass_module.dll           同一份机器码（散装）
  module.c / module.rs       源码形态
```

`.saple` 里的 `native/<平台>.<扩展名>` 与 `spass_module.dll`
**逐字节相同**。`--deeppack` 只额外写一份 ABI 清单，不重新编译任何东西。

`.saple` 里还带两样东西：

- `native/libunwind.dll`——模块链接的 unwinder。加载器在模块**旁边**解析它，
  没有它模块根本加载不了（`STATUS_DLL_NOT_FOUND`）；
- `native/source.c`——给需要为别的平台重新编译模块的宿主做参考。

### 5.2 模块的统一入口

模块除了按名字修饰规则导出各个函数，还导出**三个统一入口**：

| 符号 | 作用 |
|---|---|
| `spass_module_start` | 启动模块声明的 `sequence` |
| `spass_module_wait` | 等这些 `sequence` 结束 |
| `spass_module_call` | 按符号名调用一个函数 |

有了它们，**不认识 Spass 签名的加载器也能调用模块**：
调用方只需要知道符号名和结果槽位数，不需要知道任何参数类型。
这正是 `.saple` 运行器能做到「装一个它没编译过的包」的原因。

参数语义与命令行一致：值域参数取**状态索引**，其余取原值；
结果槽位是 `out` 参数（按声明顺序）在前、返回值在后。

### 5.3 sequence 的启动与汇合

一个程序在 `main` 里启动自己的 `sequence`。一个库没有 `main`，
所以**没有 `spass_module_start` 就没有 sequence 会跑**。

反过来，**拥有模块生命周期的加载器必须在卸载前调用 `spass_module_wait`**：
sequence 的线程属于模块，模块被卸载时线程还在跑就是访问冲突。
`spass runner` 的顺序因此是固定的：**启动 → 调用 → 汇合**，
而且**调用失败也必须汇合**——错误在汇合之后才报出来。

### 5.4 运行器地址

模块通过 spasio 协议和运行器说话（§9.2），而运行器的地址是**运行期的事实**：
模块在自己进程里找到的环境变量，就是运行器必须在的地方。

`.saple` 的 `manifest.json` 与 deeppack 的 ABI 清单都带一个 `runner` 段：

```json
"runner": {
  "start_symbol": "spass_module_start",
  "call_symbol": "spass_module_call",
  "has_sequences": true,
  "host_env": "SPASS_SOCKET_HOST",
  "port_env": "SPASS_SOCKET_PORT",
  "default_host": "127.0.0.1",
  "default_port": 9100
}
```

宿主据此把模块放到运行器所在的环境里，再加载它。

### 5.5 saple 运行器与文件关联

§2.5 说 saple 运行器是 saple 的**唯一运行入口**，外部程序永不
`dlopen`/`LoadLibrary` 包里的库。命令行上因此有**两种形态**的运行器：

| 形态 | 命令 | 生命周期 |
|---|---|---|
| 常驻 | `spass daemon` | 服务后续调用，空闲后自己退出 |
| 一次性 | `spass runner <file.saple> [函数] [参数…]` | 跑完这一次就退出 |

一次性形态是**文件关联**的落点：双击一个 `.saple` 就是它。
安装器（MSI 与 `spass-setup`）在用户的注册表里登记：

```
HKCU\Software\Classes\.saple                             → spassfile.saple
HKCU\Software\Classes\spassfile.saple                    → Spass Package
HKCU\Software\Classes\spassfile.saple\shell\open\command → "<安装目录>\spass.exe" runner "%1"
```

卸载时这三个键被删除，机器恢复原状。登记在 `HKCU` 下，
所以安装**不需要提权**，也不会触发 UAC。

`spass runner` 装载一个包的步骤：打开 ZIP → 读 `manifest.json` →
取本平台的 `native/`（连同旁边的 unwinder）→ 加载 → 启动 `sequence` →
按需调用函数 → 汇合。

`module.sps` 是**数据模式**：通道的类型、值域、位宽、数组长度。
它描述的是程序自己的数据模型，由程序拥有；一次性运行器调度函数时用的是
`manifest.json` 里的函数表与 `runner` 段，所以它不读 `module.sps`。
§2.5 里那套「读 `module.sps` 建立对外可见的数据模式」是**常驻**运行器
（`spass daemon`）为外部程序服务时做的事。

### 5.6 一致性怎么验

「编译成功」不是证据，「三种方式调用同一个函数得到同一个数」才是。

`compiler/scripts/verify-packaging.ps1` 对同一个项目的每个导出函数
跑三遍：

- **program**：`spass run --direct`（模块里的 dispatcher）；
- **saple**：`spass runner`（从包里解出机器码再加载）；
- **deeppack**：宿主进程自己加载 `spass_module.dll`，调统一入口。

三个结果必须逐位相同，槽位数必须与清单里的 `results` 相同。
另外两项单独验：`sequence` 在包里要跑完并落盘，spasio 通道的值
在两种模式下都要穿过运行器到达。
