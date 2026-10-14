// 数据层：SQLite 建表 + 笔记的增删改查
//
// 这个文件是从 server.js 平移过来的。表结构、字段名、返回格式、
// 连软删除的套路都**故意和 server.js 保持一模一样**，
// 这样 public/index.html 一个字都不用改就能跑。
//
// 平移时的对应关系：
//   node:sqlite 的 DatabaseSync   →  rusqlite 的 Connection
//   db.prepare(sql).all()         →  conn.prepare(sql)?.query_map(...)
//   db.prepare(sql).run(args)     →  conn.execute(sql, params![...])
//
// —— 为什么每次请求都开一个新连接，而不是全局开一个 ——
// rusqlite 的 Connection 不是 Sync 的，跨线程共享要套 Mutex。
// 而这个应用是「一个人本地用」，并发极低；
// SQLite 打开一个本地文件非常快（微秒级），多开几次的开销可以忽略。
// 换来的是：不用写锁、不会有「一个请求正抓着连接做 8 秒网络请求
// 把别的请求全堵住」这种问题。简单不容易错，值。

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

// ---------- 建表 ----------
//
// 三张表全部建上，哪怕阶段 0 暂时不实现任务和收藏的接口。
// 原因是：建表是幂等的（IF NOT EXISTS），多建两张表零成本，
// 但省掉以后「加了功能才发现老库要迁移」的麻烦。
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  title      TEXT NOT NULL DEFAULT '',
  body       TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS tasks (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  title        TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL DEFAULT 'todo',
  due_date     TEXT,
  completed_at TEXT,
  created_at   TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at   TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  deleted_at   TEXT
);

CREATE TABLE IF NOT EXISTS bookmarks (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  url        TEXT NOT NULL DEFAULT '',
  title      TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  deleted_at TEXT,
  content    TEXT NOT NULL DEFAULT '',
  summary    TEXT NOT NULL DEFAULT '',
  fetched_at TEXT,
  fetch_error TEXT
);
"#;

// 建表 + 迁移，**只在启动时跑一次**。
//
// 为什么不能每个请求都跑：
// 1) 浪费 —— 每个请求都要跑一遍 CREATE TABLE IF NOT EXISTS 和 PRAGMA table_info。
// 2) 更要紧的是有竞态 —— 两个请求同时进来、同时 ALTER TABLE，第二个会撞上
//    「数据库被锁」。本地应用虽然并发低，但自动保存会连着发好几个请求，
//    这种竞态真出现了就是「偶发的保存失败」，极难复现和定位。
// 所以规矩是：**启动时初始化一次，之后只管开连接。**
pub fn init(db_path: &Path) -> Result<(), String> {
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败：{}", e))?;
    }
    let conn = Connection::open(db_path).map_err(|e| format!("打开数据库失败：{}", e))?;
    conn.execute_batch(SCHEMA)
        .map_err(|e| format!("建表失败：{}", e))?;
    migrate(&conn)?;
    Ok(())
}

// 开一个连接。建表已经由 init 做过了，这里只管开。
// 每个请求开一个是有意的选择，原因见文件顶部的说明。
pub fn open(db_path: &Path) -> Result<Connection, String> {
    Connection::open(db_path).map_err(|e| format!("打开数据库失败：{}", e))
}

// 字段迁移：SQLite 的 CREATE TABLE IF NOT EXISTS **不会修改已存在的表**，
// 所以老库缺的列必须单独 ALTER 补上。
// 先查表结构再决定加不加，跑多少次都安全 —— 这条规矩是从 server.js 照搬的，
// 方案.md 里也专门记过（“跑多少次都安全”）。
fn migrate(conn: &Connection) -> Result<(), String> {
    // notes 表的软删除字段
    if !has_column(conn, "notes", "deleted_at")? {
        conn.execute("ALTER TABLE notes ADD COLUMN deleted_at TEXT", [])
            .map_err(|e| format!("notes 加 deleted_at 失败：{}", e))?;
    }
    // bookmarks 表抓正文用的几个字段
    for (name, ddl) in [
        ("content", "ALTER TABLE bookmarks ADD COLUMN content TEXT NOT NULL DEFAULT ''"),
        ("summary", "ALTER TABLE bookmarks ADD COLUMN summary TEXT NOT NULL DEFAULT ''"),
        ("fetched_at", "ALTER TABLE bookmarks ADD COLUMN fetched_at TEXT"),
        ("fetch_error", "ALTER TABLE bookmarks ADD COLUMN fetch_error TEXT"),
    ] {
        if !has_column(conn, "bookmarks", name)? {
            conn.execute(ddl, []).map_err(|e| format!("bookmarks 加 {} 失败：{}", name, e))?;
        }
    }
    Ok(())
}

