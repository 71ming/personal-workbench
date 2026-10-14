// 抓网页内容（阶段 1c）
//
// 从 server.js 平移过来的。一行 HTTP 客户端都没有 —— 全部手写，
// 这样每一处「为什么」都能写清楚，而不用去翻依赖库的文档。
//
// ==================== 这一节最要紧的是安全 ====================
//
// 这个服务会**替用户去访问任意网址**。用户粘一条链接进来，
// 我们就替他去请求。如果不加拦截，等于开了一条内网通道：
//
//   http://127.0.0.1:8787/api/notes   → 把工作台自己的数据库读出来
//   http://169.254.169.254/            → 云主机的元数据接口（能拿到访问密钥）
//   http://192.168.1.1/admin            → 路由器后台
//
// 所以下面这几道闸一道都不能少，也**不能因为「本地小工具」就省掉**：
//
//  1. 只放行 http/https
//  2. 拦内网地址（回环 / 10.x / 172.16-31.x / 192.168.x / 169.254.x / IPv6 内网）
//  3. **每跳重定向都重新验一次** —— 见下面的「重定向那一段」
//  4. 8 秒超时、2MB 体积上限、Content-Type 校验
//  5. 不存 cookie、不带任何凭据
//
// 第 3 条是最容易被省掉、也最要命的一条：
// 用户粘一个正常的外网地址，那个站把你 302 到 169.254.169.254，
// 你照样乖乖请求过去。只在**入口**查一次是不够的。
//
// 为什么不去支持「登录过的页面」：那就等于要在你的数据库里存网站凭据，
// 泄露风险和「记密码」是一个级别的事。真要做，该做成独立功能单独评估。

use std::time::Duration;

/// 超时：8 秒。跟 server.js 一样。
///
/// 为什么是 8 秒：抓网页是个「锦上添花」的操作，用户在等的时候
/// 界面在转圈。8 秒还没回应，多等一轮只是让用户多生 8 秒的气。
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// 体积上限 2MB。正文用不了这么多，超了直接放弃。
///
/// 不是为了内存安全（2MB 撑不死谁），是为了**别把一篇十万字的长文
/// 存进数据库** —— 那东西打开要卡半天，导出也 bloated。
const MAX_BYTES: usize = 2 * 1024 * 1024;

/// 最多跟 5 跳重定向。再多就是网站在绕你，直接放弃。
const MAX_REDIRECTS: usize = 5;

/// 抓取失败时带的原因。分开「值不值得重试」和「给人看的消息」。
pub struct FetchError {
    pub message: String,
    /// 5xx 和 429 值得重试一次；4xx 是我们自己不该重试，重试只是浪费 8 秒。
    pub retryable: bool,
    /// 超时不重试，见上面的说明。
    pub timed_out: bool,
}

impl FetchError {
    fn new(msg: impl Into<String>, retryable: bool) -> Self {
        Self { message: msg.into(), retryable, timed_out: false }
    }
    fn timeout() -> Self {
        Self { message: "超时（8 秒没响应）".into(), retryable: false, timed_out: true }
    }
}

/// 判断一个主机名是不是内网地址。
///
/// 照搬 server.js 的 `isPrivateHost()`。**判断必须按 IP 段，
/// 不能用字符串前缀** —— `10.0.0.1` 是内网，但 `100.64.0.1` 不是，
/// 前缀匹配会把它错判进去（反了错判成内网只是少抓一个网页，
/// 错了就是放行）。
fn is_private_host(hostname: &str) -> bool {
    let h = hostname.to_ascii_lowercase();
    // 去 IPv6 的方括号：[::1] → ::1
    let h = h.trim_start_matches('[').trim_end_matches(']');

    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    // IPv6 内网：::1（回环）、fc/fd（唯一本地地址）、fe80（链路本地）
    if h == "::1" || h.starts_with("fc") || h.starts_with("fd") || h.starts_with("fe80") {
        return true;
    }

    // IPv4：必须严格是四段数字。不合法的（比如 `999.1.1.1`）交给 DNS 去报错，
    // 这里不拦 —— 拦了反而会挡掉一些合法的写法。
    let parts: Vec<&str> = h.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let nums: Option<Vec<u32>> = parts.iter().map(|p| p.parse::<u32>().ok()).collect();
    let n = match nums {
        Some(v) if v.iter().all(|x| *x <= 255) => v,
        _ => return false,
    };
    let (a, b) = (n[0], n[1]);

    a == 127                          // 回环
        || a == 10                    // 10.0.0.0/8
        || (a == 172 && (16..=31).contains(&b))   // 172.16.0.0/12
        || (a == 192 && b == 168)     // 192.168.0.0/16
        || (a == 169 && b == 254)     // 链路本地（含云元数据 169.254.169.254）
        || a == 0                      // 0.0.0.0/8「本机」
}

