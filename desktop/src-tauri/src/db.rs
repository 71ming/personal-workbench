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
    conn.execute("INSERT INTO notes (title,body) VALUES (?1,?2)", [title, body])
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
            rusqlite::params![title, body, id],
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
    Ok(out)
}

