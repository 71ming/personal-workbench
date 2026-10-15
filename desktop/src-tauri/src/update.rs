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
// updater 插件只在桌面端注册（官方支持矩阵里没有 Android），
// 手机上这两个导入和下面的 classify 都是死代码，不加 cfg 会报
// 「never used」—— 那是我自己引入的警告，不能留着。
#[cfg(desktop)]
use tauri_plugin_updater::{Error as UpdaterError, UpdaterExt};
use std::time::Duration;

/// 手机端的更新清单地址。
///
/// 为什么不用 GitHub API 现查现取：API 对未认证请求有每小时 60 次的限额，
/// 而一个静态文件是走 CDN 的、无限流、也不会因为 API 改版而突然不能用。
/// 代价是发版时要多传一个附件 —— 跟 latest.json 一样，
/// 由 tools/make-latest-json.js 一并生成。
///
/// 地址里的 `latest` 跟桌面版用的是同一个「最新发布」，
/// 所以只要我们不单独给 Android 建 Release（刻意的，见方案.md 十三章），
/// 两个平台的清单就永远指向同一个发布。
// 只在手机版编译；桌面版带着它会是一段没人用的死代码（编译器会警告）。
#[cfg(mobile)]
const ANDROID_MANIFEST_URL: &str = "https://github.com/71ming/personal-workbench/releases/latest/download/android-latest.json";

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
///
/// `#[cfg(desktop)]`：手机上根本不调 updater 插件（没注册），
/// 这个映射用不上，不加 cfg 会在编 Android 时报「never used」。
#[cfg(desktop)]
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

/// GET /api/update/check（桌面版）
///
/// 写成两个同名函数、各带各的 `#[cfg]`，而不是在一个函数里 `#[cfg]` 分支：
/// 分支版在手机上会留下一个没人用的 `app` 参数（编译器会警告），
/// 而且函数体缩进会乱掉，最后读者得同时在脑子里编译两个平台。
#[cfg(desktop)]
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
                // 告诉界面按钮该是「下载并安装」还是「下载新版本」。
                // 桌面只有前者，手机只有后者（见上面 android_check 的注释）。
                "mode": "install",
            })
        }
        Err(e) => return fail(classify(&e), Some(e.to_string())),
    }
}

/// GET /api/update/check（手机版）
///
/// 手机上查的是自己的小清单，不是 updater 插件 —— 原因见 android_check 顶部。
#[cfg(mobile)]
pub fn check(_app: &AppHandle) -> Value {
    android_check()
}

/// 手机上的「检查更新」。
///
/// 桌面那套（tauri-plugin-updater）在手机上**根本用不了**：
/// 插件只在 `#[cfg(desktop)]` 下注册（官方支持矩阵里没有 Android），
/// 就算注册了，它做的事是「下载 msi/exe 然后调安装器」，手机上也无从谈起。
///
/// 手机能做的只有一件事：**告诉用户有新版本，给一个下载链接**，
/// 由浏览器（opener 插件会调起系统 Intent）去下，用户自己点安装。
/// 好处是「自动」这件事只差最后一步 —— 而那一步本来也只有用户能按。
#[cfg(mobile)]
fn android_check() -> Value {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => return fail(FailKind::Network, Some(e.to_string())),
    };

    // ⚠️ 地址上必须挂一个随时间变的参数。
    //
    // 实测踩到的：把清单从 0.6.3 改成 0.6.4 再传上去，
    // 紧接着去拉，拿到的**还是 0.6.3** —— GitHub 的 CDN 把这个附件缓存住了。
    // 对「检查更新」来说这是要命的方向性错误：新版本发出来了，
    // 用户却被告知「已经是最新」，而且要等到缓存过期才可能发现不对。
    //
    // Tauri 桌面版没这个问题是因为它自己的 updater 插件会加
    // `?current_version=xxx`（源码里写死的），我们这边手写请求就得自己来。
    let url = format!(
        "{}?current_version={}",
        ANDROID_MANIFEST_URL,
        current_version()
    );
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(_) => return fail(FailKind::Network, None),
    };
    if !resp.status().is_success() {
        return fail(FailKind::Manifest, None);
    }

    match resp.text() {
        Ok(t) => android_check_from_text(&t),
        Err(_) => fail(FailKind::Network, None),
    }
}