/// 从一个 URL 里取出主机名部分。
///
/// 自己切而不用 URL 库：这个模块其余部分也都是手写字符串处理，
/// 混进一个重依赖反而更难看出边界在哪。
/// 返回的是**去掉端口、去掉路径、去掉 IPv6 方括号**的主机名。
///
/// 注意 IPv6 那条分支：`[::1]:8080` 里，端口的冒号是**最后一个**冒号，
/// 但地址本身全是冒号。不能简单 `rfind(':')` 一刀切。
fn hostname_of(url: &str) -> &str {
    let rest = match strip_scheme(url) {
        Some(r) => r,
        None => return "",
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..host_end];

    if host.starts_with('[') {
        // IPv6 带方括号：[::1] 或 [::1]:8080 —— 一直找到第一个 ']'
        if let Some(p) = host.find(']') {
            return &host[1..p];
        }
        return host;   // 没闭合的方括号，交给下面按普通域名处理
    }
    // 普通域名可能带端口：example.com:8080 → example.com
    match host.rfind(':') {
        Some(p) => &host[..p],
        None => host,
    }
}

fn strip_scheme(s: &str) -> Option<&str> {
    let t = s.trim();
    let b = t.as_bytes();
    if b.len() > 7 && b[..7].eq_ignore_ascii_case(b"http://") {
        Some(&t[7..])
    } else if b.len() > 8 && b[..8].eq_ignore_ascii_case(b"https://") {
        Some(&t[8..])
    } else {
        None
    }
}

