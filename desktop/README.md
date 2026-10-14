# 个人工作台 · 桌面版（Tauri 2）

把现有的「Node 服务 + 浏览器」形态，换成一个**双击图标就能开、不弹黑色命令行窗口**的桌面软件。

**当前状态：阶段 0（路线验证）。代码已写完，尚未编译过** —— 缺 Rust 工具链，见下面「先决条件」。

---

## 这个版本的目��

**只验证一件事：Tauri 这条路走得通吗？**

具体是验证三个假设：

1. 现有的 `public/index.html`（176KB）能不能**一行不改**就在 Tauri 窗口里跑起来
2. Rust 版的 SQLite 数据层能不能替代 `server.js`
3. 真实安装包到底多大、启动多快

功能上**只做了笔记**（新建 / 编辑 / 自动保存 / 搜索 / 软删除 / 导出），
任务和收藏的接口先返回空数组 —— 让界面能完整显示、不报红横幅而已，
它们的逻辑要等这一关过了再平移。

> 为什么要分阶段：`server.js` 有 71KB，一次性全量平移的话，
> 出问题时你分不清是「Tauri 本身不行」还是「某一块 Rust 写错了」。
> 拆开之后每一步都能定位。

---

## 先决条件（Windows）

装这两个，**缺一个都编译不了**：

```powershell
# 1. Visual Studio C++ 生成工具（Tauri 在 Windows 上编译 Rust 的硬前提，约 5-8GB）
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# 2. Rust 工具链
winget install Rustlang.Rustup
```

⚠️ **装完 Rust 必须重开一个终端**，否则 `cargo` 不在 PATH 里。

**不需要**装 WebView2 —— Windows 10/11 已经预装了（检测到本机是 155.0.4283），
这正是 Tauri 比 Electron 小 20 倍的原因：它不捆绑浏览器，直接用系统的。

验证：

```powershell
cargo --version
rustc --version
```

---

## 怎么构建

```powershell
cd E:\个人工作台\desktop

# 开发模式：编译并弹出窗口。改了前端刷新就生效，不用重编 Rust
# （debug 模式会带一个黑色控制台窗口，那是给你看日志用的）
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH
cd src-tauri
cargo run

# 出安装包：需要 tauri-cli（装一次，大概几分钟）
cargo install tauri-cli --version "^2"
cd ..
cargo tauri build
```

> **调试时**如果本机还开着 `server.js`（占着 8787），桌面版会明确报错提示你关掉它。
> 想两个一起跑，设个环境变量换个端口：
> ```powershell
> $env:WORKBENCH_PORT = "8788"
> cargo run
> ```
> （换端口后 Markdown 视图模式那个 localStorage 设置读不到旧值，属正常）

产物位置：

- `src-tauri/target/release/bundle/nsis/个人工作台.exe`（安装程序）
- `src-tauri/target/release/bundle/msi/`（MSI 包）

---

## 数据在哪（重要，跟原来不一样）

| | 原来（server.js） | 桌面版 |
|---|---|---|
| 数据库 | `E:\个人工作台\data\workbench.db` | `C:\Users\<你>\AppData\Roaming\com.workbench.desktop\workbench.db` |

必须换位置：桌面应用会装进 `C:\Program Files\` 这类**没有写权限**的目录，
往那儿建 `data/` 会直接失败。

**所以你现有的笔记在桌面版里是看不到的**，两库独立。
等桌面版功能补齐之后，会加一个「导入现有数据库」的入口 —— 现在还不需要处理。

---

## tauri.conf.json 的几处「为什么不那么写」

> ⚠️ **这个文件里放不了注释**。Tauri 用严格 JSON 解析它，
> 写 `//` 注释会直接报 `key must be a string`（而且行号指向注释处，很难一眼看懂）。
> 所以下面这些说明都记在这里，不写进配置文件。

| 配置 | 值 | 为什么 |
|---|---|---|
| `build.frontendDist` | `../../public` | 直接指现有的 `public/`，不用复制一份。相对 `src-tauri/` 往上两级正好是它 |
| `app.security.csp` | `null` | 不是疏忽。窗口加载的是 `http://127.0.0.1`，页面里有大量内联 `<style>`/`<script>`（零构建的单文件前端），收紧 CSP 会全部拦掉、白屏。真实攻击面（用户粘进来的链接）在 Rust 侧的 `is_private_host` / `norm_url` 上拦，不依赖 CSP |
| `windows[0]` 不配 `url` | —— | 配了窗口会先加载那个地址、然后 Rust 再 `navigate` 过去，白白多一次失败请求，还可能看到一闪而过的白屏。真实地址在 `lib.rs` 里运行时决定 |
| `bundle.windows.nsis.installMode` | `currentUser` | 默认装到 Program Files 要管理员权限。个人工具用当前用户安装，省得每次弹 UAC |
| `app.withGlobalTauri` | `false` | 不把 Tauri API 挂到 `window.__TAURI__`。这个应用的前端完全不需要它，暴露出来只会多一条攻击面 |

