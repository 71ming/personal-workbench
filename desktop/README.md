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
