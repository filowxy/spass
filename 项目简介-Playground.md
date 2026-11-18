# Spass Playground 扩展规范 v1.0

> 本文件是《项目简介》(v1.4) 的**附加扩展**。它不修改《项目简介》里的任何条款，
> 只在其上增加一层**可选的宿主能力**。核心设计——spasio 协议（§9）、saple 运行器
> 中介（§2.5）、值域零运行时开销（§1.2）——完全不变。

## 0. 与《项目简介》的关系

《项目简介》§1.5 写明「Spass 不是渲染引擎」「不提供无限制的文件系统访问」。
本文件定义的能力与这两条并不矛盾，理由是它**必须显式启用**：

| | 不使用 playground 的程序 | 显式调用 `s2d:` / `s3d:` / `spio:` 的程序 |
|---|---|---|
| 像素 | 产物里没有任何光栅化代码 | 拥有自己的窗口与帧缓冲 |
| 文件系统 | 没有文件访问入口（§1.5 成立） | 拥有 `spio:` 的文件读写 |
| 编译器可见性 | — | 构建产物里链接了 playground 运行时（§11.5 会列出） |

也就是说：**§1.5 描述的是默认语言边界；playground 是程序显式选择的越界。**
不使用这些命名空间的项目，产物与 §1.5 完全一致。

命名空间 `s2d` / `s3d` / `spio` 与 `spasio` 同形，都是语法级关键字，
只能出现在 `ns:func(args)` 的位置，不能作为结构体或字段名。

## 1. 坐标系与窗口

- `s2d:window(w, h, title)` 创建一个 `w × h` 像素的窗口。`s2d:` 与 `s3d:`
  **共享同一个窗口**，但**不共享同一个帧缓冲**。
- 2D 坐标原点在**左上角**，x 向右、y 向下，单位是像素。
- 颜色分量为 `0.0 .. 1.0` 的浮点数，写入帧缓冲时量化为 `round(v * 255)`。
- 不透明度 `a` 也是 `0.0 .. 1.0` 的浮点数：0 完全透明，1 完全不透明。

### 1.1 两个图层

帧缓冲有两层：

| 层 | 谁往里画 | 合成 |
|---|---|---|
| 3D 层 | `s3d:` 图元（带深度缓冲） | 底 |
| 2D 层 | `s2d:` 图元（每像素带 alpha） | **永远叠在 3D 层之上** |

`present` 时合成：`out = 2D 不透明处取 2D，半透明处按 alpha 混合，其余取 3D`。

由此得到两条与旧版不同的性质：

- **2D 与 3D 的绘制顺序无关紧要。** 2D 永远在上面，所以「先画 3D 再画 2D」
  和「先画 2D 再画 3D」得到同一帧。旧版把 2D 和 3D 画进同一个缓冲，
  顺序错了 2D 就会消失，而运行时只能在事后打一条警告；现在这个错误不存在了。
- **2D 图元之间仍然按调用顺序覆盖。** 同一层里后画的盖住先画的，
  和所有 2D 图形库一样。

一帧的典型顺序：

```
s2d:clear(...)      // 清空整帧（s2d:clear 与 s3d:clear 都清空两层）
draw_2d()           // 2D 覆盖层：先画后画都一样
draw_3d()           // 3D 场景：先画后画都一样
s2d:present()       // 合成两层并显示
```

`clear` 清的是**两层**：2D 覆盖层是这一帧的一部分，不是另一块屏幕。

## 2. spass2d（`s2d:`）

软件光栅化的 2D 层。