## capabilities/default.json 为什么只有一条权限

Tauri 2 新增的「权限声明」明确规定前端 JS 能调用哪些系统能力。
这里只给了 `core:default`（最基础的一组），因为界面完全不需要碰文件系统、
剪贴板、注册表——所有事情都由 Rust 后端做，前端只发 HTTP 请求。

保持权限最小化有实际意义：将来万一出现 XSS，攻击者能调用的系统能力也是有限的。
这跟「别让一条异常路径捅到整个系统」是同一个思路。

---

## 实测数据（阶段 0，2026-10-14）

在这台 Windows 11 上量的：

| 项 | 实测 | Electron 参考值 |
|---|---|---|
| **NSIS 安装包** | **1.57 MB** | 120–200 MB |
| 单个 exe | 3.73 MB | （同一个） |
| 启动到接口就绪 | 788 ms | 2–5 s |
| 空闲内存 | 23.4 MB | 150–400 MB |

安装包比 exe 还小，是因为 NSIS 会再压一次（LZMA）。
界面文件（index.html 176KB）已经打包进去了，**装完直接能开，不需要额外拷任何东西**。

对照组：原来的 Node 版代码本身只有 0.24 MB，但**要求你先装 Node 24**（93 MB）。
所以「零依赖」的真正成本不是仓库大小，是**部署门槛**——现在换成了
「下载 1.6MB 的安装包，双击」，方向是对的。

> 构建缓存（`target/`）约 6.7 GB，那是编译中间产物，**不进安装包**。
> 但它会占磁盘，删掉不会影响已生成的安装包，只是下次编译要重来一遍。

`Cargo.toml` 里开了体积优化（`opt-level="z"` + `lto` + `strip`）才到这个数。
去掉那些开关会明显变大。

> ⚠️ **MSI 没打出来**。`tauri.conf.json` 里 `targets` 写了 `["nsis","msi"]`，
> NSIS 成功、MSI 在 WiX 的 `light.exe` 那步失败了。
> NSIS 是 Windows 主流格式而且能用「当前用户」模式装（不用管理员权限），
> 所以不影响使用。暂时只出 NSIS，`targets` 里那个 `"msi"` 待查清原因后再决定去留。

---

## 实测踩到的坑（阶段 0）

这六条全是真踩的，不是预防性提醒。

### 1. `tauri.conf.json` 里放不了注释

写 `//` 注释会报 `key must be a string`，而且**行号指向注释那一行**，看错误信息完全想不到是注释的问题（JSON 里 `//` 在字符串内部是合法的）。

规矩：配置文件保持纯 JSON，所有「为什么这么写」记在这份 README 里。

### 2. 路由切路径段时，`api` 那一段要先去掉

```rust
// ❌ "/api/notes" → trim_matches('/') → "api/notes" → split → ["api","notes"]
//    seg[0] 是 "api"，拿它比 "notes" 永远比不上
let seg: Vec<&str> = path.trim_matches('/').split('/').collect();

// ✅
let seg: Vec<&str> = path.trim_start_matches("/api/").trim_matches('/').split('/').collect();
```

这个 bug 烦人的地方：服务是活的、端口是通的、JSON 正常返回，
只是内容永远是 `no such endpoint`。看起来「像那么回事」，不是崩了。

**是逐条打接口打出来的，不是读代码读出来的。**

### 3. rusqlite 要求「参数个数」和「占位符个数」完全一致

```rust
// ❌ 无搜索那条 SQL 里一个占位符都没有，却照样传了一个参数
// → error: Wrong number of parameters passed to query. Got 1, needed 0
query_map([&pattern], ...)

// ✅ 无占位符就传空数组
query_map([], ...)
```

别图省事合并两条 SQL（`(?1 IS NULL OR title LIKE ?1 ...)`），
分开写更好读。

### 4. 判断「这条还在不在」要看受影响行数，不能「更新完再读回来」

