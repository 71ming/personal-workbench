// ============================================================================
// AI：服务商配置、探测模型、连通性测试、带引用地问笔记
// ============================================================================
//
// 这一整个文件是 server.js 里 v3 那部分的 Rust 版。两边的返回字段、
// 提示文案、错误分类、引用规则必须**逐条一致** —— 界面上「问 AI」和
// 「AI 设置」那两块是共用同一段 JS 的，返回的东西对不上就是界面报错。
//
// —— 为什么一个调用器能服务六家 ——
//
// DeepSeek / 豆包 / 通义 / Kimi / 智谱 / OpenAI 都实现了同一套协议：
//   POST {base_url}/chat/completions
//   Authorization: Bearer <key>
//   { model, messages: [{role, content}], max_tokens }
//   返回 { choices:[{message:{content}}], usage:{prompt_tokens, completion_tokens} }
// 换厂商因此只是「改一个配置项」，不是「换一套代码」。Claude 用的不是这套协议
// （system 在顶层、x-api-key 鉴权），所以**故意不做**，见下面 PENDING。
//
// —— 三个必须守住的安全规矩 ——
//
// 1. **key 永远从服务端配置读，不从请求参数读**（探测接口除外，且那条不写盘）。
//    否则任何能访问这个接口的东西都能拿你的密钥去调模型、烧你的钱。
// 2. **base_url 只从下面的目录里取，绝不从请求里读**。
//    否则「探测模型」就成了「让本机去访问任意地址」的跳板 ——
//    粘贴抓取那个功能已经踩过这个坑（见 fetch.rs 的 is_private_host）。
// 3. **key 永远不回传前端**，只回「有没有」和一段打码提示。
//    前端是浏览器里的 JS，页面上的一切控制台里都看得到。
//
// —— 不存 cookie、不记日志 ——
// 和抓网页同一个理由：本地小工具，安全边界靠「不留下东西」而不是靠过滤。
//
// ============================================================================

use crate::db;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 连通性测试最多等多久。列模型是快操作，12 秒足够；
/// 聊天要模型思考，给 90 秒。
const PROBE_TIMEOUT_SECS: u64 = 12;
const CHAT_TIMEOUT_SECS: u64 = 90;

/// max_tokens 必须给，而且要**小**。
/// 不给的话某些模型会一路写下去直到撞上下文上限 —— 那是拿你的钱赌。
/// 问笔记不需要长篇大论，2000 字足够。
const MAX_TOKENS: u32 = 2000;

/// 一次最多带多少条笔记、每条最多带多少字。
const MAX_NOTES: usize = 30;
const MAX_NOTE_CHARS: usize = 4000;

// ---------------------------------------------------------------------------
// 服务商目录
// ---------------------------------------------------------------------------
//
// 这份清单是「查证过的快照」，不是永久正确的。
// 查证日期：2026-10。厂商改模型名很频繁 —— DeepSeek 就在 2026-09-10 把
// deepseek-v4-flash 换成了 deepseek-flash，2026-07-24 停用了 deepseek-chat。
// 所以模型名只是「建议值」，用户可以填目录里没有的任何名字
// （所以 PUT /api/ai/config 不校验模型名对不对，只校验填没填、会不会太长）。
const PROVIDERS_JSON: &str = r#"[
  {
    "id": "deepseek", "name": "DeepSeek", "protocol": "openai",
    "base_url": "https://api.deepseek.com",
    "key_url": "https://platform.deepseek.com",
    "models": [
      { "id": "deepseek-flash", "note": "便宜、够用。2026-09-10 起用这个名字（V4.1）" },
      { "id": "deepseek-v4-pro", "note": "更强，也更贵" }
    ],
    "retired": "deepseek-chat / deepseek-reasoner 已于 2026-07-24 停用",
    "note": "国内直连不用代理。有峰谷定价，闲时价格减半。"
  },
  {
    "id": "doubao", "name": "豆包（火山方舟）", "protocol": "openai",
    "base_url": "https://ark.cn-beijing.volces.com/api/v3",
    "key_url": "https://console.volcengine.com/ark",
    "models": [],
    "modelHint": "填火山方舟控制台里的「接入点 ID」（ep- 开头）",
    "note": "国内最便宜的一档，实测适合「读笔记答问」这种短问答。"
  },
  {
    "id": "qwen", "name": "通义千问（阿里云百炼）", "protocol": "openai",
    "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
    "key_url": "https://bailian.console.aliyun.com",
    "models": [
      { "id": "qwen3.8-flash", "note": "便宜，上下文 100 万" },
      { "id": "qwen3.7-plus", "note": "均衡" },
      { "id": "qwen3.8-max", "note": "最强，也最贵" }
    ],
    "note": "新账号通常有免费额度，可以先拿它试。"
  },
  {
    "id": "kimi", "name": "Kimi（月之暗面）", "protocol": "openai",
    "base_url": "https://api.moonshot.cn/v1",
    "key_url": "https://platform.moonshot.cn",
    "models": [
      { "id": "kimi-k3", "note": "当前主力" },
      { "id": "kimi-k2.6", "note": "上一代，长上下文" }
    ],
    "note": "长文档处理见长。"
  },
  {
    "id": "glm", "name": "智谱 GLM", "protocol": "openai",
    "base_url": "https://open.bigmodel.cn/api/paas/v4",
    "key_url": "https://open.bigmodel.cn",
    "models": [
      { "id": "glm-5.2", "note": "" }
    ],
    "modelHint": "模型名更新较频繁，不确定就填控制台里列出的最新一个",
    "note": "有免费额度的新用户活动。"
  },
  {
    "id": "openai", "name": "OpenAI", "protocol": "openai",
    "base_url": "https://api.openai.com/v1",
    "key_url": "https://platform.openai.com",
    "models": [
      { "id": "gpt-6-astra", "note": "通用" },
      { "id": "gpt-6-sol", "note": "偏推理" }
    ],
    "retired": "gpt-4o / o3 / o4-mini 官方标注已不推荐",
    "note": "国内直连需要代理，本工作台不内置代理功能（见方案.md）。"
  }
]"#;

