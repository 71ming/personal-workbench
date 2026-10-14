// 本地 HTTP 服务：把现有前端「原封不动」跑起来
//
// ============================================================================
// 这个文件是整个移植里最重要的一个设计决定，很值得解释清楚。
// ============================================================================
//
// 现在的前端（public/index.html）里，所有接口调用都长这样：
//
//     fetch('/api/notes')
//     fetch(`/api/notes/${currentId}`, { method: 'PUT', ... })
//
// 都是**相对路径**。那么问题来了：窗口怎么加载这个页面？
//
// 方案 A：加载 index.html 这个文件（file:// 协议）
//   → 相对路径会指向磁盘目录而不是服务，fetch 直接失败。
//     要修就得把前端每一处 fetch 都改成写死的绝对地址 —— 几百处改动。
//
// 方案 B：用 Tauri 原生的 invoke() 通道
//   → 同上，每个 fetch 都要重写成 invoke，而且从此不能再用浏览器跑这套前端了。
//
// 方案 C（本文件采用）：Rust 起一个真的 HTTP 服务，窗口加载 http://127.0.0.1:端口
//   → 相对路径天然工作，**前端一行都不用改**，
//     而且 index.html 仍然能直接用浏览器打开，两种方式共用一套代码。
//
// 这也不是我拍脑袋想的：Tauri 官方的移动端方案就是这么做的
// （iOS 上 tauri:// 协议不可用，同样退回本地 HTTP 服务）。
//
// ============================================================================
// 另一个决定：**端口固定 8787，不是随机**。
// ============================================================================
//
// 这条是一开始想错的，中途改回来的，值得记下来。
//
// 最初的方案是绑 0 让操作系统分配空闲端口 —— 听起来更稳，不会跟别人撞车。
// 但有个致命问题：**localStorage 是按 origin 隔离的**，origin = 协议 + 主机 + 端口。
// 前端第 1806 行有一句：
//     let mode = localStorage.getItem('wb-mode') || 'edit';
// 存的是 Markdown 三视图当前用哪个模式。
//
// 端口每次启动都变 = 每次都是新 origin = 每次 localStorage 都是空的
// = 「分栏视图」这个偏好每次启动都白存一场。
// 而且这个 bug 极难发现：界面完全正常，只是有一个偏好设置悄悄失效了。
//
// 撞车怎么办：端口占用时**报错退出**，而不是悄悄换一个端口。
// 因为「8787 被占」这个事实本身就说明工作台已经开着，
// 让用户去切换到已经开着的那个窗口，比给他一个空白的第二实例有用。
// server.js 现在也是这个行为。
//
// ============================================================================

use crate::db;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// 默认端口，跟 server.js 保持一致，同一个地址，习惯和排查都省事。
const DEFAULT_PORT: u16 = 8787;

/// 请求体上限：5MB。跟 server.js 的 5e6 保持一致。
const MAX_BODY: u64 = 5 * 1024 * 1024;

// ============================================================================
// 界面文件内嵌进 exe
// ============================================================================
//
// 这行是修「双击闪退」的正解。
//
// 出事的链条：
//   1. tauri.conf.json 里 frontendDist 指向 ../../public
//   2. 但**编译时 Tauri 不会把 public/ 复制到 target/ 目录里**——
//      frontendDist 只对 `cargo tauri build` 打的**安装包**有意义
//   3. 于是直接跑 target/release/workbench.exe 时，
//      resource_dir() 指向 target/release，那儿没有 public/index.html
//   4. release 又没有控制台（见 lib.rs 的 report_failure）→ 静默退出 → 闪一下就没了
//
// 「优先读磁盘、读不到用内嵌」这个组合，两个需求都满足：
//   开发时：磁盘上有 public/，改完 index.html 刷新窗口就生效，不用重编译
//   发布时：磁盘上没有（或被杀毒软件删了），用编译进 exe 的这一份，一定能开
//
// 代价：改 index.html 会让 cargo 重编本 crate（debug 约 8 秒，release 约 1 分钟）。
// 前端是单文件、自包含、无外部资源，所以内嵌一份就够。
const EMBEDDED_INDEX: &[u8] = include_bytes!("../../../public/index.html");


/// 实际用哪个端口。
///
/// 平时就是 8787。设 `WORKBENCH_PORT` 环境变量可以换 —— 用途是
/// 「你电脑上还开着 server.js，想两个一起跑」这种调试场景。
/// 换了端口要注意：浏览器的 localStorage 是按 origin 隔离的，
/// 端口一换，Markdown 视图模式那个设置就读不到旧的了（见下面 serve 的注释）。
fn resolve_port() -> u16 {
    std::env::var("WORKBENCH_PORT")
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(DEFAULT_PORT)
}