```rust
// ❌ SQL 里有 AND deleted_at IS NULL，已删除的记录其实改了 0 行（拒绝得完全正确）
//    但 get_note 不过滤 deleted_at，会把那条记录原样返回 —— 看起来像更新成功了
conn.execute(sql, params![..])?;
get_note(conn, id)

// ✅ execute 返回的是实际改动的行数
let changed = conn.execute(sql, params![..])?;
if changed == 0 { return Ok(None); }   // 前端转成 404
```

SQL 写得对、返回的 JSON 也有模有样，只有真去删一条再改它才会暴露。

### 5. Windows 上两个进程可以同时 bind 同一个端口

这是实测撞上的：用户的 `server.js`（node）和 `workbench` 同时占着 8787，
**两边都 bind 成功、都不报错**，但谁来处理请求变得不确定。

所以不能依赖 bind 报错。正确做法是**先用 `TcpListener` 探一下**：

```rust
match TcpListener::bind(&addr) {
    Ok(probe) => drop(probe),          // 探完就还回去
    Err(e) => return Err(format!("端口 {} 被别的程序占着……", port)),
}
```

中间有一瞬间空档存在竞态，但这个进程本来就在启动阶段，概率小到可忽略。

### 6. 端口固定 8787，不能用随机端口

`localStorage` 按 origin（协议+主机+端口）隔离，而前端用它存 Markdown 视图模式：

```js
let mode = localStorage.getItem('wb-mode') || 'edit';
```

端口每次启动都变 = 每次都是新 origin = 这个偏好设置**每次启动都白存**。
而这个 bug 极难发现：界面完全正常，只有一个偏好悄悄失效。

（代价是 8787 被占时不能自动换端口，所以同时加了 `WORKBENCH_PORT` 环境变量，
需要并跑时手动指定。）

### 7. release 版不能回退到「构建机的源码路径」找资源

第 6 条那个「先资源目录、再源码树」的写法，在开发时很好用，但实测拷到临时目录
单独跑 exe 时发现：**服务起来了、接口也通，看起来完全正常**——
因为 `CARGO_MANIFEST_DIR` 是**编译期常量**，构建机的路径已经写死在 exe 里了，
而这台机器上那个路径确实还在。

拷到别人电脑上，那条路径必然不存在，界面直接白屏；程序不崩、没有日志，
极难判断问题在哪。

改成按构建类型区分：

```rust
if cfg!(debug_assertions) {
    // 只有开发构建才回退到源码树（改了前端刷新就生效）
    return Ok(dev_path);
}
Err("找不到界面文件 public/index.html。重新安装一次应该就好……")
```

**发布版找不到就明确报错**，不要悄悄回退到一个在目标机器上必然失效的路径。
「把『明确找不到』变成『莫名其妙白屏』」是最坏的一种降级。

### 8. 「双击闪一下就没了」——发布版没有控制台，等于错误信息全丢

用户装完双击，窗口开一下就关，什么提示都没有。

**根因是一条链**：

1. `tauri.conf.json` 的 `frontendDist` 指向 `../../public`
2. 但**编译时 Tauri 并不会把 public/ 复制到 target/ 目录里**
   —— `frontendDist` 只对 `cargo tauri build` 打的**安装包**有意义
3. 所以直接跑 `target/release/workbench.exe` 时，
   `resource_dir()` 指向 `target/release`，那儿根本没有 `public/index.html`
4. → 报错退出 → 而 release 故意设了 `windows_subsystem = "windows"`（没有控制台）
   → **错误信息唯一的去处就是空气** → 用户只看到闪一下

**两个错误叠在一起**，而且单独任何一个都不致命：
「找不到资源」本来是能弹窗告诉用户的，
「没有控制台」本来是为了体验好。
凑一起就成了「出错了却不让用户知道怎么修」——这比出错更糟。

**修法一：出错必须留下痕迹。**
所有启动失败都写 `app_data_dir\启动错误.log`，并且**当场打开**它。
用系统自带记事本而不是弹窗，是为了不加 `winapi` 或 `tauri-plugin-dialog` 依赖。

⚠️ 写日志**必须带 UTF-8 BOM**：不带的话记事本按系统 ANSI（中文 Windows 是 GBK）读，
满屏「涓汉宸ヤ綔鍙」。这跟坑「启动.bat 必须 UTF-8 BOM」是同一个坑的另一个方向——
那边是 cmd 不看 `chcp`，这边是记事本不看编码标记。

**修法二：根本不要依赖外部文件。**
把界面用 `include_bytes!` 编译进 exe，逻辑是「磁盘上有就用磁盘的，没有就用内嵌的」：