/// 清单文本 → 响应。**纯函数，不碰网络。**
///
/// 抽出来只有一个理由：上面那个 android_check 里混着网络请求，
/// 只能在手机上验；而手机在墙内经常连不上 GitHub（见方案.md），
/// 也就是说你根本没法稳定地验证「有新版」那条分支。
/// 把解析单独拎出来之后，四条分支全都能在桌面上用单元测试钉死。
///
/// 这个文件顶部说的「逐条对齐」也靠它 ——
/// 每一种输入该回什么，都写成了断言，而不是「到手机上试一把」。
#[cfg(any(mobile, test))]
fn android_check_from_text(text: &str) -> Value {
    let current = current_version();

    // 解析不了、或者缺字段，一律当成「清单不对」——
    // 这跟桌面版拿不到合法清单时的归类保持一致，界面上说的话也就一致。
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return fail(FailKind::Manifest, Some(e.to_string())),
    };
    let remote_version = v.get("version").and_then(|x| x.as_str()).unwrap_or("");
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
    if remote_version.is_empty() || url.is_empty() {
        return fail(FailKind::Manifest, None);
    }

    // ⚠️ 这一步不能省：清单是从网上取来的，它说下载哪个地址我们就开哪个地址，
    // 那等于把「点一下更新」变成了「点一下就打开任意网址」。
    // 所以先验一遍，白名单只认 github.com 的 https 链接。
    if !is_trusted_download(url) {
        return fail(FailKind::Manifest, Some("下载地址不可信".to_string()));
    }

    if !newer_than(remote_version, current) {
        return json!({
            "ok": true,
            "available": false,
            "current": current,
        });
    }

    json!({
        "ok": true,
        "available": true,
        "current": current,
        "version": remote_version,
        "notes": v.get("notes").and_then(|x| x.as_str()).unwrap_or(""),
        "date": v.get("pub_date").and_then(|x| x.as_str()).and_then(|s| s.get(0..10)),
        "url": url,
        // 界面上按钮要变成「下载新版本」而不是「下载并安装」，
        // 点了以后走 openExternal（opener 插件在手机上也能用）。
        "mode": "download",
    })
}

/// 只认 github.com 的 https 下载地址。
///
/// 写成一个独立函数是为了能写单元测试 —— 安全判断不能靠「读一遍觉得没问题」。
///
/// `cfg(any(mobile, test))`：只有手机端才真的用到，但**测试里要用**，
/// 而测试只在桌面上跑。桌面正式构建时不带它，免得报「定义了没人用」。
#[cfg(any(mobile, test))]
fn is_trusted_download(url: &str) -> bool {
    // 用 tauri 重导出的 Url（和 open.rs 里验链接用的是同一个类型、同一个解析器），
    // 不额外给 Cargo.toml 加 url 这条依赖。
    match tauri::Url::parse(url) {
        Ok(u) => u.scheme() == "https" && u.host_str() == Some("github.com"),
        // 解析失败（比如少了 scheme）一律不信。这里用 Url::parse 而不是正则：
        // 正则认不全 `HTTPS://…`、`https://github.com.evil.com` 这类变形。
        Err(_) => false,
    }
}