/// Claude 单独列出来但**不接**：它用的是 Anthropic Messages 协议，
/// 请求体结构跟 OpenAI 那套不一样（system 是顶层字段、max_tokens 必填、
/// 鉴权头是 x-api-key 不是 Authorization）。
/// 宁可少一个选项，也不要一个看起来能选、一调就报错的选项。
const PENDING_JSON: &str = r#"[
  { "id": "claude", "name": "Claude", "why": "协议不同，需要单独写一套调用（以后再做）" }
]"#;

/// 价格表。元 / 百万 tokens。**这是一份会过期的快照，不要当权威。**
///
/// 查不到就返回 null，界面只报 token 数 —— 绝不编一个价格出来。
/// token 数来自服务商返回的 usage，那才是准确的；价格只是折算，错了也不影响判断。
const PRICES_JSON: &str = r#"{
  "deepseek-flash":      { "in": 1.01,  "out": 2.02,  "date": "2026-10-14", "note": "闲时（低峰期）价格减半" },
  "deepseek-v4-pro":     { "in": 3.13,  "out": 6.26,  "date": "2026-10-14", "note": "闲时价格减半" },
  "qwen3.8-flash":       { "in": 0.80,  "out": 2.70,  "date": "2026-10-14" },
  "qwen3.7-plus":        { "in": 1.20,  "out": 3.60,  "date": "2026-10-14" },
  "qwen3.8-max":         { "in": 14.0,  "out": 42.0,  "date": "2026-10-14" },
  "doubao-seed-2.0-mini":{ "in": 0.20,  "out": 2.00,  "date": "2026-10-14", "note": "豆包按接入点计费，此为同档参考价" },
  "kimi-k3":             { "in": 4.00,  "out": 20.0,  "date": "2026-10-14" },
  "glm-5.3-flash":       { "in": 0.65,  "out": 2.00,  "date": "2026-10-14" }
}"#;

/// GLM-4.7-Flash 暂不收录：查到两个互相矛盾的价格（$0.07/$0.40 vs 免费），
/// 与其赌一个，不如让界面只报 token 数。

// ============================================================================
// 配置的读写
// ============================================================================

/// 配置放在**数据库旁边**，不放进 SQLite。
///
/// 1) 它跟「笔记 / 任务 / 收藏」完全不是一回事，塞进数据库就得跟着备份走，
///    而密钥绝对不该出现在你可能发给别人看的文件里；
/// 2) 单独一个文件，坏了、想改、想搬到另一台电脑，都是复制一个文件的事。
///
/// Node 版放在 `data/ai-config.json`，桌面版放在应用数据目录 —— 同一个思路，
/// 位置不同是因为「程序旁边的目录」在绿色安装（装进 Program Files）下不可写。
fn config_path(db_path: &Path) -> PathBuf {
    db_path.with_file_name("ai-config.json")
}

/// 打码：只露头 3 个和尾 4 个，中间固定几个点。
/// 定这个规矩是因为不少 key 是「sk-」开头，头部露出来够你确认存的是哪一家的，
/// 又不至于让人看着像在显示完整 key。
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "********".to_string();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{}****{}", head, tail)
}

/// 读配置。文件不存在 / 坏了 / 手改坏了，**都当「没设置过」**，
/// 不该让工作台起不来。
///
/// 顺带保证读回来的**一定是个对象**。手改成 `[1,2]` 或 `123` 也是「坏了」，
/// 而下面写配置时是按对象去改字段的 —— 拿到数组/数字直接改会 panic，
/// 配置文件是用户能碰到的文件，这里必须按最坏情况处理。
fn read_config(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) if v.is_object() => v,
            _ => json!({}),
        },
        Err(_) => json!({}),
    }
}

/// 写配置：先写临时文件再改名。
///
/// 直接覆盖的话，中途断电 / 程序被杀会留下半截 JSON，
/// 下次启动配置全没了 —— 本地小工具最常见的翻车方式。
fn write_config(path: &Path, cfg: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{}", e))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("写配置失败：{}", e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("保存配置失败：{}", e))?;
    Ok(())
}