- 开发时：磁盘上有 `public/`，改完 index.html 刷新窗口就生效，不用重编译
- 发布时：磁盘上没有（或被杀毒软件删了），用内嵌的那份，**一定能开**

代价是改 index.html 会让 cargo 重编本 crate（debug 约 8 秒）。可以接受。

### 9. `TcpListener::bind` 成功 **不等于** 端口是空的

第一个版本判断「端口被占用」用的是：试着 `bind` 一下，失败就是被占。

**实测证明这个判断是错的**：8787 已经被 node（server.js）占着时，
`TcpListener::bind("127.0.0.1:8787")` **依然返回成功**。
Windows 的 socket 默认可复用地址，两个进程可以同时 bind 同一端口且都不报错，
谁处理请求变得不确定（谁先 accept 谁接）。

**正确做法是「连一下试试」**：能不能连上，才是真的有人在用那个端口。
这个判断不受 `SO_REUSEADDR` 影响 —— 端口上有活着的服务就一定连得上。

> 通用教训：**「我抢占成功」不能证明「那里没人」**。
> 想判断资源是否被占用，要用「能不能碰到对方」，不是「我能不能占下」。

### 10. 诊断开关写反了，差点得出错误结论

为了能在有控制台的 debug 环境里复现发布版行为，加了个 `WORKBENCH_STRICT_RESOURCES=1`
的开关，本意是「设了就别回退到源码树」。

结果判断写反了 —— 变量叫 `strict`，条件却是「允许回退」。
于是设了 `=1` 反而回退，诊断显示「一切正常」，
我差点据此在提交信息里写下「资源查找没问题」。

**判断反了的标志：一开诊断开关，故障就消失了。**
诊断开关本身出 bug 时，它的表现和「问题已修复」一模一样。
以后写这类开关，先确认「开关**关掉**时行为和原来完全一致」。

### 11. `setup` 返回 `Err`，Tauri 内部还会再 panic 一次

上面那条修完优雅退出后，**退出码还是 -1073740791**（panic）。

原来以为是这样：setup 返回 Err → `run()` 返回 Err → 外层 `if let Err` 接住 → 优雅退出。
结果外层那个分支**压根没机会执行** —— Tauri 收到 setup 的 Err 之后内部自己 panic 了。

改法很直接：**报告完直接在 setup 里 `std::process::exit(1)`，不把错误返回给 Tauri。**

代价是那种「Tauri 自己内部的启动失败」不再被上报（比如窗口创建失败），
但那种情况本来也会 panic，用户看到「已停止工作」，区别不大。

**顺序很重要：先把错误说明摆到用户眼前（写日志 + 打开），再退出。**
Windows 那个「XX 已停止工作」的红框是**模态对话框**，
会盖住刚打开的记事本——用户先看到红框，合理地推断「这软件崩了」，
根本不会去看旁边那篇写清楚了该怎么办的日志。

**用不上的排查手段就别留在代码里**：先前那版把「优雅退出」写在 `run()` 外面，
看着更「规整」，实际上永远执行不到。`#[warn(unused variable)]` 已经提示过。

---

## 代码结构

```
desktop/
├── tools/make-icons.js      生成图标的脚本（配色抄的 index.html 的 --accent）
└── src-tauri/
    ├── Cargo.toml           依赖清单 + 体积优化开关
    ├── tauri.conf.json      窗口、图标、打包配置
    ├── capabilities/        Tauri 2 的权限声明（这里只给了最小集）
    ├── icons/               生成出来的图标
    └── src/
        ├── main.rs          入口：找数据目录 → 起服务 → 窗口指向它
        ├── db.rs            数据层：rusqlite，照搬 server.js 的表结构
        └── http_server.rs   本地 HTTP 服务，让前端零改动
```

**最重要的一件事**在 `http_server.rs` 顶部的注释里：
窗口加载的是 `http://127.0.0.1:8787` 而不是文件，
所以前端那几百处 `fetch('/api/xxx')` 的相对路径原样就能用，
`index.html` 一个字都不用改。

---

## 已知问题 / 下一步

- [ ] 任务、收藏的接口还没平移（阶段 1）
- [ ] AI 相关接口没平移（阶段 1）
- [ ] 现有数据没有导入通道
- [ ] 收藏视图的「打开链接 ↗」用 `target="_blank"`，
      在 Tauri 里点可能会在应用窗口内打开而不是跳系统浏览器 —— 要拦截导航
- [ ] 没有自动更新、没有首次启动引导
- [ ] macOS / Linux 构建需要对应平台的机器，代码本身不用改