// 起服务，返回实际端口。
//
// 数据库路径由调用方传进来；前端目录可以是 None
//（意思是「磁盘上没有，用内嵌的那一份」，见 EMBEDDED_INDEX）。
pub fn serve(db_path: PathBuf, public_dir: Option<PathBuf>) -> Result<u16, String> {
    let port = resolve_port();

    // 绑 127.0.0.1 而不是 0.0.0.0。
    // 这台机器自己能用，局域网里的其他设备访问不到。
    // 桌面应用就该是这个行为 —— 别把自己的笔记开放给同一个 Wi-Fi 下的人。
    //
    // 用 127.0.0.1 字面量而不是 localhost：localhost 在部分 Windows 上
    // 会优先解析到 IPv6 的 ::1，那样只绑了 IPv4 的服务就连不上了。
    // 写死 IPv4 最省事。
    let addr = format!("127.0.0.1:{}", port);

    // ---- 先用「连接测试」确认端口没人用，再交给 tiny_http ----
    //
    // 踩坑记录，这条是被实测推翻过一次才写对的：
    //
    // 最初用的是「`TcpListener::bind` 成功就说明端口空闲」。
    // 听着很合理，但实测发现**它是错的**：当 8787 已经被 node（server.js）占着时，
    // `TcpListener::bind("127.0.0.1:8787")` **依然返回成功**。
    // 原因是 Windows 的 socket 默认可复用地址，两个进程可以同时 bind 同一端口，
    // 谁来处理请求变得不确定（谁先 accept 谁接）。
    // 换句话说：「bind 成功」根本不能证明「端口是空的」。
    //
    // 换成**连一下试试**：能不能连上，才是真的有人在用那个端口。
    // 这个判断不受 SO_REUSEADDR 影响 —— 端口上有活着的服务就一定连得上。
    if std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port)
            .parse()
            .map_err(|_| "端口格式不对".to_string())?,
        std::time::Duration::from_millis(400),
    )
    .is_ok()
    {
        return Err(format!(
            "端口 {} 上已经有东西在跑。\n\n\
             最常见的原因：**你电脑上那个「启动.bat」还开着**（那是 Node 版，跟桌面版抢同一个端口）。\n\n\
             怎么办：把那个黑色命令行窗口关掉，再双击一次。\n\
             如果你确实想两个一起跑，设个环境变量换端口：set WORKBENCH_PORT=8788",
            port
        ));
    }

    let server = Server::http(&addr).map_err(|e| format!("起服务失败：{}", e))?;
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| "拿不到实际端口".to_string())?;

    // 建表和字段迁移在这里做一次，然后就撒手不管了。
    // 具体为什么不能挪到每个请求里做，见 db::init 的注释。
    db::init(&db_path)?;

    // 数据库路径和前端目录要跨线程用（每个连接一个线程），所以包一层 Arc。
    let db_path = Arc::new(db_path);
    let public_dir = Arc::new(public_dir);

    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            handle(request, &db_path, &public_dir);
        }
    });

    Ok(bound_port)
}

fn handle(mut request: Request, db_path: &Path, public_dir: &Option<PathBuf>) {
    let url = request.url().to_string();
    let method = request.method().clone();
    let (path, query) = split_url(&url);

    if path.starts_with("/api/") {
        let response = match route(&method, &path, &query, request.as_reader(), db_path) {
            Ok(resp) => resp,
            Err(e) => json_response(500, &json!({ "error": e })),
        };
        let _ = request.respond(response);
        return;
    }

    // 不带 /api/ 的一律当静态文件。
    let _ = request.respond(serve_static(&path, public_dir.as_deref()));
}