fn delete_config(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 找服务商。找不到返回 None —— 调用方要靠它拒掉瞎填的 provider id。
fn find_provider(id: &str) -> Option<Value> {
    let list: Vec<Value> = serde_json::from_str(PROVIDERS_JSON).unwrap_or_default();
    list.into_iter().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn providers() -> Value {
    serde_json::from_str(PROVIDERS_JSON).unwrap_or_else(|_| json!([]))
}

fn pending() -> Value {
    serde_json::from_str(PENDING_JSON).unwrap_or_else(|_| json!([]))
}

fn cfg_str(cfg: &Value, key: &str) -> String {
    cfg.get(key).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

/// 给前端看的配置：**没有 key 原文**，只有「有没有」和打码提示。
fn config_public(db_path: &Path) -> Value {
    let cfg = read_config(&config_path(db_path));
    let provider = cfg_str(&cfg, "provider");
    let model = cfg_str(&cfg, "model");
    let key = cfg_str(&cfg, "apiKey");
    let known = find_provider(&provider).is_some();
    json!({
        "provider": provider,
        "model": model,
        "has_key": !key.is_empty(),
        "key_preview": if key.is_empty() { String::new() } else { mask_key(&key) },
        // ready 是「能不能问」的判断依据，前端拿它决定要不要先引导去设置
        "ready": known && !key.is_empty() && !model.is_empty(),
        // 时间格式跟 Node 版略有不同（那边是 2026/10/14 20:30:00，
        // 这里是 2026-10-14 20:30:00）。前端**没有**显示这个字段，
        // 纯存档用，不值得为它引一个时间库。
        "updated_at": cfg_str(&cfg, "updatedAt"),
    })
}

/// GET /api/ai/config
pub fn get_config(db_path: &Path) -> Value {
    json!({
        "providers": providers(),
        "pending": pending(),
        "config": config_public(db_path),
    })
}

/// PUT /api/ai/config
///
/// key 的三种情况用一个显式字段区分，不靠猜：
///   不传 api_key    → 保留原来的（前端只在用户真的输入了新 key 时才带这个字段）
///   传 clear_key    → 清空
///   传 api_key 文本 → 换新的
pub fn put_config(conn: &Connection, db_path: &Path, body: &Value) -> Result<Value, String> {
    // provider 必须是目录里的一个。不认的 id 直接拒 ——
    // 否则调用器会拿着一个用户瞎填的 base_url 去发请求，那既是安全问题也是用户自己的问题。
    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !provider.is_empty() && find_provider(&provider).is_none() {
        return Err(format!("不认识的服务商：{}", provider));
    }

    // 模型名**不校验对不对**，只校验填没填、会不会太长。
    // 理由同上：厂商改名比本程序更新快，拿一份可能过期的白名单去卡用户，
    // 只会让正确的新模型名用不了。
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if model.chars().count() > 120 {
        return Err("模型名太长了".to_string());
    }

    // reset 是显式的「全部清空」开关。
    // 为什么需要它：下面几行是 `if (!x.is_empty())`，空字符串会被当成「没传」跳过，
    // 所以光靠传空串永远清不掉服务商和模型。
    // 用一个显式字段表达「我真的要清空」，不靠猜。
    if body.get("reset").and_then(|v| v.as_bool()) == Some(true) {
        delete_config(&config_path(db_path));
        return Ok(json!({ "config": config_public(db_path) }));
    }

    let path = config_path(db_path);
    let mut cfg = read_config(&path);

    if body.get("clear_key").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(obj) = cfg.as_object_mut() {
            obj.remove("apiKey");
        }
    } else if let Some(k) = body.get("api_key").and_then(|v| v.as_str()) {
        let k = k.trim();
        if !k.is_empty() {
            // 非英文字符一律拒绝，而不是存下去。
            //
            // **HTTP 头只能放 ASCII**，一个带中文（或全角括号、中文空格）的密钥
            // 发出去会被逐字节当成别的字符，拿到的是一串看不懂的 401。
            // 而这种密钥几乎总是「复制时混进了看不见的东西」，
            // 用户看到的现象是「明明从官网复制的，怎么就是不对」——
            // 直接告诉他「密钥里有非英文字符」比让他去猜快得多。
            if k.chars().any(|c| !c.is_ascii()) {
                return Err("密钥里有非英文字符。多半是复制时混进了全角符号或中文空格，请重新复制一次。".to_string());
            }
            cfg["apiKey"] = json!(k);
        }
    }

    if !provider.is_empty() {
        cfg["provider"] = json!(provider);
    }
    if !model.is_empty() {
        cfg["model"] = json!(model);
    }

    // 现在仍然清不空（比如只发了 clear_key），但留着这个判断作为兜底：
    // 万一以后加了别的清空路径，这里保证不会留一份空壳文件在磁盘上。
    let empty = cfg_str(&cfg, "provider").is_empty()
        && cfg_str(&cfg, "model").is_empty()
        && cfg_str(&cfg, "apiKey").is_empty();
    if empty {
        delete_config(&path);
    } else {
        cfg["updatedAt"] = json!(db::now_local(conn));
        write_config(&path, &cfg)?;
    }

    Ok(json!({ "config": config_public(db_path) }))
}

// ============================================================================
// 网络
// ============================================================================

fn http_client(secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .map_err(|e| format!("建网络连接失败：{}", e))
}

/// 判断一个模型名是不是「明显不是用来对话的」。
///
/// Node 版那边是一条正则，这里手写等价逻辑（这个项目不引 regex crate，
/// 为一个过滤规则加个依赖不值）。
///
/// 与 Node 正则的**唯一差别**：`clip` 这条，Node 要求它是独立的一段
/// （`(^|[-_/])clip($|[-_/])`），这里用「按 -_/ 切开后整段匹配」实现同样的意思；
/// 其余关键词都是「包含即命中」，两边一致。
///
/// 判断错了的后果是「把能用的模型藏起来」，所以前端同时有「显示全部」开关 ——
// 宁可多列出来让人自己挑，也别静默吞掉。
fn is_non_chat(model_id: &str) -> bool {
    let lower = model_id.to_lowercase();
    // 这些都是「包含即命中」
    for k in [
        "embedding", "embed", "rerank", "whisper", "tts", "speech", "moderation",
        "dall-e", "dalle", "stable-diffusion", "ocr", "asr", "transcribe",
        "image", "video", "audio",
    ] {
        if lower.contains(k) {
            return true;
        }
    }
    // clip 要按分隔符切开后整段相等才算（clip 出现在 "eclipse" 里不算）
    lower
        .split(|c| c == '-' || c == '_' || c == '/')
        .any(|seg| seg == "clip")
}

/// POST /api/ai/models —— 列出这家当前可用的模型。
///
/// 只列名字，不做聊天：不产生 token 花费，也不用等模型生成。
pub fn detect_models(db_path: &Path, body: &Value) -> Value {
    let provider = body.get("provider").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    // base_url 只从目录里取，绝不从请求里读 —— 否则这个接口就成了
    // 「让本机去访问任意地址」的跳板（粘贴抓取功能已经踩过这个坑）。
    let p = match find_provider(&provider) {
        Some(p) => p,
        None => return json!({ "ok": false, "error": "不认识的服务商" }),
    };
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("这家");

    // 密钥：优先用这次请求带来的（用户刚贴上还没保存的），没有就用已保存的。
    // **这条路径不写盘** —— 探测不等于保存，用户可能只是试一下。
    let key = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let key = if key.is_empty() {
        cfg_str(&read_config(&config_path(db_path)), "apiKey")
    } else {
        key
    };
    if key.is_empty() {
        return json!({ "ok": false, "error": "还没有密钥，先填一个再检测" });
    }

    let base = p.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
    let url = format!("{}/models", base.trim_end_matches('/'));

    let client = match http_client(PROBE_TIMEOUT_SECS) {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {}", key))
        .header("accept", "application/json")
        .send();

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            // 连不上要说清是「超时」还是「网络不通」，不然用户会去查 key，白查半天
            let msg = if e.is_timeout() {
                format!("连 {} 超时（{} 秒没响应）", name, PROBE_TIMEOUT_SECS)
            } else {
                format!("连不上 {}：{}", name, e)
            };
            return json!({ "ok": false, "error": msg });
        }
    };

    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();

    if !(200..300).contains(&status) {
        // 把对方给的错误原文尽量带出来。401/403 单独说，因为那几乎总是 key 错了，
        // 而不是「模型名不对」—— 两件事的排查方向完全不同。
        let detail = error_detail(&text);
        let hint = if status == 401 || status == 403 {
            "密钥可能填错了，或者没激活"
        } else if status == 404 {
            "这家可能不支持列出模型"
        } else {
            ""
        };
        return json!({
            "ok": false,
            "error": format!("{} 返回 {}{}{}", name, status,
                if hint.is_empty() { String::new() } else { format!("（{}）", hint) },
                if detail.is_empty() { String::new() } else { format!("：{}", detail) }),
        });
    }

    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let raw_list = parsed
        .get("data")
        .filter(|v| v.is_array())
        .or_else(|| parsed.get("models").filter(|v| v.is_array()));
    let list = match raw_list {
        Some(v) => v,
        // 返回格式不认识。不报错，但要老实说「我不知道列出来的是什么」，
        // 免得前端显示一个空下拉框，用户以为是「这家没有模型可用」。
        None => return json!({ "ok": false, "error": format!("{} 返回的格式看不懂，没法自动列出模型", name), "raw": true }),
    };

    let mut all: Vec<String> = Vec::new();
    for m in list.as_array().cloned().unwrap_or_default() {
        let id = match &m {
            Value::String(s) => s.clone(),
            _ => m
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| m.get("name").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string(),
        };
        if !id.is_empty() && !all.contains(&id) {
            all.push(id);   // 去重：有些家会重复列
        }
    }
    all.sort();

    let chat: Vec<&String> = all.iter().filter(|m| !is_non_chat(m)).collect();
    // 一条都没有的话，宁可把原始列表交出去，也不要显示「没有可用模型」——
    // 那会让用户以为自己买错了。
    let shown: Vec<String> = if chat.is_empty() {
        all.clone()
    } else {
        chat.iter().map(|s| (*s).clone()).collect()
    };
    let hidden = all.len() - shown.len();

    json!({
        "ok": true,
        "provider": provider,
        "total": all.len(),
        "models": shown,
        // 这个标记告诉前端「过滤掉了一些，可能是误伤」，好让它显示「显示全部」开关
        "filtered": if chat.is_empty() { 0 } else { hidden },
        "hidden": hidden,
        "all_models": all,
    })
}

/// 从一段错误响应里挖出能给人看的那句。
fn error_detail(text: &str) -> String {
    if let Ok(j) = serde_json::from_str::<Value>(text) {
        if let Some(m) = j
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
        {
            return m.to_string();
        }
        if let Some(m) = j.get("message").and_then(|v| v.as_str()) {
            return m.to_string();
        }
    }
    // 不是 JSON 就截前 200 字。对方偶尔会返回一整页 HTML 错误页，
    // 全塞进提示框会把界面撑爆。
    text.chars().take(200).collect()
}

/// 把服务商的 HTTP 错误翻译成人能看懂的话，并分好类。
///
/// 分类的意义：401 是「密钥错了，去设置里改」，模型不对是「重新检测一下」，
/// 超时是「网络慢，再试一次」。几种情况处理完全不同，不能都报一句「失败了」。
///
/// 有个坑：**各家对「模型不存在」用的状态码不一样**。
/// 智谱返回的是 400 + 中文「模型不存在」，不是 404。
/// 所以光看状态码会漏掉这一类，必须也看错误内容 —— 这是 Node 版实测抓出来的。
fn classify_error(status: u16, detail: &str) -> (&'static str, String) {
    let d = detail.trim();
    if status == 401 || status == 403 {
        return (
            "auth",
            format!("密钥无效或没权限。去「AI」设置里检查一下。{}", wrap_detail(d)),
        );
    }
    // 模型名不对：404 是标准做法，但也有厂商用 400 + 一句中文说明。
    let looks_like_model_error = ["模型不存在", "无可用模型", "invalid model", "unknown model"]
        .iter()
        .any(|k| d.to_lowercase().contains(&k.to_lowercase()))
        || d.to_lowercase().contains("model not found")
        || d.to_lowercase().contains("model not exist");
    if status == 404 || looks_like_model_error {
        return (
            "model",
            format!("模型名不对。点「检测模型」重新选一个。{}", wrap_detail(d)),
        );
    }
    if status == 429 {
        return (
            "rate",
            format!("被限流或余额不足，等一会儿再试。{}", wrap_detail(d)),
        );
    }
    if status >= 500 {
        return (
            "server",
            format!("对方服务出问题了（{}），这是他们那边的问题，可以重试。", status),
        );
    }
    (
        "http",
        format!("对方返回 {}{}", status, wrap_detail(d)),
    )
}

fn wrap_detail(d: &str) -> String {
    if d.is_empty() {
        String::new()
    } else {
        format!("（{}）", d)
    }
}

/// 调用成功时拿到的结果。
struct CallOk {
    content: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    cost: Value,
    returned_model: String,
    finish_reason: String,
}

/// 调用失败时带上的信息。`kind` 决定界面上显示哪句「该怎么办」。
struct CallErr {
    kind: &'static str,
    error: String,
    status: Option<u16>,
}

/// 真正发一次请求。
///
/// —— 为什么一律不重试 ——
///
/// 抓网页那次是**幂等**的（GET，重复抓一次不花钱），所以重试无脑做。
/// 调模型是**生成类**的：请求已经送到对方服务器，网络在半路断掉时，
/// 对方很可能已经算完并计费了。再发一次就是花两次钱拿一次回答。
/// 这里只对「连接都没建起来」的情况重试一次（DNS 抖动、连接被拒）——
/// 那种情况对方肯定没收到，不会重复计费。
fn call_model(
    db_path: &Path,
    provider_id: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<CallOk, CallErr> {
    let p = match find_provider(provider_id) {
        Some(p) => p,
        None => {
            return Err(CallErr {
                kind: "config",
                error: "不认识的服务商".to_string(),
                status: None,
            })
        }
    };
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("这家");

    let cfg = read_config(&config_path(db_path));
    // key 只从配置读，调用方不传。就算接口是公开的，也不能让密钥从参数进来 ——
    // 那意味着任何能访问这个接口的东西都能用它去调模型。
    let key = cfg_str(&cfg, "apiKey");
    if key.is_empty() {
        return Err(CallErr { kind: "config", error: "还没保存密钥".to_string(), status: None });
    }
    if model.is_empty() {
        return Err(CallErr { kind: "config", error: "还没填模型名".to_string(), status: None });
    }

    // 请求参数只写一处。下面要重试，同样的东西不该抄两遍 ——
    // 抄两遍就一定会有一遍漏改（Node 版那次就漏了 method: 'POST'，
    // Node 的 fetch 默认 GET，带 body 直接被服务端拒绝）。
    //
    // 注意 reqwest 没开 json feature，所以 body 是自己序列化的字符串。
    let payload = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_prompt },
        ],
        "max_tokens": MAX_TOKENS,
        "stream": false,
    })
    .to_string();

    let client = http_client(CHAT_TIMEOUT_SECS).map_err(|e| CallErr {
        kind: "network",
        error: e,
        status: None,
    })?;
    let url = format!(
        "{}/chat/completions",
        p.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim_end_matches('/')
    );

    let send = || {
        client
            .post(&url)
            .header("authorization", format!("Bearer {}", key))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(payload.clone())
            .send()
    };

    let resp = match send() {
        Ok(r) => r,
        Err(e) => {
            if e.is_timeout() {
                return Err(CallErr {
                    kind: "timeout",
                    error: format!(
                        "等了 {} 秒还没回。可能是模型在思考很久，也可能网络断了。",
                        CHAT_TIMEOUT_SECS
                    ),
                    status: None,
                });
            }
            // 连接都没建起来，对方肯定没收到请求，重试一次不花钱
            match send() {
                Ok(r) => r,
                Err(e2) => {
                    return Err(CallErr {
                        kind: "network",
                        error: format!("连不上 {}：{}", name, e2),
                        status: None,
                    })
                }
            }
        }
    };

    let status = resp.status().as_u16();
    let raw = resp.text().unwrap_or_default();
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

    if !(200..300).contains(&status) {
        let detail = if parsed.is_null() {
            raw.chars().take(200).collect()
        } else {
            error_detail(&raw)
        };
        let (kind, text) = classify_error(status, &detail);
        return Err(CallErr { kind, error: text, status: Some(status) });
    }

    let content = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        // 返回了 200 但没有内容。这通常意味着模型触发了内容过滤，
        // 或者请求里少了 messages。两种都不该假装成功。
        let extra = parsed
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .map(|m| format!("（{}）", m))
            .unwrap_or_else(|| "，可能是内容被过滤了".to_string());
        return Err(CallErr {
            kind: "empty",
            error: format!("对方返回了 200 但没有内容{}", extra),
            status: None,
        });
    }

    let prompt_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as i64;
    let completion_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as i64;

    Ok(CallOk {
        content: content.to_string(),
        prompt_tokens,
        completion_tokens,
        cost: estimate_cost(model, prompt_tokens, completion_tokens),
        // 对方认的这个模型名（有时它会把它映射到具体版本），
        // 记下来让界面能显示「你填的是 A，其实跑的是 B」
        returned_model: parsed
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(model)
            .to_string(),
        finish_reason: parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// 估算这次调用花了多少钱（元）。没有价格收录时返回 JSON null，
/// 界面就只报 token 数 —— 宁可少说，也不要编一个数字。
fn estimate_cost(model: &str, prompt_tokens: i64, completion_tokens: i64) -> Value {
    let prices: Value = serde_json::from_str(PRICES_JSON).unwrap_or(Value::Null);
    let key = model.trim().to_lowercase();
    let pr = match prices.get(&key) {
        Some(p) if !p.is_null() => p,
        _ => return Value::Null,
    };
    let pin = pr.get("in").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pout = pr.get("out").and_then(|v| v.as_f64()).unwrap_or(0.0);
    // 除以 100 万是因为价格单位是「每百万 tokens」
    let yuan = (prompt_tokens as f64 / 1e6) * pin + (completion_tokens as f64 / 1e6) * pout;
    // 保留 6 位小数（1e-6 元 = 百万分之一元），再少就没有意义了
    let rounded = (yuan * 1e6).round() / 1e6;
    json!({
        "yuan": rounded,
        "price_date": pr.get("date").and_then(|v| v.as_str()).unwrap_or(""),
        "note": pr.get("note").and_then(|v| v.as_str()).unwrap_or(""),
    })
}

fn usage_json(prompt: i64, completion: i64) -> Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    })
}

