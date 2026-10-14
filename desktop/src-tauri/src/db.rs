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
    let ok = s.len() == 10
        && s.as_bytes().get(4) == Some(&b'-')
        && s.as_bytes().get(7) == Some(&b'-')
        && s[..4].bytes().all(|c| c.is_ascii_digit())
        && s[5..7].bytes().all(|c| c.is_ascii_digit())
        && s[8..10].bytes().all(|c| c.is_ascii_digit());
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

    Ok(out)
}

/// `2026-10-15` → `10月15日`。
///
/// 导出是给人读的，写 `2026-10-15截止` 也认得出来，但中文格式短得多，
/// 一行放得下。年份就不带了 —— 备份里的东西基本都是今年的。
fn format_due_cn(d: &str) -> String {
    if d.len() != 10 {
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