// 个人工作台 · 自动更新
//
// ============================================================================
// 为什么不用官方文档里那段前端写法
// ============================================================================
//
// Tauri 官方 README 的例子是「前端 import { check } from '@tauri-apps/plugin-updater'」。
// 在这个项目里**用不了**，两条原因叠加：
//
//   1. tauri.conf.json 里 withGlobalTauri = false，
//      而且窗口加载的是 http://127.0.0.1:8787，不是 tauri:// 协议
//      —— 前端连 window.__TAURI__ 都拿不到，压根没有 IPC 通道。
//   2. 界面是 Node 版和桌面版**共用**的 public/index.html。
//      前端要能调 updater，就得同时处理 Node 版那边「压根没有 Tauri」的分支，
//      等于把一个纯桌面功能渗进共用界面。
//
// 所以改成：**Rust 侧用 AppHandle 调，再包成两个本地 HTTP 接口**
//（/api/update/check 和 /api/update/install），界面照旧 fetch('/api/xxx')。
// 这样界面那一侧完全不知道自己背后是 Tauri —— 和别的接口没区别。
//
// ============================================================================
// 接口约定（和 Node 版逐条对齐）
// ============================================================================
//
//   GET  /api/update/check    → 查有没有新版本
//   POST /api/update/install  → 下载并安装，然后重启
//
// 调用类的一律 200 + ok:false + kind（理由见 方案.md 坑 #59）：
// 前端 api() 遇到 4xx 会抛异常，用户只能看到「接口出错」，看不到人话。
//
// kind 的取值：
//   not_available  现在就是最新的
//   network        连不上 GitHub（断网 / 被墙 / 公司网关拦了）
//   manifest       清单取回来了但不对（多半是 latest.json 没发布，或格式写错了）
//   signature      **签名对不上 —— 这条最要紧，说明有人在伪造更新包**，
//                  界面必须如实说「没有安装」，绝不能糊过去
//   unsupported    Node 版（它没有 Tauri，本来就不支持）
//   install_failed 下载或安装失败
//
// ============================================================================
// 同步 / 异步的接缝
// ============================================================================
//
// 插件的 check() 和 download_and_install() 都是 async，
// 而我们的 HTTP 服务（tiny_http）和数据层（rusqlite）都是同步的。
//
// 这里用 tauri::async_runtime::block_on 把 async 拉回同步，
// **不引入 tokio**。安全性前提：调用方是 HTTP 服务自己的线程，
// 不是 Tauri 主线程 —— 在主线程上 block_on 会直接 panic。
// （真要在主线程上调，block_on 那句会 panic 而不是死锁，
//   症状是「一点检查更新程序就崩」，很好认。）

use serde_json::{json, Value};
use tauri::AppHandle;
use tauri_plugin_updater::{Error as UpdaterError, UpdaterExt};
use std::time::Duration;

/// 当前版本号。取自编译期环境变量 CARGO_PKG_VERSION —— 编译一次写死一次，
/// 不去读 tauri.conf.json，避免两处版本号打架。
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 失败的大类。界面靠这个决定说哪句话。
///
/// 单独抽成一个类型，而不是直接返回字符串：
/// 「什么错」和「跟用户怎么说」是两件事，拆开之后后半截能直接写单元测试
/// （前一截要构造插件的私有错误类型，测试里造不出来）。
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum FailKind {
    /// 连不上更新服务器
    Network,
    /// 清单不对（没发布过 / 格式错 / 没有你平台的包）
    Manifest,
    /// **签名对不上 —— 有人在伪造更新包，绝不能装**
    Signature,
    /// 下载或安装过程本身失败
    Install,
}