// ============================================================================
// 连通性测试
// ============================================================================

/// 试一句最短的话，验证「密钥 + 模型名 + 网络」这三样通不通。
/// 刻意不喂笔记、不带任何用户数据 —— 这个按钮存在的意义只是连通性测试。
const TEST_PROMPT: &str =
    "你好。这是一条连通性测试，你只需要回复四个字：连接正常。不要多说别的。";

pub fn test_connection(db_path: &Path, body: &Value) -> Value {
    let saved = read_config(&config_path(db_path));
    // 这里必须用**页面上现在填着的**服务商和模型，不能一律读已保存的配置。
    // 原因：「测试连接」最自然的用法就是「换个模型先试一下」，
    // 而换模型只改了输入框、没点保存。要是这里只读磁盘上的配置，
    // 测出来的永远是上一个模型 —— 人只会以为「怎么选都是这一个」，
    // 其实是自己填的名字压根没发出去。没传就回落到已保存的配置。
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg_str(&saved, "provider"));
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cfg_str(&saved, "model"));

    match call_model(db_path, &provider, &model, "你是一个只需要简短回答的助手。", TEST_PROMPT) {
        Err(e) => json!({
            "ok": false,
            "kind": e.kind,
            "error": e.error,
            "tested_model": model,
            "config": config_public(db_path),
        }),
        Ok(r) => {
            // 判断「填的和实际跑的对不上」时，要拿**发出去的那个**名字比。
            // 拿已保存的比是没用的：既然测试用的就是已保存的配置，
            // 这两项本来就会相等，mismatch 永远不会亮。
            let mismatch = !r.returned_model.is_empty()
                && r.returned_model.to_lowercase() != model.to_lowercase();
            json!({
                "ok": true,
                "content": r.content,
                "usage": usage_json(r.prompt_tokens, r.completion_tokens),
                "cost": r.cost,
                "returned_model": r.returned_model,
                "finish_reason": r.finish_reason,
                "mismatch": mismatch,
                "tested_model": model,
                "config": config_public(db_path),
            })
        }
    }
}