/// 把 Location 头里的相对地址补成完整的。
///
/// 302 的 Location 经常是 `/a/b` 或 `../c` 这样的相对路径，
/// 直接拿来请求会失败。
fn resolve_redirect(location: &str, base: &str) -> String {
    let loc = location.trim();
    if strip_scheme(loc).is_some() {
        return loc.to_string();   // 已经是完整的 http/https
    }
    // 用 base 的「协议 + 域名」当前缀
    let rest = match strip_scheme(base) {
        Some(r) => r,
        None => return loc.to_string(),
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let root = base[..7 + host_end].trim_end_matches('/');
    let _ = root;

    if loc.starts_with('/') {
        let scheme_end = if base[..7].eq_ignore_ascii_case("http://") { 7 } else { 8 };
        format!("{}{}", &base[..scheme_end + host_end], loc)
    } else {
        // 相对路径：接在 base 最后一段后面
        let dir = match base.rfind('/') {
            Some(i) if i > 8 => &base[..i + 1],
            _ => return format!("{}/{}", root, loc),
        };
        format!("{}{}", dir, loc)
    }
}

/// 抓一个网址。手动处理重定向（每跳一次都重新验一次是不是内网）。
pub fn fetch_page(start_url: &str) -> Result<(String, String), FetchError> {
    // 不自动跟重定向 —— 每跳一次中间地址就看不见了，没地方插内网检查。
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| FetchError::new(format!("建网络连接失败：{}", e), false))?;

    let mut target = start_url.to_string();

    for _ in 0..=MAX_REDIRECTS {
        // ---- 每一跳都重做这三道检查，不能只在入口做一次 ----
        if strip_scheme(&target).is_none() {
            return Err(FetchError::new("只支持 http/https", false));
        }
        let host = hostname_of(&target).to_string();
        if host.is_empty() {
            return Err(FetchError::new("网址里没有域名", false));
        }
        if is_private_host(&host) {
            return Err(FetchError::new(format!("不抓内网地址（{}）", host), false));
        }

        let resp = client
            .get(&target)
            // 老实报自己是本地小工具，别装成浏览器
            // （很多站会直接拒绝假的 UA，或者返回降级页面）
            .header(
                "user-agent",
                "PersonalWorkbench/1.0 (local bookmark fetcher)",
            )
            .header("accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5")
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    FetchError::timeout()
                } else if e.is_redirect() {
                    // 根本没发生（我们禁掉了自动重定向），保险起见不重试
                    FetchError::new("重定向处理异常", false)
                } else {
                    // 连接被拒 / DNS 抖动 —— 值得重试一次
                    FetchError::new(format!("连不上：{}", e), true)
                }
            })?;

        let status = resp.status();
        // ---- 重定向 ----
        if status.is_redirection() {
            match resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                Some(loc) => {
                    target = resolve_redirect(loc, &target);
                    continue;   // 回到循环顶部，重新做内网检查
                }
                None => return Err(FetchError::new("对方让重定向但没给新地址", false)),
            }
        }

        if !status.is_success() {
            let code = status.as_u16();
            return Err(FetchError::new(
                format!("对方返回 {}", code),
                // 5xx 和 429 是「对方暂时不行」，重试有意义；
                // 4xx 是「这个地址就是不对」，重试只是白等 8 秒
                code >= 500 || code == 429,
            ));
        }

        // ---- Content-Type 校验 ----
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(ct.contains("text/html") || ct.contains("text/plain") || ct.contains("application/xhtml")) {
            let kind = ct.split(';').next().unwrap_or("未知类型").trim();
            return Err(FetchError::new(format!("不是网页（{}）", kind), false));
        }

        // ---- 边读边算体积，超了立刻放弃 ----
        // 先看响应头里有没有声明长度，有的话超了直接拒，连读都不用读
        if let Some(len) = resp.content_length() {
            if len > MAX_BYTES as u64 {
                return Err(FetchError::new("网页太大（超过 2MB），已放弃", false));
            }
        }

        let body = resp.bytes().map_err(|e| {
            if e.is_timeout() {
                FetchError::timeout()
            } else {
                FetchError::new(format!("读取失败：{}", e), true)
            }
        })?;

        if body.len() > MAX_BYTES {
            return Err(FetchError::new("网页太大（超过 2MB），已放弃", false));
        }

        // 少数老站点还是 GBK/GB2312，按响应头声明的编码解。
        // Rust 没有内建 GBK 解码器（那是另一个 crate），
        // 这里是**有意识的能力缺失**：解不出中文的老站点会显示乱码，
        // 但不会崩，也不会把数据弄坏 —— 原文总比没有强。
        let html = String::from_utf8_lossy(&body).into_owned();
        return Ok((html, target));
    }

    Err(FetchError::new("重定向跳的次数太多", false))
}

/// 抓一个网址并从里面抠出内容。
///
/// 只重试一次，而且**超时不再重试**（见 `fetch_page` 上面的说明）。
pub fn fetch_bookmark_content(url: &str) -> Result<Picked, FetchError> {
    let mut last: Option<FetchError> = None;
    for attempt in 0..2 {
        match fetch_page(url) {
            Ok((html, _final_url)) => {
                let picked = extract_from_html(&html);
                // 抓到的是登录框 → 算失败，保留原来的标题和正文。
                // 不这么判的话，标题会变成「登录 XXX」，正文是一堆登录按钮，比不抓还糟。
                if looks_like_login_wall(&html, &picked.content, &picked.title) {
                    return Err(FetchError::new(
                        "这个页面要登录才能看，抓到的是登录页",
                        false,
                    ));
                }
                return Ok(picked);
            }
            Err(e) => {
                // 不值得重试的直接返回，别让用户对着转圈多等 8 秒
                if !e.retryable || e.timed_out {
                    return Err(e);
                }
                last = Some(e);
                let _ = attempt;
            }
        }
    }
    Err(last.unwrap_or_else(|| FetchError::new("抓取失败", false)))
}