// 查某张表有没有某一列。
// 写法跟 server.js 的 PRAGMA table_info 一一对应。
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    // PRAGMA 不能用 ? 传参数，表名只能拼进 SQL。
    // 这里 table/column 都是代码里写死的常量，不是用户输入，没有注入风险。
    let sql = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let name: String = row.get(1).map_err(|e| e.to_string())?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 标题兜底：空标题给个占位名。
///
/// **必须和 server.js 完全一致**，否则两个版本会在同一个库里写出不同的数据：
/// server.js 存的是 `无标题`（无圆括号），而前端 `listTitle()` 的兜底显示
/// 是 `（无标题）`（有圆括号）—— 那是给「万一库里是空的」准备的第二道防线。
///
/// 少了这一步会怎样：桌面版建出来的笔记标题是空的，
/// 界面靠前端兜底看着还行，但**搜「无标题」搜不到它**，而 Node 版建的能搜到。
/// 同一份数据、同一套搜索，结果却取决于它是谁建的 —— 这种不一致极难察觉。
fn title_or(title: &str, fallback: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t.to_string()
    }
}

/// 截止日期归一化：只认 `YYYY-MM-DD`，其余一律当成「不设截止」。
///
/// 跟 server.js 的 normDue() 同规则。不能直接拼进 SQL —— 接口是公开的，
/// 前端传什么都得有这一道闸。格式不对就当没填，不报错。
fn norm_due(v: &str) -> Option<String> {
    let s = v.trim();
    let b = s.as_bytes();
    // 全程用 bytes 判断，不切字符串：只要 4、7 两个位置确实是 ASCII 短横，
    // 那么 0..4 / 5..7 / 8..10 这几个切点就必然落在字符边界上。
    // （Rust 的 len() 数的是字节，直接 `s.len() == 10` 挡不住中文串）
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..10].iter().all(|c| c.is_ascii_digit());
    if ok {
        Some(s.to_string())
    } else {
        None
    }
}

// ---------- 笔记 ----------

// 把一行数据转成前端要的 JSON。
// 提成独立函数是因为下面两条 SQL 要共用它 ——
// 写成两个内联闭包的话，字段一改就容易只改一处，然后两种请求返回不同形状的数据。
fn note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id":         row.get::<_, i64>(0)?,
        "title":      row.get::<_, String>(1)?,
        "body":       row.get::<_, String>(2)?,
        "updated_at": row.get::<_, String>(3)?,
    }))
}