// ============================================================================
// 问 AI
// ============================================================================

/// 提示词。写法上的两个要点：
/// - 规则放在最前面、说得极其具体，因为小模型对「请基于以下内容」的遵守度
///   远不如对「你要严格做到以下 4 条」的遵守度；
/// - 明确允许它说「没提」，并且说明这是**正确行为而不是失败** ——
///   不然模型会倾向于给一个泛泛的通用答案（这正是我们要防的事）。
fn system_prompt(note_count: usize) -> String {
    [
        format!("你在帮用户查阅他自己的笔记。你只能依据下面提供的 {} 条笔记回答。", note_count),
        String::new(),
        "必须严格做到这 4 点：".to_string(),
        "1. 每说完一个来自笔记的内容，就在后面用 [笔记N] 标出它来自哪一条。N 是下面笔记的编号。".to_string(),
        format!("2. 如果 {} 条笔记里根本没有相关信息，就直接说「这些笔记里没有提到」，然后停下。", note_count),
        "   绝对不要用你自己的知识来补充，也不要根据常识推测。".to_string(),
        "3. 如果笔记里的说法互相矛盾，把矛盾如实列出来，不要替用户判断哪个对。".to_string(),
        "4. 用中文回答，尽量简洁，不要复述整个笔记内容，只答问题相关的部分。".to_string(),
        String::new(),
        "特别提醒：说「没有提到」是**正确且被鼓励**的行为。".to_string(),
        "编一个听起来合理的答案才是错误。如果你觉得该说的笔记里真没有，就这么答。".to_string(),
    ]
    .join("\n")
}