/// 从 HTML 里抠出来的「标题 / 摘要 / 正文」。
pub struct Picked {
    pub title: String,
    pub summary: String,
    pub content: String,
}

/// 这些块整块扔掉。head 在里面是有意的：
/// 里面的 <title>/<meta> 在抠标题那一步已经单独取过了，
/// 留着只会把一堆脚本和样式冲进正文。
const DROP_BLOCKS: [&str; 12] = [
    "script", "style", "noscript", "svg", "iframe", "head", "nav", "header", "footer", "aside",
    "form", "template",
];

/// 从 HTML 里抠出「标题 / 描述 / 正文」。
///
/// 没有 HTML 解析器可用，走最朴素但够稳的一条路：
/// 先扔掉不该看的块，再把版式标签换成换行，最后收空白。
///
/// **为什么不用解析器**：加一个 HTML 解析 crate 能拿到的，
/// 无非就是更准一点边界处理。而这个功能是「锦上添花」——
/// 抓回来的正文本来就要给人读，不是给程序解析。
/// 为了 100KB 依赖去赌一堆正则的边界情况，不划算。
fn extract_from_html(html: &str) -> Picked {
    let lower = html.to_ascii_lowercase();

    // ---- 标题：og:title（网站自己声明的，最准）→ <title> ----
    let mut title = meta_content(html, &lower, &["property=\"og:title\"", "property='og:title'"])
        .or_else(|| meta_content(html, &lower, &["name=\"og:title\"", "name='og:title'"]))
        .unwrap_or_default();
    if title.is_empty() {
        // <title>…</title>
        if let Some(i) = lower.find("<title") {
            if let Some(start) = html[i..].find('>').map(|p| i + p + 1) {
                if let Some(end) = lower[start..].find("</title>").map(|p| start + p) {
                    title = html[start..end].to_string();
                }
            }
        }
    }
    let title = truncate_chars(&decode_entities(&title).split_whitespace().collect::<Vec<_>>().join(" "), 200);

    // ---- 描述：og:description → meta[name=description] ----
    let mut summary = meta_content(html, &lower, &["property=\"og:description\"", "property='og:description'"])
        .or_else(|| meta_content(html, &lower, &["name=\"description\"", "name='description'"]))
        .unwrap_or_default();
    summary = decode_entities(&summary).split_whitespace().collect::<Vec<_>>().join(" ");

    // ---- 正文 ----
    let mut body = strip_comments(html);
    // 每轮都让 drop_block 自己算小写版 —— 不能把外面那个 `lower` 传进来。
    // body 每删掉一个块长度就变了，而外面那个 lower 还是原始 html 的，
    // 两者长度不一致，后面所有下标都会错位：
    // 表现是「标题抓对了，正文只剩半句」，而且极难看出是哪一步坏的。
    for tag in DROP_BLOCKS {
        body = drop_block(&body, tag);
    }
    // <br> / <hr> 本来就是「另起一行」
    body = replace_tags(&body, &["br", "hr"], "\n");
    // 这些标签在版面上就是另起一行，换成换行符比全塞进一行好读得多
    body = replace_tags(
        &body,
        &["p", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
          "blockquote", "pre", "td", "dd", "dt", "figcaption"],
        "\n",
    );
    // 剩下的标签全删
    body = strip_all_tags(&body);
    let body = decode_entities(&body);

    // 收空白 + 丢碎行 + 丢纯装饰线。
    // 一路 collect 成 String，别留在 &str 里 —— 中间那些
    // `split_whitespace().collect::<Vec<_>>().join(" ")` 会产生临时 String，
    // 引用它们等于借了会被立刻释放的内存（悬垂引用），debug 下能编过、release 下炸。
    let mut kept: Vec<String> = Vec::new();
    for line in body.split('\n') {
        let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
        // 丢掉只剩标点的碎行
        if t.chars().count() > 1 && !is_decoration(&t) {
            kept.push(t);
        }
    }
    let mut content = kept.join("\n");

    if content.chars().count() > 20000 {
        content = truncate_chars(&content, 20000) + "\n…（正文太长，已截断）";
    }

    // ---- 摘要兜底：网页没写 description 就拿正文开头 ----
    // 截在句号处，别拦腰截半句话
    if summary.is_empty() && !content.is_empty() {
        let head: String = content.split('\n').next().unwrap_or("").chars().take(200).collect();
        let cut = head
            .rfind(['。', '.', '！', '？'])
            .filter(|i| *i > 30);
        summary = match cut {
            Some(i) => head.chars().take(i + 1).collect(),
            None => head,
        };
        summary = summary.trim().to_string();
    }
    if summary.chars().count() > 300 {
        summary = truncate_chars(&summary, 300) + "…";
    }

    Picked { title, summary, content }
}