/// `remote` 是否比 `local` 新。
///
/// **逐段按数字比，不能按字符串比。** 这是个真会踩的坑：
/// 字符串比较里 `"0.6.9" > "0.6.10"`（因为 '9' 比 '1' 大），
/// 于是发到第 10 个补丁版时，手机上的用户会收到「已经是最新的」，
/// 而且**测试还是绿的** —— 除非专门为它写一条。
#[cfg(any(mobile, test))]
fn newer_than(remote: &str, local: &str) -> bool {
    let nums = |s: &str| -> Vec<u64> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let (r, l) = (nums(remote), nums(local));
    // 段数不等时按「先到先比，缺的一段当 0」处理，
    // 这样 0.6 和 0.6.0、0.6.0.0 会被判成一样新。
    for i in 0..r.len().max(l.len()) {
        let a = r.get(i).copied().unwrap_or(0);
        let b = l.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

/// POST /api/update/install
///
/// 手机上**不装**：系统不允许一个应用悄悄把自己换掉，
/// 「安装未知来源的应用」这一步必须由用户手动点。
/// 这里如实告诉界面「该点哪个按钮」，而不是假装失败或者干脆不响应。
#[cfg(mobile)]
pub fn install(_app: &AppHandle) -> Value {
    json!({
        "ok": false,
        "kind": "not_available",
        "message": "手机上请点「下载新版本」，在浏览器里下完包再点开安装",
        "current": current_version(),
    })
}

/// POST /api/update/install
///
/// 做法是**重新查一次再装**，而不是把 check 那次的 Update 对象留着等 install。
/// 原因：Update 内部攥着下载状态，跨 HTTP 请求存着既别扭又容易过期；
/// 而重新查一次最多多一次几十 KB 的请求，换来的是两个接口完全无状态。
///
/// `#[cfg(desktop)]` 不能省：手机上有同名的一个（见上面 android 那段），
/// 两个都编的话会撞名 —— 而这个错误**只在编 Android 时才报**，
/// 桌面 `cargo build` 是绿的（手机上那个函数压根没参与编译）。
#[cfg(desktop)]
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
        // 要比的是 **Cargo.toml 和 tauri.conf.json 这两个版本号**：
        //   界面显示的版本来自 CARGO_PKG_VERSION（Cargo.toml），
        //   而打进安装包、写进签名清单的版本来自 tauri.conf.json。
        // 这俩不是同一个东西，对不上就会出现「界面上写着 0.6.2，
        // 而最新版本提示的是 0.6.3」这种没法解释的情况。
        //
        // 为什么读文件而不是直接写死一个版本号：
        // 原来这条断言是 `assert_eq!(current_version(), "0.6.1")`，
        // 看起来在盯着版本一致性，实际只是盯着「Cargo.toml 是不是那个字符串」。
        // 于是每次升版本号它就红一次，而红的原因跟它声称要防的事毫无关系 ——
        // 真实经历：提到 0.6.2 之后忘了重跑测试，隔了一天才在这里撞见。
        // **一个只能靠「记得别改它」才能过的测试，是坏测试。**
        let conf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
        let text = std::fs::read_to_string(&conf)
            .unwrap_or_else(|e| panic!("读不到 tauri.conf.json：{}", e));
        // 只需要顶层那个 "version"，不去引 serde_json（这里手写两行更省）。
        let conf_version = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("\"version\":"))
            .map(|v| v.trim().trim_end_matches(',').trim_matches('"'))
            .unwrap_or_else(|| panic!("tauri.conf.json 里找不到顶层 version 字段"));

        assert_eq!(
            current_version(),
            conf_version,
            "Cargo.toml 是 {}，tauri.conf.json 是 {}，两个必须一样",
            current_version(),
            conf_version
        );
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

    #[test]
    fn 版本号按数字比而不是按字符串比() {
        // 这一组全按字符串比较都会得出**错误**的结论，
        // 而且前三条在「只有 0.6.x」的世界里根本不会触发 ——
        // 所以它们平时是绿的，等真发到 0.6.10 那天才集体翻车。
        assert!(newer_than("0.6.10", "0.6.9"), "0.6.10 比 0.6.9 新");
        assert!(newer_than("0.7.0", "0.6.99"), "0.7.0 比 0.6.99 新");
        assert!(newer_than("1.0.0", "0.99.99"), "1.0.0 比 0.99.99 新");
        // 字符串比较会栽在这里：'9' > '1'，于是 "0.6.9" > "0.6.10"。
        assert!(!newer_than("0.6.9", "0.6.10"), "0.6.9 比 0.6.10 旧");
    }

    #[test]
    fn 相同版本和缺号段都不算更新() {
        assert!(!newer_than("0.6.3", "0.6.3"));
        // 「缺的一段当 0」：写法不同但意思一样，不该提示更新。
        assert!(!newer_than("0.6", "0.6.0"));
        assert!(!newer_than("0.6.0.0", "0.6"));
        // 带 v 前缀（清单里有人这么写）也要能认。
        assert!(!newer_than("v0.6.3", "0.6.3"));
        assert!(newer_than("v0.6.4", "0.6.3"));
        // 空的 / 认不出来的，一律当成「不比新」，
        // 宁可漏报也不能把用户骗去装一个不明版本。
        assert!(!newer_than("", "0.6.3"));
        assert!(!newer_than("abc", "0.6.3"));
    }

    #[test]
    fn 下载地址只信_github_的_https() {
        assert!(is_trusted_download(
            "https://github.com/71ming/personal-workbench/releases/download/v0.6.4/app.apk"
        ));
        // 下面每一条都必须被挡掉。清单是网上取的，
        // 它说开哪个地址我们就开哪个地址 = 「点更新变成点任意链接」。
        for bad in [
            "http://github.com/x/y.apk",          // 不是 https：明文可被中间人改
            "https://github.com.evil.com/x.apk",  // 后缀伪装：域名根本不是 github
            "https://evil.com/github.com/x.apk",  // 路径伪装
            "file:///sdcard/x.apk",               // 本地文件
            "javascript:alert(1)",                // 脚本
            "not a url at all",
            "",
        ] {
            assert!(!is_trusted_download(bad), "这个地址不该被放行：{}", bad);
        }
    }

    // ---- 手机端清单解析：四条分支全部用单元测试钉死 ----
    //
    // 为什么要费这个劲：真正跑这条代码的只有手机，而手机在墙内
    // 经常连不上 GitHub（实测过：同一台机器上浏览器能上、app 连不上），
    // 也就是说「有新版」那条分支**没法靠真机稳定验证**。
    // 那就只能在这里钉死，别等发出去才发现是坏的。

    const 好的地址: &str =
        "https://github.com/71ming/personal-workbench/releases/download/v0.9.9/x_android.apk";

    #[test]
    fn 手机端_比当前新_要给下载链接和下载模式() {
        let r = android_check_from_text(&format!(
            r#"{{"version":"0.9.9","notes":"说明文字","pub_date":"2026-11-01T08:30:00.000Z","url":"{}"}}"#,
            好的地址
        ));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["available"], json!(true));
        assert_eq!(r["version"], json!("0.9.9"));
        assert_eq!(r["current"], json!(current_version()));
        // 界面靠 mode 决定按钮文案，这个值错了按钮就变成「下载并安装」，
        // 然后去调后端那个手机上根本不会装的接口。
        assert_eq!(r["mode"], json!("download"));
        assert_eq!(r["url"], json!(好的地址));
        assert_eq!(r["notes"], json!("说明文字"));
        assert_eq!(r["date"], json!("2026-11-01"));
    }

    #[test]
    fn 手机端_不比当前新_就说已是最新且不给任何按钮() {
        // 跟当前版本一样大小时，前端会走「已经是最新」那条分支，
        // 根本不看 url/mode。这里顺手断言它们是 null，
        // 免得哪天有人「顺手」把 url 也填上，导致界面多出一个能点的按钮。
        let r = android_check_from_text(&format!(
            r#"{{"version":"{}","url":"{}"}}"#,
            current_version(),
            好的地址
        ));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["available"], json!(false));
        assert_eq!(r["current"], json!(current_version()));
        assert_eq!(r.get("url"), None);
        assert_eq!(r.get("mode"), None);
    }

    #[test]
    fn 手机端_下载地址不可信_要当成清单问题且绝不把地址透出去() {
        // 危险分支：清单是网上来的，它让开哪就开哪 = 点更新变成点任意链接。
        // 断言里特意检查 **响应里不能出现那个地址** ——
        // 光「返回失败」不够，万一哪天改错了还会把地址带给前端。
        let evil = "https://evil.example.com/x.apk";
        let r = android_check_from_text(&format!(
            r#"{{"version":"0.9.9","url":"{}"}}"#,
            evil
        ));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["kind"], json!("manifest"));
        assert!(!r.to_string().contains("evil.example.com"), "不可信的地址不能出现在响应里");
    }

    #[test]
    fn 手机端_清单坏了或缺字段_都当清单问题() {
        for bad in [
            "这不是 json",
            "{}",
            r#"{"version":"0.9.9"}"#,                       // 缺 url
            r#"{"url":"https://github.com/a/b.apk"}"#,       // 缺 version
            r#"{"version":"","url":"https://github.com/a/b.apk"}"#,
            "",
        ] {
            let r = android_check_from_text(bad);
            assert_eq!(r["ok"], json!(false), "这份清单该被判为有问题：{}", bad);
            assert_eq!(r["kind"], json!("manifest"), "清单：{}", bad);
        }
    }
}