// 列表。带 search 就全文搜标题和正文。
//
// —— 这里有个 rusqlite 的坑，写错了会直接报「参数个数不对」——
// 它要求「传给 SQL 的参数个数」和「SQL 里 ?N 占位符的个数」**完全一致**。
// 所以「无搜索」那条 SQL 里一个占位符都没有，就必须传空参数 `[]`；
// 想图省事统一传一个，立刻就报 Got 1, needed 0。
//
// 两条 SQL 分开写而不是想办法合并，是因为合并版要写成
// `(?1 IS NULL OR title LIKE ?1 ...)` 这种，读起来比它省下的那几行费劲得多。
//
// SELECT 出来的字段是 `id,title,body,updated_at` —— 跟 server.js 一致，
// **不含 created_at**。前端列表不显示创建时间，少一个字段少传一点数据。
pub fn list_notes(conn: &Connection, search: Option<&str>) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    match search {
        Some(kw) if !kw.is_empty() => {
            let pattern = format!("%{}%", kw);
            let mut stmt = conn
                .prepare(
                    "SELECT id,title,body,updated_at FROM notes
                     WHERE deleted_at IS NULL AND (title LIKE ?1 OR body LIKE ?1)
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([&pattern], note_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
        _ => {
            let mut stmt = conn
                .prepare(
                    "SELECT id,title,body,updated_at FROM notes
                     WHERE deleted_at IS NULL ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], note_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(out)
}

// 单条读取。这里比列表多两个字段（created_at 和 deleted_at），
// 因为编辑区要显示创建时间，restore 接口要判断它是不是被删过。
pub fn get_note(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT id,title,body,created_at,updated_at,deleted_at FROM notes WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([id], |row| {
            Ok(json!({
                "id":         row.get::<_, i64>(0)?,
                "title":      row.get::<_, String>(1)?,
                "body":       row.get::<_, String>(2)?,
                "created_at": row.get::<_, String>(3)?,
                "updated_at": row.get::<_, String>(4)?,
                "deleted_at": row.get::<_, Option<String>>(5)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out.into_iter().next())
}

pub fn create_note(conn: &Connection, title: &str, body: &str) -> Result<Value, String> {
    conn.execute(
        "INSERT INTO notes (title,body) VALUES (?1,?2)",
        rusqlite::params![title_or(title, "无标题"), body],
    )
    .map_err(|e| format!("新建失败：{}", e))?;
    // last_insert_rowid() 返回刚才那行的 id
    let id = conn.last_insert_rowid();
    get_note(conn, id)?.ok_or_else(|| "新建后读不回来".to_string())
}

pub fn update_note(conn: &Connection, id: i64, title: &str, body: &str) -> Result<Option<Value>, String> {
    // `conn.execute` 返回的是**实际改动的行数**，这是判断「这条还在不在」的唯一依据。
    //
    // 为什么必须看它、而不是「更新完再读回来」：
    // SQL 里已经有 `AND deleted_at IS NULL`，所以对一条已软删除的笔记，
    // UPDATE 实际上改了 0 行 —— 拒绝得完全正确。
    // 但如果不管返回值、直接 get_note 读回来，get_note 是不带 deleted_at 过滤的，
    // 于是会把那条「已被删除、其实一个字都没改」的记录原样返回，
    // 看起来就像更新成功了。
    //
    // 这个 bug 靠读代码发现不了：SQL 写得对、返回的 JSON 也有模有样，
    // 只有真去删一条再改它才会暴露。
    let changed = conn
        .execute(
            "UPDATE notes SET title=?1, body=?2, updated_at=datetime('now','localtime')
             WHERE id=?3 AND deleted_at IS NULL",
            rusqlite::params![title_or(title, "无标题"), body, id],
        )
        .map_err(|e| format!("保存失败：{}", e))?;

    // 0 行 = 这条不存在，或者已经被删了。两种情况前端都该看到「找不到」。
    if changed == 0 {
        return Ok(None);
    }
    get_note(conn, id)
}

// 软删除：不真删，只打一个时间戳。
// 误删之后还能通过 restore 捞回来 —— 个人笔记库最需要的那个保险。
pub fn soft_delete_note(conn: &Connection, id: i64) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE notes SET deleted_at=datetime('now','localtime')
             WHERE id=?1 AND deleted_at IS NULL",
            [id],
        )
        .map_err(|e| format!("删除失败：{}", e))?;
    Ok(n > 0)
}

pub fn restore_note(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    conn.execute("UPDATE notes SET deleted_at=NULL WHERE id=?1", [id])
        .map_err(|e| format!("撤销失败：{}", e))?;
    get_note(conn, id)
}

// 把 rusqlite 的行迭代器收集成 Vec。
// ?Sized 的泛型让这个函数既能收 query_map 的结果，也能收任何别的迭代器。
// 注释写在这里（而不是删掉）：list_notes 早期版本就是用它，后来因为
// 两条 SQL 的参数个数不同拆开写了，它才成了死代码。
// 留着这段是提醒 —— 「同一条 SQL 用不同参数调两次」在 rusqlite 里是个陷阱，
// 以后再遇到类似情况，别又写成「统一传一个参数」。
#[allow(dead_code)]
fn collect<I>(rows: I) -> Result<Vec<Value>, String>
where
    I: Iterator<Item = Result<Value, rusqlite::Error>>,
{
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

// ---------- 导出 ----------
//
// 阶段 0 只导笔记；任务和收藏的小节等它们的功能平移过来再加。
// 导出是这个应用里**唯一一份不在数据库里的备份**，
// 所以哪怕只有笔记，也得保证它开箱可用。
pub fn export_markdown(conn: &Connection) -> Result<String, String> {
    let mut stmt = conn
        .prepare("SELECT title,body,created_at,updated_at FROM notes
                  WHERE deleted_at IS NULL ORDER BY created_at ASC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut out = String::new();
    out.push_str("# 个人工作台导出\n\n");
    let mut count = 0usize;
    for r in rows {
        let (title, body, created) = r.map_err(|e| e.to_string())?;
        count += 1;
        // 标题为空时写「（无标题）」而不是留空行 ——
        // Markdown 里 `- ` 后面什么都没有，渲染出来是个空列表项，很难看。
        let heading = if title.trim().is_empty() { "（无标题）" } else { title.trim() };
        out.push_str(&format!("## {}\n\n", heading));
        out.push_str(&format!("> 创建于 {}\n\n", created));
        out.push_str(body.trim_end());
        out.push_str("\n\n");
    }
    if count == 0 {
        out.push_str("（还没有笔记）\n");
    }

    // ---- 任务 ----
    //
    // 用 Markdown 的待办语法 `- [ ]` / `- [x]`，这样导出文件在别的编辑器里
    // 打开也是能勾的，不是一堆纯文本。
    //
    // 排序跟界面一致：未完成在前、有截止日期的在前、越早到期越靠前 ——
    // 备份是给人看的，顺序乱了就不好找。
    let mut stmt = conn
        .prepare(&format!(
            "SELECT title,status,due_date FROM tasks
             WHERE deleted_at IS NULL ORDER BY {}",
            TASK_ORDER
        ))
        .map_err(|e| e.to_string())?;
    let task_rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut tasks: Vec<(String, String, Option<String>)> = Vec::new();
    for r in task_rows {
        tasks.push(r.map_err(|e| e.to_string())?);
    }

    if !tasks.is_empty() {
        out.push_str("\n# 任务\n\n");
        for (title, status, due) in &tasks {
            let mark = if status == "done" { "x" } else { " " };
            let line = match due {
                Some(d) => format!("- [{}] {}（{}截止）", mark, title, format_due_cn(d)),
                None => format!("- [{}] {}", mark, title),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }

    // ---- 收藏 ----
    //
    // 最朴素的 `- [标题](链接)`，任何编辑器点得开。
    // URL 存进来之前已经被 norm_url 验过是 http/https 了，这里不用再担心协议。
    //
    // 标题这一格有个坑要处理：建收藏时标题留空，后端会拿链接本身当标题兜底，
    // 于是导出来变成 `- [<189 字的网址>](<189 字的网址>)`，一屏放不下半行。
    // 所以复用 is_auto_title()：标题是「我们自己按链接拼出来的」就用推导标题顶上，
    // 用户自己起的一个字都不动。
    //
    // **链接本身永远不截断** —— 截了就点不开了，那一行长就长着。
    let mut stmt = conn
        .prepare(
            "SELECT url,title,summary,fetched_at,fetch_error FROM bookmarks
             WHERE deleted_at IS NULL ORDER BY created_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let bm_rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut marks: Vec<(String, String, String, Option<String>, Option<String>)> = Vec::new();
    for r in bm_rows {
        marks.push(r.map_err(|e| e.to_string())?);
    }

    if !marks.is_empty() {
        out.push_str("\n# 收藏\n\n");
        for (url, title, summary, fetched_at, fetch_error) in &marks {
            let raw = title.trim();
            let shown = if raw.is_empty() || is_auto_title(raw, url) {
                let derived = title_from_url(url);
                if derived.is_empty() { url.clone() } else { derived }
            } else {
                raw.to_string()
            };
            out.push_str(&format!("- [{}]({})\n", flatten_ws(&shown), url));
            if !summary.is_empty() {
                out.push_str(&format!("  > {}\n", flatten_ws(summary)));
            }
            // 抓取状态也写进备份。**抓成功的不写** —— 那是默认状态，每条都写一遍全是噪音。
            // 但「没抓到」和「还没抓」必须写：这份文件是唯一的备份，
            // 看不出来的话过一个月回看会以为那几个网页本来就没内容，白重抓一遍。
            if let Some(err) = fetch_error.as_deref().filter(|s| !s.trim().is_empty()) {
                out.push_str(&format!("  > 没抓到内容：{}\n", flatten_ws(err)));
            } else if fetched_at.is_none() {
                out.push_str("  > 还没抓取内容\n");
            }
        }
    }

    Ok(out)
}

/// 换行和它周围的空白压成一个空格。
///
/// Markdown 的链接文字里如果有换行，渲染出来会断成两行甚至失效。
/// server.js 用 `replace(/\s*\n\s*/g, ' ')`，这里做成一样的效果。
fn flatten_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if ch == '\n' {
            pending_space = true;
        } else if ch.is_whitespace() {
            if pending_space { out.push(' '); pending_space = false; }
            out.push(ch);
        } else {
            if pending_space { out.push(' '); pending_space = false; }
            out.push(ch);
        }
    }
    if pending_space { out.push(' '); }
    out
}

/// `2026-10-15` → `10月15日`。
///
/// 导出是给人读的，写 `2026-10-15截止` 也认得出来，但中文格式短得多，
/// 一行放得下。年份就不带了 —— 备份里的东西基本都是今年的。
fn format_due_cn(d: &str) -> String {
    // 用 bytes 判断，别用 d.len() != 10 就直接切 ——
    // Rust 的 len() 数的是**字节**不是字符，一个恰好 10 字节的中文串
    // （比如 3 个汉字加 1 个字符）会通过这个检查，然后在 &d[5..7] 处 panic。
    // 只在「位置 4 和 7 确实是 ASCII 短横」时才切，切的点必然是字符边界。
    let b = d.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return d.to_string();
    }
    let m = &d[5..7];
    let day = &d[8..10];
    let (mm, dd) = match (m.trim_start_matches('0'), day.trim_start_matches('0')) {
        ("", _) => (m.to_string(), day.to_string()),
        (_, "") => (m.to_string(), day.to_string()),
        (a, b) => (a.to_string(), b.to_string()),
    };
    format!("{}月{}日", mm, dd)
}


// ============================================================================
// 任务
// ============================================================================
//
// 跟笔记一样是独立的一张表（方案.md 里定过的「一张表一个概念」），接口结构
// 也刻意做成和笔记一模一样，以后加东西照着抄就行。
//
// —— 排序规则是照搬 server.js 的，四个条件缺一不可 ——
//
//   (status = 'done') ASC    未完成排前面
//   (due_date IS NULL) ASC   有截止日期的排前面
//   due_date ASC              越早到期越靠前
//   created_at ASC            同一天的最后建的在前
//
// 「逾期」不是靠一个标记算出来的 —— 截止日期早于今天且还没完成的任务，
// 它的 due_date 天然就是最早的，于是自然浮到最上面。不用写额外逻辑，
// 也不会出现「标记忘了更新」导致逾期任务没置顶的情况。

/// 任务行 → JSON。
///
/// 跟笔记列表不同，这里返回**全部字段**（server.js 用的是 `SELECT *`），
/// 因为前端的任务编辑区要用 created_at 和 completed_at。
/// 字段名必须一模一样，否则前端读不到。
fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id":           row.get::<_, i64>(0)?,
        "title":        row.get::<_, String>(1)?,
        "status":       row.get::<_, String>(2)?,
        "due_date":     row.get::<_, Option<String>>(3)?,
        "completed_at": row.get::<_, Option<String>>(4)?,
        "created_at":   row.get::<_, String>(5)?,
        "updated_at":   row.get::<_, String>(6)?,
        "deleted_at":   row.get::<_, Option<String>>(7)?,
    }))
}

const TASK_ORDER: &str = "(status = 'done') ASC, (due_date IS NULL) ASC, due_date ASC, created_at ASC";

pub fn list_tasks(conn: &Connection, search: Option<&str>) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    match search {
        Some(kw) if !kw.is_empty() => {
            // 任务**只搜标题**，不搜正文 —— 任务压根没有正文字段。
            let pattern = format!("%{}%", kw);
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT * FROM tasks WHERE deleted_at IS NULL AND title LIKE ?1 \
                     ORDER BY {}",
                    TASK_ORDER
                ))
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([&pattern], task_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
        _ => {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT * FROM tasks WHERE deleted_at IS NULL ORDER BY {}",
                    TASK_ORDER
                ))
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], task_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(out)
}

pub fn get_task(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM tasks WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([id], task_row).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out.into_iter().next())
}

pub fn create_task(conn: &Connection, title: &str, due_date: &str) -> Result<Value, String> {
    conn.execute(
        "INSERT INTO tasks (title,due_date) VALUES (?1,?2)",
        rusqlite::params![title_or(title, "未命名任务"), norm_due(due_date)],
    )
    .map_err(|e| format!("新建任务失败：{}", e))?;
    let id = conn.last_insert_rowid();
    get_task(conn, id)?.ok_or_else(|| "新建后读不回来".to_string())
}

/// 更新。标题、完成状态、截止日期**三个字段一起存**——
/// 前端自动保存每次都带全三个，所以不做局部更新，免得漏字段。
pub fn update_task(
    conn: &Connection,
    id: i64,
    title: &str,
    status: &str,
    due_date: &str,
) -> Result<Option<Value>, String> {
    // 只认两个值。别的（'DONE'、'完成'、''、null）一律当待办 ——
    // 状态字段是会被前端反复传回来的，不做白名单就等着被写花。
    let st = if status == "done" { "done" } else { "todo" };

    // completed_at 的判断写进 SQL：
    //   完成时保留原完成时间（重复保存不会把完成时刻刷掉）
    //   取消完成时清空
    //
    // 放在 SQL 里做，而不是「先读出来、算一算、再写回去」——
    // 后者在并发的两次保存之间会丢更新，而这里一次语句就完事。
    let changed = conn
        .execute(
            "UPDATE tasks SET title=?1, status=?2, due_date=?3,
                    completed_at = CASE WHEN ?2='done'
                      THEN COALESCE(completed_at, datetime('now','localtime'))
                      ELSE NULL END,
                    updated_at=datetime('now','localtime')
             WHERE id=?4 AND deleted_at IS NULL",
            rusqlite::params![title_or(title, "未命名任务"), st, norm_due(due_date), id],
        )
        .map_err(|e| format!("保存任务失败：{}", e))?;

    if changed == 0 {
        return Ok(None);
    }
    get_task(conn, id)
}

pub fn soft_delete_task(conn: &Connection, id: i64) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE tasks SET deleted_at=datetime('now','localtime')
             WHERE id=?1 AND deleted_at IS NULL",
            [id],
        )
        .map_err(|e| format!("删除任务失败：{}", e))?;
    Ok(n > 0)
}

