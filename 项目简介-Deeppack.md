# Spass Deeppack 扩展规范 v1.0

> 本文件是《项目简介》(v1.4) 的**附加扩展**。它不修改《项目简介》里的任何条款，
> 只在其上增加**一种编译产物形态**。核心设计——spasio 协议（§9）、saple 运行器
> 中介（§2.5）——完全不变。

## 0. 解决的问题

《项目简介》§1.3 定义 `.saple` 是编译产物，§1.4 规定外部程序通过 spasio 与
**saple 运行器**交互、「外部程序**永不**直接加载 saple 内的库」。

这套模型对「宿主是一个需要长期驻留的世界」是对的——运行器持有通道槽位、
裁剪越界输入、维持 `sequence` 的生命周期。但它对另一类宿主不适用：

- 宿主是一个游戏引擎 / 工具 / 编辑器，**自己就有进程和渲染器**，
  想把 Spass 的计算结果直接拿进自己的渲染管线；
- 宿主希望**同进程调用**，省掉一次 IPC 和一次数据编解码；
- 宿主想自己决定 Spass 产物的加载时机，而不是交给运行器。

deeppack 就是为这类宿主准备的产物形态：**同一个编译管线，产出一个共享库**。

## 1. 产物

```powershell
spass build --deeppack
```

在常规构建产物之外，额外产出三个文件：

| 文件 | 内容 |
|---|---|
| `build/spass_module.dll` | 编译好的模块，Rust `cdylib`。公开函数按**精确符号名**导出 |
| `build/spass_module.abi.json` | 导出符号的 ABI 清单（见 §3） |
| `build/libunwind.dll` | Rust 工具链的 unwinder。共享库链接了它，宿主必须能解析到 |

`--deeppack` 是**附加**的，不是替换：`.saple` 照常产出，运行器中介的流程不受影响。

**共享库和包里的机器码是同一份。** `spass build` 总是编译一次机器码，
`build/spass_module.dll` 与 `.saple` 里的 `native/<平台>.<扩展名>`
**逐字节相同**；`--deeppack` 只是多写一份清单，不重新编译。
两种产物形态因此不可能出现「同一份源码、两个不同的数」。

## 2. 与运行器的关系

deeppack 只改变**产物形态**，不改变程序与外界的数据通路：

- 程序若使用 spasio 通道，通道仍然由运行器托管，程序仍然通过 §9 的帧协议
  与运行器通信。**deeppack 的模块和普通模块在通道行为上完全一样。**
- deeppack 省掉的是「宿主 → 运行器 → 程序」这一层里的**前一段**：
  宿主直接拿到函数的地址直接调，不再经过运行器的 JSON/帧封装。

换句话说，deeppack 回答的是「宿主怎么拿到函数」，
而不是「程序怎么拿到数据」。

## 3. ABI

### 3.1 符号

模块的公开函数按 `_spass_<名字>_<参数个数>_<参数类型>...` 的形式导出，
例如 `func height(x: float, z: float): float` 导出为 `_spass_height_2_f_f`。
导出带 `#[no_mangle]`，因此符号名就是字面量，宿主用
`GetProcAddress` / `dlsym` 就能解析，不需要知道 Rust 的 mangling 规则。

`fileclude` / `ownclude` 的私有符号**不导出**（§7.2 的可见性规则不变）。

### 3.2 值域编码（§3.5）

带值域的参数在 ABI 上是**编码后的状态索引**，不是 `double`：

```spass
func height_quant(h: float[0.0, 380.0, 0.1]): float[0.0, 380.0, 0.1]
```

导出为 `_spass_height_quant_1_f(__h: u16) -> u16`。宿主必须：

1. 编码：`state = clamp(round((v - min) * k), 0, state_count - 1)`，
   其中 `k = 1/step` 是编译器算好的精确倒数；
2. 调用；
3. 解码：`v = min + step * state`。

清单里给出了每个域的 `min` / `max` / `step` / `k` / `state_count` / `bits`，
宿主不需要自己推导。

### 3.3 out 参数（§3.7）

`out` 参数按指针传递，**不量化**：槽位里放的是编码后的状态索引（若该参数带值域），
宿主按 §3.2 解码。

### 3.4 结构体与多返回值（§3.3 / §3.8）

结构体的每个字段都是一个 `f64`，整个结构体是 `#[repr(C)]`，
所以布局就是声明顺序。清单里的 `structs` 段按顺序列出字段名。
多返回值以 `SpassRet<N>` 结构体返回。

### 3.5 清单

`spass_module.abi.json` 长这样：

```json
{
  "module": "spass_module",
  "version": "0.1.0",
  "compiler": "0.1.0",
  "artifact": "spass_module.dll",
  "artifact_kind": "cdylib",
  "abi": "c",
  "requires": ["libunwind.dll"],
  "functions": [
    {
      "name": "height_quant",
      "symbol": "_spass_height_quant_1_f",
      "results": 1,
      "params": [
        {
          "name": "h",
          "type": "uint16_t",
          "is_out": false,
          "domain": { "min": 0.0, "max": 380.0, "step": 0.1, "k": 10.0,
                      "state_count": 3801, "bits": 12, "type": "uint16_t" }
        }
      ],
      "returns": { "type": "uint16_t", "domain": { "...": "..." } }
    }
  ],
  "structs": [{ "name": "Point", "fields": ["x", "y", "height"] }],
  "runner": { "...": "..." }
}
```

`functions[].results` 是**一次调用填几个结果槽**：`out` 参数按声明顺序在前，
返回值在后。带 `out` 参数的函数没有可用的返回值，槽位就只有那些 `out` 参数。
清单里的这个数与生成代码的 dispatcher 是同一套规则，宿主按它分配缓冲区。