| 函数 | 签名 | 说明 |
|---|---|---|
| `s2d:window` | `(w: float, h: float, title: string) -> bool` | 创建窗口 |
| `s2d:open` | `() -> bool` | 泵消息、刷新输入；返回 false 表示窗口已关闭，程序应当退出 |
| `s2d:present` | `() -> bool` | 合成两层并显示本帧，同时计时；返回 false 表示应当结束 |
| `s2d:clear` | `(r, g, b: float) -> bool` | 用一个颜色填满整帧（两层都清） |
| `s2d:pixel` | `(x, y, r, g, b, a: float) -> bool` | 画一个点 |
| `s2d:line` | `(x0, y0, x1, y1, r, g, b, a: float) -> bool` | 画一条线（Bresenham） |
| `s2d:rect` | `(x, y, w, h, r, g, b, a: float) -> bool` | 填充矩形 |
| `s2d:frame` | `(x, y, w, h, r, g, b, a: float) -> bool` | 矩形边框 |
| `s2d:circle` | `(cx, cy, rad, r, g, b, a: float) -> bool` | 填充圆 |
| `s2d:ellipse` | `(cx, cy, rx, ry, r, g, b, a: float) -> bool` | 填充椭圆（轴对齐） |
| `s2d:tri` | `(x0, y0, x1, y1, x2, y2, r, g, b, a: float) -> bool` | 填充三角形 |
| `s2d:text` | `(x, y, scale, r, g, b, a: float, text: string) -> bool` | 画一行位图文字 |
| `s2d:text_width` | `(text: string, scale: float) -> float` | 最宽一行的推进宽度（像素） |
| `s2d:text_height` | `(scale: float) -> float` | 一行文字的高度（像素） |
| `s2d:label` | `(x, y: float, text: string, a: float) -> bool` | 立即模式 UI：标签 |
| `s2d:button` | `(x, y, w, h: float, label: string, a: float) -> bool` | 立即模式 UI：按钮，返回是否被点击 |
| `s2d:slider` | `(x, y, w, h, v, lo, hi, a: float) -> float` | 立即模式 UI：滑块，返回新值 |
| `s2d:checkbox` | `(x, y: float, label: string, on: bool, a: float) -> bool` | 立即模式 UI：复选框 |
| `s2d:mouse_x` / `mouse_y` | `() -> float` | 鼠标位置 |
| `s2d:mouse_dx` / `mouse_dy` | `() -> float` | 鼠标位移 |
| `s2d:mouse_down` | `() -> bool` | 左键是否按下 |
| `s2d:wheel` | `() -> float` | 滚轮增量 |
| `s2d:key` | `(key: string) -> bool` | 指定按键是否按下 |
| `s2d:time` | `() -> float` | 启动以来的秒数 |
| `s2d:dt` | `() -> float` | 上一帧到这一帧的秒数 |

`a` 是显式参数而不是状态式开关（没有 `opacity()` 那样需要配对的
`push/pop`）：API 一致，没有隐藏状态，一个部件（按钮、滑块、复选框）的
`a` 作用于整个部件。

### 2.1 文字度量

`text_width` 与 `text_height` 是**度量**，不是状态：它们回答「这段文字在屏幕上
占多大地方」，好让调用方按文字的实际大小画底板、排版。

字形格是 **6×8** 像素（5 列笔画 + 1 列间距），所以 `scale = 1` 时一行
*n* 个字符宽 *6n* 像素、高 8 像素。`scale` 被截断成整像素（至少 1），
这与绘制时用的规则一致——**度量必须和画出来的东西对得上**。

`text_width` 取**最宽的一行**：换行符开始新的一行，其余行各自算宽度。

## 3. spass3d（`s3d:`）

带 z 缓冲的 3D 层，与 `s2d:` 共用窗口与帧缓冲。

| 函数 | 签名 | 说明 |
|---|---|---|
| `s3d:window` | `(w, h: float, title: string) -> bool` | 同 `s2d:window`（同一个窗口） |
| `s3d:open` / `s3d:present` / `s3d:clear` | 同 `s2d:` | 同 `s2d:` |
| `s3d:camera` | `(ex, ey, ez, tx, ty, tz: float, fov: float) -> bool` | 相机：眼点、目标点、垂直视场角（度） |
| `s3d:light` | `(dx, dy, dz: float) -> bool` | 平行光方向 |
| `s3d:depth` | `(on: bool) -> bool` | 开关 z 缓冲 |
| `s3d:tri` | `(x0,y0,z0, x1,y1,z1, x2,y2,z2, r,g,b: float) -> bool` | 三角形 |
| `s3d:quad` | `(x0,y0,z0, x1,y1,z1, x2,y2,z2, x3,y3,z3, r,g,b: float) -> bool` | 四边形 |
| `s3d:box` | `(cx, cy, cz, sx, sy, sz, r,g,b: float) -> bool` | 轴对齐长方体 |
| `s3d:grid` | `(half, step, r, g, b: float) -> bool` | 地面网格 |
| `s3d:push` | `() -> bool` | 保存当前模型变换 |
| `s3d:pop` | `() -> bool` | 恢复 `s3d:push` 保存的变换 |
| `s3d:translate` | `(tx, ty, tz: float) -> bool` | 复合一个平移 |
| `s3d:rotate` | `(rx, ry, rz: float) -> bool` | 复合一个旋转（度） |
| `s3d:scale` | `(sx, sy, sz: float) -> bool` | 复合一个缩放 |
| `s3d:sphere` | `(cx, cy, cz, rad: float, segs: int, r, g, b: float) -> bool` | 平面着色的球 |
| `s3d:cylinder` | `(cx, cy, cz, rad, h: float, segs: int, r, g, b: float) -> bool` | 平面着色的圆柱（轴沿 Y） |
| `s3d:mouse_*` / `key` / `time` / `dt` | 同 `s2d:` | 同一个窗口的输入 |