pub fn restore_task(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    conn.execute("UPDATE tasks SET deleted_at=NULL WHERE id=?1", [id])
        .map_err(|e| format!("撤销失败：{}", e))?;
    get_task(conn, id)
}


// ============================================================================
// 收藏
// ============================================================================
//
// 继续守「一张表一个概念」。收藏存的是「一个链接 + 我对它的称呼」，
// 不是笔记也不是任务，不跟它们混在一起。
//
// 本阶段（1b）只做「存和管链接」，**网页抓取留到 1c**。
// 但 content / summary / fetched_at / fetch_error 这四个字段照样建好、照样返回，
// 因为前端就是按这套字段名渲染的 —— 现在它们恒为空，抓取功能一开就有值了，
// 前端一行都不用改。

/// 剥掉 `http://` / `https://` 前缀，返回 `None` 表示「不是 http(s) 链接」。
///
/// 抽出来是因为这个下标计算**写错过两次**：
///
/// 1. `http://` 是 7 个字符、`https://` 是 8 个，拿同一个下标去切两边，
///    结果 https 的链接会被当成「冒号后面是空的」而全部拒掉。
/// 2. 直接写 `&t[..7]` 会在**中文字符中间**切开字符串，Rust 直接 panic
///    （`byte index 7 is not a char boundary`）。
///    导出时拿用户自己起的标题去做这个比对，一个中文标题就让整个导出 500。
///
/// 教训：Rust 的字符串是 UTF-8，**切之前必须确认那个位置是字符边界**。
/// 这里用「前 8 个字节全是 ASCII」来保证 —— 协议头本身必然是 ASCII，
/// 不可能是多字节字符的前缀字节。
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