/// 这个大类对应给用户看的那句话。
///
/// 写成人话而不是插件的英文原文，是这个函数存在的全部理由：
/// 用户看到 "Could not fetch a valid release JSON from the remote"
/// 既不知道是自己断网了还是对方挂了，更不知道下一步该干什么。
pub fn describe(kind: FailKind) -> &'static str {
    match kind {
        FailKind::Network => "连不上 GitHub，检查一下网络再试",
        FailKind::Manifest => "取不到更新清单，多半是这个地址上还没有发布过带清单的版本",
        FailKind::Signature => "更新包签名不对，为了安全没有安装",
        FailKind::Install => "下载或安装失败",
    }
}

/// 把插件的错误归到上面四个大类之一。
///
/// 变体名是照着 tauri-plugin-updater 2.13 的 error.rs 抄的
/// —— 一开始我按印象写，编译报了一串「找不到变体」，才发现整个枚举跟我记的不一样。
fn classify(e: &UpdaterError) -> FailKind {
    match e {
        // 签名相关的三个都归到 Signature：验签没过的，一律不能装。
        UpdaterError::Minisign(_) | UpdaterError::Base64(_) | UpdaterError::SignatureUtf8(_) => {
            FailKind::Signature
        }
        UpdaterError::ReleaseNotFound
        | UpdaterError::Serialization(_)
        | UpdaterError::TargetNotFound(_)
        | UpdaterError::TargetsNotFound(_)
        | UpdaterError::EmptyEndpoints
        | UpdaterError::UnsupportedArch
        | UpdaterError::UnsupportedOs
        | UpdaterError::UrlParse(_) => FailKind::Manifest,
        UpdaterError::Reqwest(_) | UpdaterError::Network(_) | UpdaterError::Io(_) => {
            FailKind::Network
        }
        _ => FailKind::Install,
    }
}

/// 组装失败响应。所有失败一律 200（理由见文件顶部）。
fn fail(kind: FailKind, detail: Option<String>) -> Value {
    let mut message = describe(kind).to_string();
    // Install 这一类的原文通常有信息量（权限不足、磁盘满之类），带上去；
    // 其它三类原文是英文技术词，对用户没用，就不拼进去了。
    if let Some(d) = detail {
        if kind == FailKind::Install {
            message = format!("{}：{}", message, d);
        }
    }
    json!({
        "ok": false,
        "kind": match kind {
            FailKind::Network => "network",
            FailKind::Manifest => "manifest",
            FailKind::Signature => "signature",
            FailKind::Install => "install_failed",
        },
        "message": message,
        "current": current_version(),
    })
}

/// GET /api/update/check
pub fn check(app: &AppHandle) -> Value {
    let current = current_version();

    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => return fail(classify(&e), Some(e.to_string())),
    };

    match tauri::async_runtime::block_on(updater.check()) {
        Ok(None) => json!({
            "ok": true,
            "available": false,
            "current": current,
        }),
        Ok(Some(update)) => {
            // 把远端清单里那堆字段挑出界面真用得上的几个，
            // 剩下的（各平台完整清单、下载地址、签名）不给前端 ——
            // 那是给插件下载时自己用的。
            //
            // 两个字段的类型是照插件源码 2.13 的 `Update` 结构体写的
            // （body 是 Option<String>，不是带 notes 字段的结构体 ——
            //  第一次写错，编译直接指出来了）。
            let notes = update.body.clone().unwrap_or_default();
            // 日期只取到「天」就够了：OffsetDateTime 的 format() 要开
            // time 那个 crate 的 formatting feature，为了显示一个日期
            // 去开 feature 不值当；date().to_string() 本来就是 2026-10-15。
            let date = update.date.map(|d| d.date().to_string());
            json!({
                "ok": true,
                "available": true,
                "current": current,
                "version": update.version.clone(),
                "date": date,
                "notes": notes,
            })
        }
        Err(e) => return fail(classify(&e), Some(e.to_string())),
    }
}