3D 图元走透视投影与近平面裁剪；开了 `s3d:depth` 时按深度缓冲消隐，
平行光会给各面不同的明暗。

### 3.1 模型变换栈

一个场景用局部坐标系搭起来比直接用世界坐标读起来顺：原点上的一个盒子，
转一下再挪一下，是三次调用而不是一堆坐标。变换是一个 4×4 仿射矩阵，
把**局部坐标**映射到**世界坐标**；相机再把世界坐标映射到屏幕。所以顺序永远是

```
局部 → 模型 → 世界 → 相机 → 屏幕
```

- `translate` / `rotate` / `scale` 是**复合**：`模型 = 模型 × 操作`。
  所以**后写的变换先作用到顶点上**（它更靠内层）。
- `rotate` 按 **X、Y、Z** 的顺序转，单位是度：绕 X 的旋转最先作用到顶点。
- `push` 保存当前矩阵，`pop` 把它取回来。空栈上的 `pop` 返回 **false**——
  程序要的那一帧不在那里，这是错误，不是静默的 no-op。栈最多 64 层。
- 矩阵是**绘图状态**：它跨帧保留，`clear` 不重置它（`clear` 清的是像素）。
- `segs` 是**计数**，所以是 `int`：它没有小数部分（§4 的同一条理由）。
  它被钳制到 `[3, 64]`。
- 球和柱是**平面着色**：每个三角形各自算明暗，所以球看起来是一圈一圈的
  硬边，和盒子的观感一致。球的极点行会退化成一点，所以球不需要单独的
  极点处理。

## 4. spio（`spio:`）

**原生文件 I/O：由编译出来的程序自己读写文件系统**，不经运行器、不经守护进程。

| 函数 | 签名 | 说明 |
|---|---|---|
| `spio:print` | `(text: string) -> bool` | 往标准输出打印 |
| `spio:print_num` | `(v: float) -> bool` | 打印一个数 |
| `spio:read_text` | `(path: string) -> string` | 读整个文件 |
| `spio:write_text` | `(path, text: string) -> bool` | 写整个文件（父目录不存在会自动创建） |
| `spio:append_text` | `(path, text: string) -> bool` | 追加文本 |
| `spio:read_line` | `(path: string, idx: int) -> string` | 读第 `idx` 行（从 0 起） |
| `spio:line_count` | `(path: string) -> int` | 行数 |
| `spio:replace_text` | `(path, old, new: string) -> bool` | 就地替换文本 |
| `spio:read_f64` | `(path: string) -> float` | 读一个小端 f64 |
| `spio:write_f64` | `(path: string, v: float) -> bool` | 写一个小端 f64 |
| `spio:append_f64` | `(path: string, v: float) -> bool` | 追加一个小端 f64 |
| `spio:exists` | `(path: string) -> bool` | 文件是否存在 |
| `spio:size` | `(path: string) -> float` | 文件字节数 |
| `spio:remove` | `(path: string) -> bool` | 删除文件 |
| `spio:mkdir` | `(path: string) -> bool` | 建目录 |
| `spio:copy` / `spio:rename` | `(from, to: string) -> bool` | 复制 / 改名 |
| `spio:list_count` | `(dir: string) -> int` | 目录下的条目数 |
| `spio:list_name` | `(dir: string, idx: int) -> string` | 目录下的第 `idx` 个条目名 |

计数与索引是 `int`，不是 `float`：它们没有小数部分，做成 `float` 会强迫调用方
在每次比较之前做一次转换。