/// 纯装饰线（`----`、`|·|`、`===`）在正文里没有任何信息量，丢掉。
fn is_decoration(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_whitespace() || matches!(c, '|' | '·' | '•' | '-' | '–' | '—' | '*' | '_' | '=' | '›' | '»' | '→')
        })
}

/// 在所有 `<meta ...>` 标签里找 content 属性。
///
/// `needles` 是要匹配的属性串（比如 `property="og:title"`），
/// 两种顺序都试：`property` 在前和 `content` 在前。
fn meta_content(html: &str, lower: &str, needles: &[&str]) -> Option<String> {
    for needle in needles {
        let n = needle.to_ascii_lowercase();
        // 情况一：属性在前，content 在后
        if let Some(i) = lower.find(&n) {
            if let Some(v) = attr_value_after(html, i, "content") {
                return Some(v);
            }
        }
        // 情况二：content 在前，属性在后
        if let Some(i) = lower.find("content=") {
            if let Some(v) = attr_value_after(html, i, needle) {
                return Some(v);
            }
        }
    }
    None
}

/// 从 `pos` 开始的 `>` 之前，找 `name=` 这个属性的值。
fn attr_value_after(html: &str, pos: usize, name: &str) -> Option<String> {
    let tag_end = html[pos..].find('>').map(|p| pos + p)?;
    let tag = &html[pos..tag_end];
    let lower = tag.to_ascii_lowercase();
    let idx = lower.find(&format!("{}=", name.to_ascii_lowercase()))?;
    let rest = tag[idx + name.len() + 1..].trim_start();
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let body = &rest[1..];
        let end = body.find(quote)?;
        Some(body[..end].to_string())
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// 删掉 `<!-- … -->` 注释。
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(e) => rest = &rest[i + e + 3..],
            None => return out,   // 没闭合，剩下的全丢
        }
    }
    out.push_str(rest);
    out
}

/// 删掉 `<tag>…</tag>` 整块。
///
/// **小写版必须在这里现算，不能由调用方传进来。**
/// 调用方手里那个小写版是对**原始字符串**算的，而 s 每删一块长度就变了，
/// 拿它按字节定位就全错位了 —— 症状是「正文少了开头那半句」。
fn drop_block(s: &str, tag: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut out = String::with_capacity(s.len());
    let mut pos = 0usize;   // 已经处理完的位置

    while let Some(rel) = lower[pos..].find(&open) {
        let i = pos + rel;
        // 确认后面确实是标签边界，否则 <scripted> 会被误伤
        let after = s[i + open.len()..].chars().next();
        if matches!(after, Some(c) if c.is_alphanumeric()) {
            out.push_str(&s[pos..i + open.len()]);
            pos = i + open.len();
            continue;
        }
        out.push_str(&s[pos..i]);
        match lower[i..].find(&close) {
            Some(e) => pos = i + e + close.len(),
            None => {
                // 没闭合：把这一行换掉然后放弃（比留下半截脚本强）
                out.push('\n');
                return out;
            }
        }
    }
    out.push_str(&s[pos..]);
    out
}