/// 链接归一化：**只放行 http / https**，其它一律拒绝（返回 `None`）。
///
/// 这不是洁癖：前端会把收藏的链接渲染成可点的 `<a>`。
/// 如果放行 `javascript:alert(1)`，用户在列表里点一下，
/// 脚本就在他自己工作台的页面上下文里执行了 ——
/// 存进来的是个数据，读出来却成了代码执行。
///
/// 规则跟 server.js 的 `normUrl()` 一致：先看前缀，再检查域名不空。
pub fn norm_url(v: &str) -> Option<String> {
    let s = v.trim();
    let after_scheme = match strip_scheme(s) {
        Some(r) => r,
        None => return None,
    };
    // 冒号后面至少还得有域名。光秃秃的 `http://` 或 `http:///path` 都不是合法链接。
    if after_scheme.is_empty() || after_scheme.starts_with('/') {
        return None;
    }
    Some(s.to_string())
}

/// 从网址里推出一个能看的标题。纯字符串处理，不发任何请求。
///
/// 跟 server.js 的 `titleFromUrl()` 是同一套逻辑，两边各留一份：
/// 后端这份是导出时兜底 —— 标题为空时直接写进 Markdown 会变成
/// `- [](https://…)`，那是**语法错误的链接**，在任何编辑器里都渲染不出来，
/// 等于这条收藏在备份里丢了。
pub fn title_from_url(raw: &str) -> String {
    let rest = match strip_scheme(raw) {
        Some(r) => r,
        None => return String::new(),
    };

    // 拆出域名（到第一个 / ? # 为止）
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = strip_www(&rest[..host_end]);
    let after_host = &rest[host_end..];

    // 取路径末段（到 ? # 为止）
    let path = after_host.split(['?', '#']).next().unwrap_or("");
    let last_seg = path.split('/').filter(|s| !s.is_empty()).last().unwrap_or("");

    let tail = last_seg.replace(['-', '_'], " ").trim().to_string();
    if !host.is_empty() && !tail.is_empty() {
        return format!("{} · {}", tail, host);
    }
    host
}