// ---------- 路由 ----------
//
// 阶段 0 的范围：**只把笔记做成真的**，任务和收藏先返回空数组。
// 这么切的理由：先把「窗口能不能跑起来」这件事验证掉，
// 而不是一上来就平移 71KB 后端。出问题时能立刻定位在哪一层。
//
// 返回空数组而不是报错，是因为前端加载时就会调这几个接口，
// 报错的话顶部会弹红横幅，看着像坏了，其实是阶段 0 还没做到那一步。
fn route(
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    body: &mut dyn Read,
    db_path: &Path,
) -> Result<Response<Box<dyn Read + Send>>, String> {
    // 切路径段。这里**必须先把 "api" 那一段去掉**，否则
    // "/api/notes" 会切成 ["api", "notes"]，拿 seg[0] 去比 "notes"
    // 永远比不上，全部请求都掉进最后的 404。
    //
    // 这个 bug 的烦人之处：服务是活的、端口是通的、JSON 也正常返回，
    // 看起来「像那么回事」，只是内容永远是那句 no such endpoint。
    // 是逐条打接口打出来的，不是读代码读出来的。
    let seg: Vec<&str> = path
        .trim_start_matches("/api/")
        .trim_matches('/')
        .split('/')
        .collect();

    // /api/notes...
    if seg[0] == "notes" {
        match seg.len() {
            // GET /api/notes?search=关键词
            1 if *method == Method::Get => {
                let conn = db::open(db_path)?;
                let kw = query.get("search").map(|s| s.as_str());
                let rows = db::list_notes(&conn, kw)?;
                return Ok(json_response(200, &Value::Array(rows)));
            }
            // POST /api/notes
            1 if *method == Method::Post => {
                let conn = db::open(db_path)?;
                let payload = read_json(body)?;
                let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let note_body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let note = db::create_note(&conn, title, note_body)?;
                return Ok(json_response(200, &note));
            }
            // /api/notes/:id 及其子路径
            2..=3 => {
                let id: i64 = seg[1]
                    .parse()
                    .map_err(|_| "id 不是数字".to_string())?;
                let conn = db::open(db_path)?;

                // /api/notes/:id/restore
                if seg.len() == 3 && seg[2] == "restore" && *method == Method::Post {
                    return match db::restore_note(&conn, id)? {
                        Some(note) => Ok(json_response(200, &note)),
                        None => Ok(json_response(404, &json!({ "error": "not found" }))),
                    };
                }

                if seg.len() != 2 {
                    return Ok(json_response(404, &json!({ "error": "no such endpoint" })));
                }

                match *method {
                    Method::Get => {
                        return match db::get_note(&conn, id)? {
                            Some(note) => Ok(json_response(200, &note)),
                            None => Ok(json_response(404, &json!({ "error": "not found" }))),
                        }
                    }
                    Method::Put => {
                        let payload = read_json(body)?;
                        let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let note_body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
                        return match db::update_note(&conn, id, title, note_body)? {
                            Some(note) => Ok(json_response(200, &note)),
                            None => Ok(json_response(404, &json!({ "error": "not found" }))),
                        };
                    }
                    Method::Delete => {
                        db::soft_delete_note(&conn, id)?;
                        return Ok(json_response(200, &json!({ "ok": true })));
                    }
                    _ => return Ok(json_response(404, &json!({ "error": "no such endpoint" }))),
                }
            }
            _ => return Ok(json_response(404, &json!({ "error": "no such endpoint" }))),
        }
    }

    // /api/search?q=关键词 —— 一次返回所有类型。
    // 阶段 0 只有笔记是真数据，另外两个先空着，但 key 必须都在 ——
    // 前端直接读 data.notes / data.tasks / data.bookmarks，缺一个就报错。
    if seg[0] == "search" && seg.len() == 1 && *method == Method::Get {
        let conn = db::open(db_path)?;
        let keyword = query.get("q").cloned().unwrap_or_default();
        let rows = db::list_notes(&conn, Some(&keyword))?;
        return Ok(json_response(
            200,
            &json!({
                "keyword": keyword,
                "notes": rows,
                "tasks": [],
                "bookmarks": [],
            }),
        ));
    }

    // 阶段 0 的占位：任务和收藏返回空数组，等下个阶段平移。
    if (seg[0] == "tasks" || seg[0] == "bookmarks") && seg.len() == 1 && *method == Method::Get {
        return Ok(json_response(200, &json!([])));
    }

    // 导出：阶段 0 只导出笔记。
    if seg[0] == "export" && seg.len() == 1 && *method == Method::Get {
        let conn = db::open(db_path)?;
        let md = db::export_markdown(&conn)?;
        return Ok(Response::from_string(md)
            .with_status_code(200)
            .with_header(content_type("text/markdown; charset=utf-8"))
            .boxed());
    }

    // AI 配置：阶段 0 完全没实现。
    // 前端启动时会调它，取不到就静默跳过（有 .catch），所以这里给个空的合法结构。
    if seg[0] == "ai" && seg.len() == 2 && seg[1] == "config" && *method == Method::Get {
        return Ok(json_response(200, &json!({ "catalog": [], "config": null })));
    }

    Ok(json_response(404, &json!({ "error": "no such endpoint" })))
}

// ---------- 工具 ----------

// 把 /api/notes?search=%E5%92%96%E5%95%A1 拆成 路径 + 已解码的查询参数。
//
// **必须解码**：前端用 encodeURIComponent 编码，中文会变成 %E7%BA%A2 这种。
// 不解码的话，「搜索咖啡」会拿一串百分号去 LIKE，永远搜不到 ——
// 而且接口不报错，只是搜出空数组，看起来像「没有匹配」而不是「代码有 bug」。
// 这类静默失败最难查。
fn split_url(url: &str) -> (String, HashMap<String, String>) {
    let (path, raw_query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url.to_string(), String::new()),
    };

    let mut query = HashMap::new();
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        query.insert(percent_decode(k), percent_decode(v));
    }
    (path, query)
}