/// 把 `<tag …>`、`</tag>` 都换成指定文本。
///
/// **必须能带属性**：真实页面里写的是 `<div class="x">` 而不是 `<div>`，
/// 只匹配精确的 `<tag>` 等于一条都没换上，正文会挤成一坨。
fn replace_tags(s: &str, tags: &[&str], with: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0usize;

    loop {
        let Some(rel) = lower[pos..].find('<') else {
            out.push_str(&s[pos..]);
            return out;
        };
        let i = pos + rel;
        let Some(gt_rel) = lower[i..].find('>') else {
            out.push_str(&s[pos..]);
            return out;
        };
        let gt = i + gt_rel;          // '>' 的位置
        let inner = &lower[i + 1..gt];   // 尖括号里面

        // </tag> 是闭标签
        let is_closing = inner.starts_with('/');
        let name_src = if is_closing { &inner[1..] } else { inner };
        let name = name_src
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("");

        if tags.contains(&name) {
            out.push_str(&s[pos..i]);
            out.push_str(with);
            pos = gt + 1;
        } else {
            // 不是目标标签，原样留着，「<」留给下一轮
            out.push_str(&s[pos..i + 1]);
            pos = i + 1;
        }
    }
}

/// 删掉所有 `<…>`。
fn strip_all_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// HTML 实体反转义。`&amp; &nbsp; &#39;` 这些不解出来，正文会脏。
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let i = match rest.find('&') {
            Some(p) => p,
            None => break,
        };
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        // 实体最长 `&#x10ffff;` 也就十几个字符，20 个足够，
        // 找不到分号就当普通的 & 放行
        let limit = after.len().min(20);
        let semi = after[..limit].find(';');
        let end = match semi {
            Some(p) => p,
            None => {
                out.push('&');
                rest = &rest[i + 1..];
                continue;
            }
        };
        let body = &after[..end];
        let decoded = decode_one(body);
        match decoded {
            Some(v) => {
                out.push_str(&v);
                rest = &after[end + 1..];
            }
            None => {
                // 不认识的实体原样保留（比如 `&foo;`）
                out.push_str(&rest[i..i + end + 2]);
                rest = &after[end + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_one(body: &str) -> Option<String> {
    if let Some(num) = body.strip_prefix('#') {
        let (radix, digits) = match num.strip_prefix(['x', 'X']) {
            Some(d) => (16, d),
            None => (10, num),
        };
        let n = u32::from_str_radix(digits, radix).ok()?;
        // 非法码点直接丢掉，免得打出乱码控制符
        return if (1..=0x10ffff).contains(&n) {
            char::from_u32(n).map(|c| c.to_string())
        } else {
            Some(String::new())
        };
    }
    Some(match body.to_ascii_lowercase().as_str() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" | "ensp" | "emsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "ldquo" => "“",
        "rdquo" => "”",
        "lsquo" => "‘",
        "rsquo" => "’",
        "middot" => "·",
        _ => return None,
    }
    .to_string())
}

/// 按**字符**数截断，不是字节数（这项目里栽过这个坑，见 README 坑 #14）。
fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect()
}

/// 判断抓回来的其实是登录框。
///
/// 判断条件写保守点，只在「页面很短」时才下结论 ——
/// 一篇专门讲登录/密码的文章也会命中关键词，但它正文很长，不会被误判。
fn looks_like_login_wall(html: &str, content: &str, title: &str) -> bool {
    if content.chars().count() > 1200 {
        return false;
    }
    // 有密码输入框 —— 这是最硬的信号
    let lower = html.to_ascii_lowercase();
    if lower.contains("type=\"password\"")
        || lower.contains("type='password'")
        || lower.contains("type=password")
    {
        return true;
    }
    // 或者标题和正文都在喊「登录」
    let has_kw = |s: &str| {
        s.contains("登录")
            || s.contains("请先登录")
            || s.contains("需要登录")
            || s.contains("尚未登录")
            || lower_keyword(s)
    };
    has_kw(title) && has_kw(content)
}

fn lower_keyword(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    let squashed = l.replace(' ', "");
    l.contains("signin") || squashed.contains("login")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拦得住内网地址() {
        for h in [
            "127.0.0.1", "127.1.2.3", "localhost", "foo.localhost",
            "10.0.0.1", "10.255.255.255", "172.16.0.1", "172.31.255.255",
            "192.168.1.1", "169.254.169.254", "0.0.0.0",
            "::1", "[::1]", "fd00::1", "fe80::1",
        ] {
            assert!(is_private_host(h), "应该拦住 {}", h);
        }
    }

    #[test]
    fn 不误伤公网地址() {
        for h in [
            "example.com", "www.example.com", "8.8.8.8",
            "172.32.0.1",     // 刚好出了 172.16/12 的上界
            "172.15.0.1",     // 刚好在下界外
            "100.64.0.1",     // CGNAT，不是内网
            "11.0.0.1", "1.1.1.1", "2606:4700::1111",
        ] {
            assert!(!is_private_host(h), "不该拦住 {}", h);
        }
    }

    #[test]
    fn 重定向能补成完整网址() {
        assert_eq!(
            resolve_redirect("/a/b", "https://example.com/x/y"),
            "https://example.com/a/b"
        );
        assert_eq!(
            resolve_redirect("z", "https://example.com/x/y"),
            "https://example.com/x/z"
        );
        assert_eq!(
            resolve_redirect("https://other.com/p", "https://example.com/x"),
            "https://other.com/p"
        );
    }

    #[test]
    fn 主机名切得干净() {
        assert_eq!(hostname_of("https://example.com/a/b?c=1"), "example.com");
        assert_eq!(hostname_of("http://example.com:8080/x"), "example.com");
        assert_eq!(hostname_of("https://[::1]:8080/x"), "::1");
        // 补的两条：IPv6 无端口、域名带端口
        assert_eq!(hostname_of("https://[fe80::1]/x"), "fe80::1");
        assert_eq!(hostname_of("https://example.com"), "example.com");
    }

    #[test]
    fn 切出来的名字仍然拦得住() {
        // 这一条是安全回归：上一版把 [::1]:8080 切成了 "::1]:8080"，
        // 那个字符串**不等于** "::1"，is_private_host 就会放行。
        // 「主机名切错」在这里不是「切得难看」，是「拦不住 ::1」。
        for u in [
            "http://[::1]/", "http://[::1]:8080/", "https://[::1]:9999/a",
            "http://[fd00::1]/", "http://[fe80::1]:80/",
            "http://127.0.0.1:8787/api/notes",   // 攻击自己数据库的那条
            "http://169.254.169.254/latest/meta-data/",
            "http://192.168.1.1/admin",
            "http://10.1.2.3/",
        ] {
            let h = hostname_of(u);
            assert!(is_private_host(h), "{} 切出「{}」却没拦住", u, h);
        }
    }

    #[test]
    fn 实体能解开() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#39;"), "a & b <c> '");
        assert_eq!(decode_entities("&#x4E2D;&#25991;"), "中文");
    }

    #[test]
    fn 正文能从真实结构里抠出来() {
        // example.com 的真实 HTML（简化了空白），抓出来必须含正文，
        // 且不能把 <style> 里的 CSS 混进正文
        let html = r#"<!doctype html><html lang=en><head><meta charset=utf-8><title>Example Domain</title><style>body{color:red}</style></head><body><p>This domain is for use in documentation examples.</p></body></html>"#;
        let p = extract_from_html(html);
        assert_eq!(p.title, "Example Domain");
        assert!(
            p.content.contains("This domain is for use"),
            "正文里没有那句话，实际={:?}",
            p.content
        );
        assert!(!p.content.contains("color:red"), "CSS 混进正文了：{:?}", p.content);
        assert!(!p.content.contains("<"), "标签没删干净：{:?}", p.content);
    }

    #[test]
    fn 带属性的标签也能认出来() {
        // 真实页面写的是 <div class="x">，不是 <div>。
        // 只认精确的 <div> 等于一条都没换上，正文会挤成一坨。
        let p = extract_from_html(r#"<div class="a">第一段</div><div id='b'>第二段</div>"#);
        assert!(p.content.contains("第一段"), "{:?}", p.content);
        assert!(p.content.contains("第二段"), "{:?}", p.content);
        assert!(p.content.contains('\n'), "两段之间没有换行：{:?}", p.content);
    }
}