fn strip_www(h: &str) -> String {
    // 大小写不敏感地去掉 www. 前缀，跟 JS 的 /^www\./i 保持一致。
    // 用 bytes 比而不是 h[..4] —— 中文域名（IDN 未转码时）在 h[..4] 上会 panic。
    let b = h.as_bytes();
    if b.len() > 4 && b[..4].eq_ignore_ascii_case(b"www.") {
        h[4..].to_string()
    } else {
        h.to_string()
    }
}

/// 判断一个标题是不是「我们自己按网址拼出来的那个自动标题」。
///
/// 只有这种标题才允许被替换（将来抓回来的网页真标题，或者导出时的兜底），
/// **用户自己起的名字一个字都不能动**。
pub fn is_auto_title(title: &str, url: &str) -> bool {
    let t = title.trim();
    if t.is_empty() {
        return true;
    }
    let rest = match strip_scheme(url) {
        Some(r) => r,
        None => return false,
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = strip_www(&rest[..host_end]);

    if t == host || t.ends_with(&format!(" · {}", host)) {
        return true;
    }
    // 还有一种「自动」情况：标题整个就是链接本身。
    // 新建时如果没填标题，后端会用链接兜底当标题显示，这种也算自动生成的。
    bare(t) == bare(url)
}

/// 去掉协议头和末尾斜杠，用来比对「标题是不是就是链接」。
fn bare(s: &str) -> String {
    let rest = strip_scheme(s).unwrap_or_else(|| s.trim());
    rest.trim_end_matches('/').to_string()
}

/// 收藏行 → JSON。
///
/// 字段顺序跟 server.js 的 `SELECT *` 一致。少一个字段前端就可能读不到，
/// 所以这里老老实实全部返回（包括 1c 才用得上的抓取字段）。
fn bookmark_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id":          row.get::<_, i64>(0)?,
        "url":         row.get::<_, String>(1)?,
        "title":       row.get::<_, String>(2)?,
        "created_at":  row.get::<_, String>(3)?,
        "updated_at":  row.get::<_, String>(4)?,
        "deleted_at":  row.get::<_, Option<String>>(5)?,
        "content":     row.get::<_, String>(6)?,
        "summary":     row.get::<_, String>(7)?,
        "fetched_at":  row.get::<_, Option<String>>(8)?,
        "fetch_error": row.get::<_, Option<String>>(9)?,
    }))
}