/// 把勾选的笔记拼成一段带编号的正文。
/// 编号（[笔记1]）同时用于两件事：给模型引用用，以及后面把引用换回链接用。
fn build_note_context(notes: &[(i64, String, String)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, (_, title, body)) in notes.iter().enumerate() {
        let mut b = body.trim().to_string();
        // 按**字符**截，不是按字节。Rust 里 slice 到一个中文中间会 panic，
        // 这是这个项目已经踩过的坑（fetch.rs 里的 strip_scheme）。
        if b.chars().count() > MAX_NOTE_CHARS {
            let cut: String = b.chars().take(MAX_NOTE_CHARS).collect();
            b = cut + "\n…（这条太长了，后面的没给）";
        }
        let t = if title.is_empty() { "无标题" } else { title.as_str() };
        parts.push(format!("[笔记{}] 标题：{}\n{}", i + 1, t, b));
    }
    parts.join("\n\n---\n\n")
}

/// 从模型回答里把 [笔记3] 这类标记抠出来，换成能点回原笔记的引用。
///
/// 返回 (文本, 用到的编号, 编出来的编号个数, 编出来的编号列表)。
///
/// 注意「引用了但我没发这条」的情况：那说明模型在编。
/// 这种情况**保留原样并记下来**，但换成 \u{3} 占位 —— 显示层会把它在正文原位标红。
/// （第一版是「原样保留」，结果页面上 [笔记4] 就是一段混在句子里的普通文字，
///   扫一眼根本看不出来。）
///
/// 真正的引用换成 \u{2} 包住编号。
/// 这里必须用控制符，**不能**写成 CITE1 这种看得见的字样：
/// 模型完全可能自己吐出 "CITE1"（笔记正文里正好写着内部约定时尤其容易）。
/// \u{2} 是控制字符，模型生成不出来，永远撞不了车。
fn parse_citations(answer: &str, note_count: usize) -> (String, Vec<usize>, usize, Vec<usize>) {
    let b = answer.as_bytes();
    const LABEL: &[u8] = "笔记".as_bytes();
    let mut out = String::new();
    let mut used: Vec<usize> = Vec::new();
    let mut bad: Vec<usize> = Vec::new();
    let mut i = 0usize;

    while i < b.len() {
        // 只在 '[' 上尝试解析，其它字符原样搬过去
        if b[i] == b'[' {
            let mut j = i + 1;
            if j + LABEL.len() <= b.len() && &b[j..j + LABEL.len()] == LABEL {
                j += LABEL.len();
                while j < b.len() && (b[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                let digits_start = j;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digits_start && j < b.len() && b[j] == b']' {
                    let num: usize = std::str::from_utf8(&b[digits_start..j])
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                    // 捕获组只圈数字：[笔记1] → num = 1。
                    // （Node 版第一版写成 \[(笔记\s*\d+)\]，把「笔记」两个字也圈进去了，
                    //   于是 Number('笔记1') = NaN，每一条引用都被误判成「引用了不存在的笔记」。）
                    let marker = if num >= 1 && num <= note_count {
                        if !used.contains(&num) {
                            used.push(num);
                        }
                        '\u{2}'
                    } else {
                        if !bad.contains(&num) {
                            bad.push(num);
                        }
                        '\u{3}'
                    };
                    out.push(marker);
                    out.push_str(&num.to_string());
                    out.push(marker);
                    i = j + 1;
                    continue;
                }
            }
        }
        // 原样搬一个完整字符（按 UTF-8 长度跳，保证不会切出半个汉字）
        let len = utf8_len(b[i]);
        out.push_str(&answer[i..(i + len).min(answer.len())]);
        i += len;
    }

    let bad_count = bad.len();
    (out, used, bad_count, bad)
}

/// UTF-8 字符的第一个字节决定它占几个字节。
fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// POST /api/ai/ask
///
/// 要守的四条（每一条都是「用户凭什么敢点这个按钮」）：
///   1. 只发**用户勾选**的笔记，一条没勾就什么都不发；
///   2. 只有主动点「问 AI」才发，保存笔记、打开笔记都不发；
///   3. 要求模型用 [笔记N] 标出处，我们据此生成可点回原笔记的引用；
///   4. 明确告诉模型「笔记里没有的，就说没有」，杜绝它靠通用知识编答案。
pub fn ask(conn: &Connection, db_path: &Path, body: &Value) -> Result<Value, Value> {
    let question = body
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if question.is_empty() {
        return Err(json!({ "error": "问题不能空" }));
    }
    if question.chars().count() > 500 {
        return Err(json!({ "error": "问题太长了" }));
    }

    let raw_ids = body.get("note_ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if raw_ids.is_empty() {
        // 这条要专门强调：**一条笔记都不勾，是不发送任何东西**
        return Err(json!({ "error": "一条笔记都没勾。不会勾就不该发出去 —— 请先勾选想让它看的笔记。" }));
    }

    // 只取整数的 id，最多 MAX_NOTES 条。别的类型直接丢 ——
    // 前端传上来的东西一律不认。
    let ids: Vec<i64> = raw_ids
        .iter()
        .filter_map(|v| v.as_i64())
        .take(MAX_NOTES)
        .collect();

    // 只读勾选的那些，且过滤掉已删除的。
    let mut notes: Vec<(i64, String, String)> = Vec::new();
    for id in ids {
        let found = db::get_note(conn, id).map_err(|e| json!({ "error": e }))?;
        if let Some(n) = found {
            if n.get("deleted_at").map(|v| v.is_null()).unwrap_or(true) {
                let title = n.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body = n.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                notes.push((id, title, body));
            }
        }
    }
    if notes.is_empty() {
        return Err(json!({ "error": "勾选的笔记都找不到了" }));
    }

    let cfg = read_config(&config_path(db_path));
    let t0 = std::time::Instant::now();
    let result = call_model(
        db_path,
        &cfg_str(&cfg, "provider"),
        &cfg_str(&cfg, "model"),
        &system_prompt(notes.len()),
        &format!(
            "以下是我的笔记：\n\n{}\n\n---\n\n我的问题：{}",
            build_note_context(&notes),
            question
        ),
    );
    let ms = t0.elapsed().as_millis() as i64;

    let sent_notes: Vec<Value> = notes
        .iter()
        .map(|(id, title, _)| json!({ "id": id, "title": title }))
        .collect();

    match result {
        Err(e) => Ok(json!({
            "ok": false,
            "kind": e.kind,
            "error": e.error,
            "status": e.status,
            "sent_notes": notes.len(),
            "ms": ms,
        })),
        Ok(r) => {
            let (text, cited, bad_count, bad_nums) = parse_citations(&r.content, notes.len());
            // index 是「第几条笔记」（1 起的序号），id 才是这条笔记在库里的 id。
            // 前端拿 index 去对「发过去的第几条」、拿 id 去打开那条笔记，两个不能混。
            let cited_json: Vec<Value> = cited
                .iter()
                .filter_map(|n| notes.get(*n - 1).map(|(id, title, _)| (*n, *id, title)))
                .map(|(index, id, title)| json!({ "index": index, "id": id, "title": title }))
                .collect();
            Ok(json!({
                "ok": true,
                "question": question,
                "answer": text,
                // 引用存的是笔记 id，前端拿它渲染成「打开这条笔记」
                "cited": cited_json,
                // 模型引用了不存在的编号 = 它在编。必须告诉用户。
                "bad_citations": bad_count,
                // 具体是哪几个编号，前端拿它把正文里那几处标红
                "bad_nums": bad_nums,
                "sent_notes": sent_notes,
                "usage": usage_json(r.prompt_tokens, r.completion_tokens),
                "cost": r.cost,
                "returned_model": r.returned_model,
                "finish_reason": r.finish_reason,
                "ms": ms,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 打码只露头尾() {
        assert_eq!(mask_key("sk-abcdefghijklmnop"), "sk-****mnop");
        // 太短的全盖
        assert_eq!(mask_key("sk-123"), "********");
        assert_eq!(mask_key(""), "********");
    }

    #[test]
    fn 非对话模型要被剔掉() {
        assert!(is_non_chat("text-embedding-3-large"));
        assert!(is_non_chat("bge-reranker-base"));
        assert!(is_non_chat("whisper-1"));
        assert!(is_non_chat("dall-e-3"));
        assert!(is_non_chat("qwen-vl-plus-image"));
        // clip 必须是独立一段：eclipse 不算
        assert!(is_non_chat("clip-vit-base-patch32"));
        assert!(!is_non_chat("eclipse-model"));
        // 正常对话模型不能被误伤
        assert!(!is_non_chat("deepseek-flash"));
        assert!(!is_non_chat("qwen3.8-max"));
        assert!(!is_non_chat("gpt-6-sol"));
        assert!(!is_non_chat("glm-5.2"));
    }

    #[test]
    fn 引用只圈数字() {
        // 正常引用 → \u{2}1\u{2}
        let (t, used, bad, _) = parse_citations("他说过这个 [笔记1] 还有 [笔记2]。", 2);
        assert_eq!(t, "他说过这个 \u{2}1\u{2} 还有 \u{2}2\u{2}。");
        assert_eq!(used, vec![1, 2]);
        assert_eq!(bad, 0);

        // 越界编号 → \u{3}3\u{3}\u{3}，并记成「模型在编」
        let (t, used, bad, bad_nums) = parse_citations("看 [笔记3]", 2);
        assert!(t.contains('\u{3}'));
        assert!(used.is_empty());
        assert_eq!(bad, 1);
        assert_eq!(bad_nums, vec![3]);

        // 「笔记」和数字之间可以有空格（Node 那边的正则也是 \s*）
        let (t, _, _, _) = parse_citations("[笔记 2]", 2);
        assert_eq!(t, "\u{2}2\u{2}");

        // 但数字和 ] 之间有空格就不认 —— 这是**故意跟 Node 版保持一致**的：
        // Node 的正则是 \[笔记\s*(\d+)\]，同样要求 ] 紧跟数字。
        // 这边单方面宽容就会变成「同一段回答，两个后端解析出不同的引用」。
        // 提示词里已经明确要求模型写 [笔记N]，这个边界基本碰不到。
        let (t, used, _, _) = parse_citations("[笔记 2 ]", 2);
        assert_eq!(t, "[笔记 2 ]");
        assert!(used.is_empty());

        // 同样的编号只算一次
        let (_, used, _, _) = parse_citations("[笔记1] [笔记1]", 3);
        assert_eq!(used, vec![1]);

        // 没有引用的原文一个字都不该变
        let orig = "普通回答，里面有 [方括号] 和中文，还有 emoji 🎉。";
        let (t, used, bad, _) = parse_citations(orig, 3);
        assert_eq!(t, orig);
        assert!(used.is_empty());
        assert_eq!(bad, 0);
    }

    #[test]
    fn 模型名错了要能认出来() {
        // 智谱是 400 + 中文「模型不存在」，光看状态码会漏
        let (kind, _) = classify_error(400, "模型不存在");
        assert_eq!(kind, "model");
        let (kind, _) = classify_error(404, "");
        assert_eq!(kind, "model");
        let (kind, _) = classify_error(401, "");
        assert_eq!(kind, "auth");
        let (kind, _) = classify_error(429, "");
        assert_eq!(kind, "rate");
        let (kind, _) = classify_error(503, "");
        assert_eq!(kind, "server");
    }

    #[test]
    fn 价格没收录就说没有_不编数字() {
        assert!(estimate_cost("deepseek-flash", 1000, 1000).get("yuan").is_some());
        assert!(estimate_cost("某个没收录的模型", 1000, 1000).is_null());
        // 模型名大小写不敏感
        assert!(estimate_cost("DeepSeek-Flash", 1000, 1000).get("yuan").is_some());
    }

    #[test]
    fn 一条都不勾就什么都别发() {
        // 用空数据库路径不会真的发请求：校验在发请求之前就该返回。
        let body = json!({ "question": "我写了什么", "note_ids": [] });
        let conn = Connection::open_in_memory().unwrap();
        let err = ask(&conn, Path::new("不存在的路径.db"), &body).unwrap_err();
        assert!(err["error"].as_str().unwrap().contains("一条笔记都没勾"));
    }

    #[test]
    fn 问题不能空() {
        let conn = Connection::open_in_memory().unwrap();
        let err = ask(&conn, Path::new("x.db"), &json!({ "question": "   ", "note_ids": [1] })).unwrap_err();
        assert!(err["error"].as_str().unwrap().contains("不能空"));
    }

    #[test]
    fn 笔记正文超长按字符截不切坏汉字() {
        let long = "汉".repeat(MAX_NOTE_CHARS + 100);
        let notes = vec![(1i64, "标题".to_string(), long)];
        let ctx = build_note_context(&notes);
        // 截断标记必须在，而且整个函数不能 panic（按字节切会 panic）
        assert!(ctx.contains("这条太长了"));
    }

    #[test]
    fn 提示词必须写明没有就说没有() {
        let p = system_prompt(3);
        assert!(p.contains("3 条笔记"));
        assert!(p.contains("这些笔记里没有提到"));
        assert!(p.contains("[笔记N]"));
    }
}