`spio:` 与 spasio 通道互不干扰：通道仍然走运行器（§9），
`spio:` 走的是程序自己的文件系统调用。

## 5. 自动化钩子

playground 运行时读两个环境变量，用于无人值守的运行与截图测试：

| 变量 | 作用 |
|---|---|
| `SPASS_PLAYGROUND_FRAMES=N` | 渲染 N 帧后自动关闭窗口并退出（0 = 不限） |
| `SPASS_PLAYGROUND_PPM=path` | 把最后一帧导出为二进制 PPM（P6） |

导出的是**帧缓冲的原始像素**，不是屏幕截图：没有窗口管理器参与，
因此在 CI 里结果与在桌面上完全一致。

## 6. 效率要求

playground 是软件光栅化，没有 GPU 加速，所以效率是硬性要求而不是锦上添花：

- 帧成本必须显著低于 16.6 ms（60 fps 的预算）。当前参考实现在
  320×240、地面网格 + 一个立方体 + 一个球 + 一个柱 + 若干 2D 图元的
  场景下是 **约 2.9–3.2 ms/帧（≈310–350 fps）**。
- 帧率上限：不限帧数时运行时每帧 sleep 16 ms，避免空转烧一个核。
- 运行时只在使用到相应命名空间时才被链接进产物（与 §11.5 的输出一致），
  所以不使用 playground 的项目不承担这 91 KB 的编译期与体积成本。

## 7. 参考实现与验证

- 实现：`compiler/crates/spass-playground/src/playground.rs`（Rust）、
  `playground.c`（C）。两者由 `spass-codegen` 按需嵌入生成的模块。
  两个后端的图层模型与 alpha 语义必须一致。
- 示例：`examples/playground/`，一个同时用 2D、3D 和 `spio:` 的小程序。
  它**先画 2D 再画 3D**：旧模型下这样会让 2D 消失，新模型下两帧一致。
- 验证：`compiler/scripts/verify-playground.ps1`。它以无头方式渲染一帧，
  逐像素断言背景色、矩形填充、矩形描边、圆盘、椭圆、三角形、文字底板、
  半透明混合，并断言**面板下面一个 3D 像素都没有**（2D 覆盖层真的盖住了 3D），
  再断言 3D 地面网格、受光立方体、球、柱和文字字形都在帧里，
  最后测量每帧耗时。
  **「能编译」不是证据，帧缓冲里的像素才是。**
- **两个后端的一致性靠源码结构检查兜底**：C 编译器不是工具链的依赖，
  所以 C 后端在很多机器上根本编译不了，能自动抓到的错误类别因此有限。
  `spass-playground` 的测试检查三件事：语言表面
  （`spass-playground/src/surface.rs`）上的每个符号，两个后端都导出，
  且参数个数相同；C 后端调用任何一个助手函数时，那个函数必须已经定义
  （C 没有前向声明）；两个后端的括号都配平。
  `spass-semantic` 的测试再查一遍：注册的 builtin 与运行时导出的符号
  一一对应，参数个数相同——注册少了，调用就通不过类型检查；个数对不上，
  生成的模块就链接不上，而 `cargo test` 里没有任何东西会编译它。


## 8. 已知的后端差异

| | Rust 后端 | C 后端 |
|---|---|---|
| `spio:list_count` | 返回真实条目数 | 返回 **-1**（目录列举未实现） |
| `spio:list_name` | 返回真实条目名 | 返回空串（目录列举未实现） |

C 后端返回 -1 而不是 0，是为了让它说「不支持」而不是和 Rust 后端悄悄不一致。

除这张表之外，两个后端的图层模型、alpha 语义、近平面裁剪、模型变换栈
和图元集合都是一致的；一致性由 §7 的源码结构检查守住。

## 9. 字符串的生命周期

`spio:` 读回来的字符串在生成代码里的 ABI 是 `&'static str`，所以它们活到进程结束。
运行时对读到的内容做了**驻留（intern）**：同一段文本只存一份，
所以在循环里反复读同一个文件不会每次都涨堆。

`ponytail`：内容不同的字符串仍然不会被释放——一个每帧读一个不同文件的程序
依然会无界增长。真正的修法是给字符串一个真正的生命周期（ABI 里用拥有所有权的
`String`，或者加一个显式的释放函数）。