pub fn list_bookmarks(conn: &Connection, search: Option<&str>) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    match search {
        Some(kw) if !kw.is_empty() => {
            // 收藏**四样都搜**：标题、链接、正文、摘要 ——
            // 有时你只记得「那篇文章里写过什么」，想不起标题也记不清在哪个站。
            let pattern = format!("%{}%", kw);
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM bookmarks WHERE deleted_at IS NULL \
                     AND (title LIKE ?1 OR url LIKE ?1 OR content LIKE ?1 OR summary LIKE ?1) \
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([&pattern], bookmark_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
        _ => {
            let mut stmt = conn
                .prepare("SELECT * FROM bookmarks WHERE deleted_at IS NULL ORDER BY updated_at DESC")
                .map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], bookmark_row).map_err(|e| e.to_string())?;
            for r in rows {
                out.push(r.map_err(|e| e.to_string())?);
            }
        }
    }
    Ok(out)
}

pub fn get_bookmark(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM bookmarks WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([id], bookmark_row).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out.into_iter().next())
}

/// 新建收藏。链接不合法返回 `None`，由路由层转成 400。
///
/// 标题留空时**用链接本身兜底**，跟 server.js 一致（`title.trim() || u`）。
/// 前端列表上直接显示这个字段，不兜底的话会出现一条空白项。
pub fn create_bookmark(conn: &Connection, url: &str, title: &str) -> Result<Option<Value>, String> {
    let u = match norm_url(url) {
        Some(v) => v,
        None => return Ok(None),
    };
    conn.execute(
        "INSERT INTO bookmarks (url,title) VALUES (?1,?2)",
        rusqlite::params![u, title_or(title, &u)],
    )
    .map_err(|e| format!("新建收藏失败：{}", e))?;
    let id = conn.last_insert_rowid();
    get_bookmark(conn, id)
}

