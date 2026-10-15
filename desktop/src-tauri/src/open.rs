// 个人工作台 · 把链接交给系统浏览器打开
//
// ============================================================================
// 为什么不让浏览器自己跳
// ============================================================================
//
// 界面上收藏那个「打开链接 ↗」原来写的是 `<a target="_blank">`。
// 在普通浏览器里这就是「开新标签」，天经地义；
// 但在 Tauri 窗口里不是那么回事 —— 那是**你自己的应用窗口**，
// 不是浏览器。于是 `target="_blank"` 有两种都不对的可能：
//   1. 应用内部另开一个网页窗口，把工作台窗口盖住，还得一个个关回去；
//   2. 请求被 WebView 直接丢掉，点下去**什么都不会发生**（最气人的那种）。
//
// 到底会落到哪一种，取决于 wry / WebView 的版本和配置，
// 所以正确的做法不是「赌它是哪一种」，而是**根本不让它走浏览器那条路**：
// 由我们自己把地址交给操作系统 —— 「这个链接请用你默认的浏览器打开」，
// 这才是用户点「打开链接」时脑子里的意思。
//
// ============================================================================
// 为什么走本地 HTTP 接口，而不是前端的 Tauri 调用
// ============================================================================
//
// 和自动更新一样的理由：界面是 Node 版和桌面版**共用**的 public/index.html，
// 而桌面版没开 Tauri IPC（withGlobalTauri=false，窗口加载的是 http://127.0.0.1），
// 前端根本没有能调 Rust 的通道。
//
// 走接口还有个附带好处：**两个后端行为天然一致**，
// Node 版那边也有一个真的能跳系统浏览器的实现（本来就跑在浏览器里，
// 但统一走接口以后，界面里就不用写「我这个后端到底行不行」的分支了）。
//
// ============================================================================
// 安全：这里最容易出事
// ============================================================================
//
// 这个接口的职责就是把一个字符串交给操作系统去执行。
// 如果校验不严，攻击者就能通过一个收藏链接让你执行任意命令
// （file:// 打开本地文件是最轻的，更糟的是某些平台上能直接带参数执行程序）。
//
// 所以规矩是**只放行 http 和 https**，而且必须能解析成一个真正的 URL：
//   - 用 Url::parse 而不是正则。解析失败的一律拒绝，理由写得清楚。
//   - 解析成功之后传出去的**不是用户原文，而是 Url 规范化之后的字符串**
//     （空格之类会被编码成 %20 之类）。
//     这一步看着多余，但它保证了交给子进程的东西里不会出现裸的空格、引号、换行。
//   - 交给系统的是**规范化后的 URL**，而且 opener 插件内部不过 shell ——
//     特意不用 `cmd /c start`：后者会把 URL 里的 & | > 当成命令连接符，
//     一个地址就能变成一条命令。
//
// 这三条安全约束**在移动端同样成立**，而且更重要：
// 手机上 opener 走的是原生 Intent，交给系统的也是一个完整 URL，
// 校验放松了就等于让一个收藏链接变成「让手机去做任意一件事」。
// 所以 normalize() 是**两边都要过的那一道门**，不许为了移动端开个口子。
//
// 三条都有下面的单测盯着。

use tauri::Url;

/// 只放行 http / https。别的（file:、javascript:、data:、ms-msdt: 之类）一律拒绝。
///
/// 单独抽出来是为了能测：这不是「顺手加的判断」，是安全边界，必须能被断言。
pub fn normalize(url: &str) -> Result<Url, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("没有网址。".to_string());
    }

    let u = Url::parse(trimmed).map_err(|_| {
        // 解析失败最常见的就是「没写 https://」或者「中文没编码」。
        // 提示里带上这一句，用户不用猜。
        "这不是一个网址（要以 http:// 或 https:// 开头）。".to_string()
    })?;

    match u.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "只能打开网页链接，不支持 {} 开头的地址。",
                other
            ));
        }
    }

    if u.host_str().unwrap_or("").is_empty() {
        return Err("这个网址里没有域名。".to_string());
    }

    Ok(u)
}

/// 交给系统默认浏览器打开。
///
/// 返回 Err 只代表「叫不动系统程序」，不代表链接不合法 ——
/// 合法性和能不能打开是两回事，提示语不能混。
///
/// —— 为什么用 tauri-plugin-opener，而不是自己 spawn 一个程序 ——
///
/// 最早这里写的是手搓的三段 `#[cfg]`：Windows 调 `explorer.exe`、
/// macOS 调 `open`、其它调 `xdg-open`。在桌面上没问题，
/// **但手机上三条全都不存在** —— Android 和 iOS 都没有
/// 「从命令行调另一个程序打开网页」这回事（iOS 沙箱里甚至没有进程派生）。
///
/// 而 `tauri-plugin-opener` **一个 API 覆盖全平台**：
/// `app.opener().open_url(url, None)` 在桌面上调系统程序，在手机上走原生
/// 的 Intent / UIApplication 跳转。这是官方插件，平台差异由它负责，
/// 比自己维护三段 `#[cfg]` 可靠得多。
pub fn open_in_system_browser<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    url: &str,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let u = normalize(url)?;

    // 传 Url 规范化之后的字符串，不是用户原文。理由见文件顶部。
    let target = u.as_str().to_string();

    app.opener()
        .open_url(target, None::<&str>)
        .map_err(|e| format!("打不开默认浏览器：{}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 普通的网页地址放行() {
        let u = normalize("https://github.com/71ming/personal-workbench").unwrap();
        assert_eq!(u.host_str(), Some("github.com"));
        let u = normalize("http://127.0.0.1:8787/").unwrap();
        assert_eq!(u.host_str(), Some("127.0.0.1"));
    }

    #[test]
    fn 前后的空格不碍事() {
        // 用户从别处复制网址过来，几乎总带着首尾空格。
        // 直接拒绝等于让用户自己猜哪里多了个字符。
        assert!(normalize("  https://example.com/a  ").is_ok());
    }

    #[test]
    fn 本地文件绝对不能放行() {
        // 这是最要紧的一条：file:// 能打开磁盘上任意文件，
        // 更不用提某些平台上能直接变成「执行这个程序」。
        assert!(normalize("file:///C:/Windows/System32/cmd.exe").is_err());
        assert!(normalize("file:///etc/passwd").is_err());
    }

    #[test]
    fn 脚本类协议挡住() {
        assert!(normalize("javascript:alert(1)").is_err());
        assert!(normalize("data:text/html,<h1>hi</h1>").is_err());
        assert!(normalize("ms-msdt:/id").is_err());
    }

    #[test]
    fn 不是网址的一律拒绝() {
        assert!(normalize("").is_err());
        assert!(normalize("   ").is_err());
        assert!(normalize("随便写点什么").is_err());
        assert!(normalize("example.com").is_err()); // 少了协议
        assert!(normalize("http://").is_err());       // 没有域名
    }

    #[test]
    fn 交出去的不是用户原文() {
        // 关键的一条：**不能把用户输入原样交给子进程**。
        // Url 解析会把空格之类规范化掉，交出去的必须是规范化后的结果。
        let u = normalize("https://example.com/a b").unwrap();
        assert!(
            !u.as_str().contains(' '),
            "交给子进程的地址里不能有裸空格：{}",
            u.as_str()
        );
    }
}