// 解码 %XX 和 +。
// 遇到不认识的写法（残缺的百分号序列，比如促销链接里的 "100%off"），
// 原样保留而不是报错 —— 解码失败就当没有百分号，比整条请求挂掉好。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            // 浏览器表单提交时空格是 +，这里也兼容一下
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// 读请求体并解析成 JSON。
//
// —— 这里有个很容易写反的顺序，踩过就记住了 ——
// 「先读完、再判断大小」是错的：等到你判断的时候，内存已经被那个超大请求吃满了，
// 那句 `if buf.len() > MAX` 就只剩下「告诉别人你把它读进来了」的意义。
//
// 正确做法是**在读之前就限死能读多少**：
// `body.take(MAX_BODY)` 返回一个「最多只肯从 body 里再读 MAX_BODY 字节」的包装器，
// 超出的部分根本不会进内存。Rust 标准库在这一点上设计得很好，
// 用错了纯粹是写法问题，不是库的问题。
//
// 解析失败不当成致命错误：当成空对象，让「缺字段」和「格式错了」
// 走到同一条路径上，少一个分支。
fn read_json(body: &mut dyn Read) -> Result<Value, String> {
    let mut limited = body.take(MAX_BODY);
    let mut buf = String::new();
    limited
        .read_to_string(&mut buf)
        .map_err(|e| format!("读请求体失败：{}", e))?;
    Ok(serde_json::from_str(&buf).unwrap_or_else(|_| json!({})))
}

fn content_type(value: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], value.as_bytes())
        .expect("Content-Type 是我们写死的常量，不可能构造失败")
}

// ============================================================================
// 关于下面这几个函数的返回类型
// ============================================================================
//
// 签名一律写成 `Response<Box<dyn Read + Send>>`，而不是 `Response<String>`
// 或 `Response<Vec<u8>>`。
//
// 原因：tiny_http 的 Response 是个泛型容器，body 的具体类型会一路带到响应上去。
// JSON 响应是 String、静态文件是 Vec<u8>、错误页又是 String —— 三种类型不同，
// 而路由函数只有一个返回类型，装不下它们。
//
// `.boxed()` 是标准库提供的转换：把 `Response<R>` 变成
// `Response<Box<dyn Read + Send>>`，body 在堆上装箱，类型擦除成 trait object。
// tiny_http 的 `respond()` 正好接收这个类型，所以每个出口都得 `.boxed()`，
// **一个都不能漏** —— 漏了就是类型不匹配的编译错误。
// ============================================================================

fn json_response(code: u16, value: &Value) -> Response<Box<dyn Read + Send>> {
    let body = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    Response::from_string(body)
        .with_status_code(StatusCode(code))
        .with_header(content_type("application/json; charset=utf-8"))
        .boxed()
}

// ---------- 静态文件 ----------

// MIME 表照搬 server.js，够用就行 —— 这个应用只有 html/css/js。
fn mime_of(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// public_dir 是 None 时，index.html 直接用内嵌的那份。
fn serve_static(path: &str, public_dir: Option<&Path>) -> Response<Box<dyn Read + Send>> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };

    // 目录穿越防护：请求 /../../Windows/System32 时，
    // 不做这一步检查就等于把整个硬盘的文件读出来。
    // 判断办法是看路径里有没有 ParentDir 分量（也就是 ".."），
    // 有就直接拒绝 —— 比拼字符串再比前缀更不容易出错。
    let requested = Path::new(rel);
    if requested
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
    {
        return Response::from_string("forbidden")
            .with_status_code(403)
            .with_header(content_type("text/plain; charset=utf-8"))
            .boxed();
    }

    // 磁盘上能读到就用磁盘的（开发时改前端立即生效）。
    if let Some(dir) = public_dir {
        if let Ok(bytes) = std::fs::read(dir.join(requested)) {
            let ext = requested
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            return Response::from_data(bytes)
                .with_status_code(200)
                .with_header(content_type(mime_of(ext)))
                .boxed();
        }
    }

    // 磁盘上没有：只对 index.html 退回内嵌的那份。
    // 别的路径该 404 就 404 —— 不然「资源目录配错了」这种问题会被悄悄盖住，
    // 变成「某个图标加载不出来」这种极难定位的现象。
    if requested == Path::new("index.html") {
        Response::from_data(EMBEDDED_INDEX.to_vec())
            .with_status_code(200)
            .with_header(content_type("text/html; charset=utf-8"))
            .boxed()
    } else {
        Response::from_string("not found")
            .with_status_code(404)
            .with_header(content_type("text/plain; charset=utf-8"))
            .boxed()
    }
}