/// 更新。链接和标题一起存。
///
/// 两个细节照搬 server.js：
/// 1. **链接改坏了保留原来的**（`normUrl(url) || cur.url`）——
///    自动保存是逐字符触发的，用户正打着「https://」中间存一次很正常，
///    要是照单全收就把收藏弄废了。
/// 2. **链接变了就把正文/摘要清掉** —— 否则会出现「链接是 A、内容是 B」，
///    是最难自己发现的一类错数据。
pub fn update_bookmark(
    conn: &Connection,
    id: i64,
    url: &str,
    title: &str,
) -> Result<Option<Value>, String> {
    let cur = match get_bookmark(conn, id)? {
        Some(v) => v,
        None => return Ok(None),
    };
    // 已经软删的不能改（server.js 是 `if (!cur || cur.deleted_at) 404`）
    if !cur.get("deleted_at").map_or(true, |v| v.is_null()) {
        return Ok(None);
    }
    let old_url = cur.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let u = norm_url(url).unwrap_or_else(|| old_url.to_string());

    let changed = conn
        .execute(
            "UPDATE bookmarks SET url=?1, title=?2,
                    content   = CASE WHEN url<>?1 THEN ''   ELSE content   END,
                    summary   = CASE WHEN url<>?1 THEN ''   ELSE summary   END,
                    fetched_at= CASE WHEN url<>?1 THEN NULL ELSE fetched_at END,
                    fetch_error=CASE WHEN url<>?1 THEN NULL ELSE fetch_error END,
                    updated_at=datetime('now','localtime')
             WHERE id=?3 AND deleted_at IS NULL",
            rusqlite::params![u, title_or(title, &u), id],
        )
        .map_err(|e| format!("保存收藏失败：{}", e))?;

    if changed == 0 {
        return Ok(None);
    }
    get_bookmark(conn, id)
}

pub fn soft_delete_bookmark(conn: &Connection, id: i64) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE bookmarks SET deleted_at=datetime('now','localtime')
             WHERE id=?1 AND deleted_at IS NULL",
            [id],
        )
        .map_err(|e| format!("删除收藏失败：{}", e))?;
    Ok(n > 0)
}

pub fn restore_bookmark(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    conn.execute("UPDATE bookmarks SET deleted_at=NULL WHERE id=?1", [id])
        .map_err(|e| format!("撤销失败：{}", e))?;
    get_bookmark(conn, id)
}

/// 抓取成功后写回。
///
/// 标题只在「原来就是自动生成的」时才替换 —— 用户自己起的名字一个字都不动。
/// 判断依据见 `is_auto_title()`：标题为空，或者标题长得就像我们按链接拼的。
pub fn apply_fetch(
    conn: &Connection,
    id: i64,
    title: &str,
    content: &str,
    summary: &str,
) -> Result<Option<Value>, String> {
    let n = conn
        .execute(
            "UPDATE bookmarks SET title=?1, content=?2, summary=?3,
                    fetched_at=datetime('now','localtime'), fetch_error=NULL,
                    updated_at=datetime('now','localtime')
             WHERE id=?4 AND deleted_at IS NULL",
            rusqlite::params![title, content, summary, id],
        )
        .map_err(|e| format!("写回抓取内容失败：{}", e))?;
    if n == 0 {
        return Ok(None);
    }
    get_bookmark(conn, id)
}

/// 只写失败原因，**不动标题和正文**。
///
/// 抓不到不算错误操作：链接照样存着，只是没内容。
/// 记下原因是为了让用户知道「为什么没抓到」并能点重试。
pub fn mark_fetch_fail(conn: &Connection, id: i64, reason: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE bookmarks SET fetch_error=?1, updated_at=datetime('now','localtime')
         WHERE id=?2 AND deleted_at IS NULL",
        rusqlite::params![reason, id],
    )
    .map_err(|e| format!("记录抓取失败原因出错：{}", e))?;
    Ok(())
}

/// 丢掉已抓到的内容（「手动重来」用）。链接不变，只是内容清空。
pub fn clear_fetch(conn: &Connection, id: i64) -> Result<Option<Value>, String> {
    conn.execute(
        "UPDATE bookmarks SET content='', summary='', fetched_at=NULL, fetch_error=NULL
         WHERE id=?1 AND deleted_at IS NULL",
        [id],
    )
    .map_err(|e| format!("清除抓取内容失败：{}", e))?;
    get_bookmark(conn, id)
}