/// POST /api/update/install
///
/// 做法是**重新查一次再装**，而不是把 check 那次的 Update 对象留着等 install。
/// 原因：Update 内部攥着下载状态，跨 HTTP 请求存着既别扭又容易过期；
/// 而重新查一次最多多一次几十 KB 的请求，换来的是两个接口完全无状态。
pub fn install(app: &AppHandle) -> Value {
    let updater = match app.updater_builder().build() {
        Ok(u) => u,
        Err(e) => return fail(classify(&e), Some(e.to_string())),
    };

    let update = match tauri::async_runtime::block_on(updater.check()) {
        Ok(Some(u)) => u,
        // 「已经是最新的」不是故障，但它也不是一次成功的安装 ——
        // 界面得说清楚，不然用户点了没反应会以为坏了。
        Ok(None) => {
            return json!({
                "ok": false, "kind": "not_available",
                "message": "当前已经是最新版本，没什么可装的",
                "current": current_version(),
            })
        }
        Err(e) => return fail(classify(&e), Some(e.to_string())),
    };

    let version = update.version.clone();

    // 下载 + 安装。安装包约 2MB，几秒钟就完，所以同步等它跑完。
    // 真到几十 MB，这里就该改成「立即返回 + 界面轮询进度」了。
    let result = tauri::async_runtime::block_on(async {
        update.download_and_install(|_chunk, _total| {}, || {}).await
    });

    match result {
        Ok(()) => {
            // 装完之后要重启，但**不能现在就重启**：
            // restart() 不返回，进程会被立刻换掉，
            // 那样这个 HTTP 响应就永远发不出去，界面只能看到「连接被重置」。
            // 所以先把响应发出去（返回给调用方），再另起一个线程延迟重启。
            //
            // 延迟 800ms 是给「响应写出 + 界面渲染提示」留的时间。
            // 这 800ms 是拍出来的经验值，不精确；
            // 真出问题的话症状是「装完了界面偶尔还停在安装中」，不会更糟。
            let handle = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(800));
                handle.restart();
            });
            json!({ "ok": true, "version": version, "current": current_version() })
        }
        Err(e) => fail(classify(&e), Some(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 版本号跟得上编译期那个() {
        // 编译进 exe 的版本必须和 tauri.conf.json 里写的一致，
        // 不一致的话界面会显示一个跟更新清单对不上的版本号。
        assert_eq!(current_version(), "0.6.1");
    }

    #[test]
    fn 签名不对时必须说清楚没有安装() {
        // 安全底线：验签失败意味着有人在伪造更新包。
        // 提示里如果只说「出错了」，用户可能重试很多次；
        // 更糟的是如果分类写错、界面把它当成「网络不好」，
        // 用户会一直重试一个永远不可能成功的事。
        let msg = describe(FailKind::Signature);
        assert!(msg.contains("签名"), "要说明是签名问题：{}", msg);
        assert!(msg.contains("没有安装"), "要说明没有安装：{}", msg);
    }

    #[test]
    fn 四类失败都有人话() {
        // 每一种都不能返回空话或英文原文 ——
        // 界面上就是这几句话，用户看到的就是这几句话。
        for k in [FailKind::Network, FailKind::Manifest, FailKind::Signature, FailKind::Install] {
            let m = describe(k);
            assert!(!m.is_empty());
            assert!(m.chars().any(|c| c as u32 > 127), "得是中文：{}", m);
            assert!(!m.contains("Error"), "不该混进英文错误名：{}", m);
        }
    }

    #[test]
    fn 四类失败对应四个前端kind值() {
        // 前端 updateFail() 按 kind 分支写文案，这里对不上就会掉进兜底分支，
        // 签名那条会显示成普通的红色错误 —— 安全提示就丢了。
        let want = [
            (FailKind::Network, "network"),
            (FailKind::Manifest, "manifest"),
            (FailKind::Signature, "signature"),
            (FailKind::Install, "install_failed"),
        ];
        for (kind, expect) in want {
            let v = fail(kind, None);
            assert_eq!(v["ok"], json!(false));
            assert_eq!(v["kind"], json!(expect));
            assert_eq!(v["current"], json!(current_version()));
        }
    }
}