# 个人工作台

一个装在 Windows 上的本地工作台：**笔记、任务、收藏、导入备份、问 AI**，一个窗口做完。

数据只存在你自己这台电脑上 —— **没有账号、没有云端、不同步、不上传**。
只有你主动点「问 AI」并勾选笔记时，那几条笔记才会发给 AI 服务商。

---

## 这个仓库里有两套程序

它们是**同一个软件的两个版本**，功能一样，区别只在运行方式：

| | 桌面版（**日常用这个**） | Node 版 |
|---|---|---|
| 目录 | `desktop/` | 仓库根目录 |
| 怎么跑 | 装完双击图标，不弹命令行窗口 | 要先装 Node 24，双击 `启动.bat` |
| 后端 | Rust（`desktop/src-tauri/src/*.rs`） | Node.js（`server.js`） |
| 数据 | `C:\Users\<你>\AppData\Roaming\com.workbench.desktop\workbench.db` | `data/workbench.db`（在仓库里，已被 git 忽略） |
| 安装包 | 约 2 MB | —— |

**两套共用同一个界面文件** `public/index.html`，所以功能表现完全一致；
后端接口也要求逐条对齐（同一份测试脚本会分别打两个后端，见 `desktop/tools/`）。

> 想清楚为什么会有两个版本、以及它们怎么保持一致，见 [`方案.md`](方案.md)。
> Node 版是桌面版的前身 —— 先用它把功能和界面做扎实，再用 Tauri 套一层壳。

---

## 目录结构

```
.
├── server.js              Node 版后端（单文件，约 1600 行）
├── public/index.html      界面：两个版本共用这一个文件
├── 启动.bat                Node 版双击启动
├── data/workbench.db      Node 版数据库（不进版本库）
├── 方案.md                 项目思路、架构决定、65 条踩坑记录
│
└── desktop/               桌面版
    ├── README.md          桌面版的详细文档（构建、坑、实测数据）
    ├── release/           打好的安装包放在这里（不进版本库）
    ├── tools/             测试脚本：同一套打两个后端
    │   ├── test-tasks.js / test-bookmarks.js / test-fetch.js
    │   ├── test-import.js / test-ai.js
    │   ├── clean-test-data.js   清理测试残留
    │   └── make-icons.js
    └── src-tauri/         Tauri 2 + Rust
        ├── src/*.rs       ai / db / fetch / http_server / import / update / backup
        └── tauri.conf.json
```

---

## 构建桌面版

需要 [Rust](https://rustup.rs/) 和 [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio)（勾「使用 C++ 的桌面开发」），缺一个都编译不了。

```powershell
cd desktop
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH
cargo tauri build --bundles nsis
```

产物：`desktop/release/个人工作台_<版本号>_x64-setup.exe`

> 只出 NSIS（`msi` 在 WiX 的 `light.exe` 上失败）。NSIS 默认装到当前用户目录，不需要管理员权限。
> 第一次编译要十几分钟（要现编几百个 Rust 依赖）；`desktop/src-tauri/target/` 是编译缓存，
> 删了不影响已有的安装包，只是下次要重新编。

## 跑测试

两个后端都要跑一遍 —— 一致性是这个项目最不能破的东西。

```powershell
# Node 版（先双击 启动.bat，或 node server.js）
node desktop\tools\test-tasks.js     http://127.0.0.1:8787
node desktop\tools\test-bookmarks.js http://127.0.0.1:8787
node desktop\tools\test-fetch.js     http://127.0.0.1:8787
node desktop\tools\test-import.js    http://127.0.0.1:8787
node desktop\tools\test-ai.js        http://127.0.0.1:8787

# 测完清掉测试造出来的垃圾（只删带测试特征的，不碰你自己的数据）
node desktop\tools\clean-test-data.js http://127.0.0.1:8787
```

桌面版把地址换成 `http://127.0.0.1:8795`（用 `WORKBENCH_PORT=8795` 启动）。

---

## 数据在哪 / 怎么备份

两个版本的数据库都在各自的位置，**互不相通**。要合并用界面上的「导入备份」：
只加不删、按内容判重、重复导入不会产生副本。

数据文件不进版本库（`.gitignore` 里挡住了），AI 密钥存在数据库旁边的 `ai-config.json` 里，
**导出备份时不会包含密钥**。

---

## 当前进度

笔记、任务、收藏、网页抓取、导入备份、AI 问答、首次启动引导 —— 全部完成并实测通过。

没做的：移动端（iOS 必须 macOS + Xcode）、自动更新（仓库已建，签名密钥还没生成）。

细节和实测数据见 [`desktop/README.md`](desktop/README.md)。