`runner` 段见 §3.7。

### 3.6 模块的统一入口

除了按名字修饰规则导出各个函数，模块还导出**三个统一入口**：

| 符号 | 签名 | 作用 |
|---|---|---|
| `spass_module_start` | `() -> int` | 启动模块声明的 `sequence`；`0` 表示启动成功 |
| `spass_module_wait` | `()` | 等这些 `sequence` 结束 |
| `spass_module_call` | `(symbol, symbol_len, args, argc, outs, out_cap) -> int` | 按符号名调用一个函数 |

`spass_module_call` 的参数：符号名是 UTF-8 字节（`symbol_len` 字节长，
**不带结尾的 NUL**）；`args` 是数值参数；`outs` 接收结果
（`out` 参数在前、返回值在后）；`out_cap` 是 `outs` 的容量。
返回值 `0` 表示成功，`-1` 表示符号不认识。

参数语义与命令行一致：值域参数取**状态索引**，其余取原值。

**为什么要有统一入口**：函数签名是编译器的决定，一个**不认识 Spass 签名**的
加载器没法按名字逐个调用函数。有了统一入口，加载方只需要知道
符号名和槽位数——`.saple` 运行器因此能装载一个它没编译过的包。

### 3.7 sequence 的启动契约与运行器地址

**启动契约**：一个程序在 `main` 里启动自己的 `sequence`。一个库没有 `main`，
所以**没有 `spass_module_start` 就没有 sequence 会跑**。宿主加载模块后
必须调用它。

**汇合契约**：**拥有模块生命周期的宿主必须在卸载前调用 `spass_module_wait`**。
sequence 的线程属于模块，模块被卸载时线程还在跑就是访问冲突。
顺序因此是固定的：**启动 → 调用 → 汇合**，而且**调用失败也必须汇合**。

**运行器地址**：模块通过 spasio 协议和运行器说话（§9.2），
而运行器的地址是**运行期的事实**——模块在自己进程里找到的环境变量，
就是运行器必须在的地方。清单的 `runner` 段把这件事说清楚：

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
`has_sequences` 让宿主知道**必须**调 `spass_module_start`：
没有 sequence 的模块调它也无害（返回 `0`），但宿主不必猜。

## 4. 宿主的用法

知道函数签名时，宿主直接按符号调：

```c
HMODULE lib = LoadLibraryA("spass_module.dll");
double (*height)(double, double) =
    (double (*(*)(double, double)))GetProcAddress(lib, "_spass_height_2_f_f");
double h = height(0.5, 1.5);
```

不知道签名时（`.saple` 运行器就是这样），宿主用统一入口：

```c
typedef int (*call_fn)(const uint8_t *symbol, size_t symbol_len,
                       const double *args, size_t argc,
                       double *outs, size_t out_cap);
call_fn call = (call_fn)GetProcAddress(lib, "spass_module_call");

double outs[1];
call((const uint8_t *)"_spass_height_2_f_f", 18, args, 2, outs, 1);
```

加载失败时先查 `requires`：`libunwind.dll` 必须能在搜索路径上找到，
否则 `LoadLibrary` 会以 `STATUS_DLL_NOT_FOUND` 失败——`build-msi.ps1` 和
`spass build --deeppack` 都会把它放在共享库旁边。

模块若声明了 `sequence`，宿主在调用之前调 `spass_module_start`，
在卸载之前调 `spass_module_wait`（§3.7）。

## 5. 信任边界

《项目简介》§1.4 把「运行器截断越界值」定义为信任边界上的防御。
deeppack 的模块跑在**宿主的进程里**，宿主因此就是信任边界：

- 运行器那道截断**不适用**于 deeppack 调用；
- 值域的正确性从「运行器保证」变成「宿主自己负责编码」；
- 清单给出了编码所需的全部信息，但编译器不会替宿主检查它编码得对不对。

这是把边界从「运行器 ↔ 程序」挪到「宿主 ↔ 程序」的必然结果，
不是缺陷。需要强隔离的宿主应当继续用 `.saple` + 运行器。

## 6. 与《项目简介》的对应

| 《项目简介》 | deeppack 的关系 |
|---|---|
| §1.3 编译产物（`.saple`） | 增加第二种产物形态；`.saple` 不变 |
| §1.4 外部程序如何与 Spass 交互 | **不变**。deeppack 只回答「宿主怎么拿到函数」 |
| §2.4 编译产物：saple 文件 | 不变。包里的机器码与 deeppack 的共享库是同一份 |
| §2.5 saple 运行器 | 不变，仍是通道与槽位的托管者；运行器也用统一入口装载包 |
| §10.3 sequence | 库形态下由 `spass_module_start` 启动、`spass_module_wait` 汇合 |
| §11.1 命令列表 | `spass build` 增加 `--deeppack` 选项 |

## 7. 参考实现与验证

- 实现：`compiler/crates/spass-cli/src/deeppack.rs`（产物与清单）、
  `compiler/crates/spass-codegen/src/abi.rs`（ABI 描述）、
  `spass_codegen::RustFlavor::Cdylib`（产物形态）、
  `compiler/crates/spass-runner/`（统一入口的加载器）。
- 验证：`spass build --deeppack` 之后，用宿主进程直接加载 DLL 并调用导出的
  符号——普通函数、值域编码路径（`uintN` 状态索引进出）、多返回值结构体
  三条路径都要验。**编译成功不是证据，宿主调通才是。**
- 一致性验证：`compiler/scripts/verify-packaging.ps1` 对每个导出函数
  跑三遍（程序 dispatcher、`.saple` 运行器、deeppack 宿主），
  三个结果必须逐位相同；`sequence` 与 spasio 通道另外各验一遍。
