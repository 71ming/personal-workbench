// ============================================================================
// 导入：把另一个 workbench.db 里的笔记 / 任务 / 收藏合并进当前库
// ============================================================================
//
// —— 为什么导入的是「数据库文件」本身，而不是某种导出格式 ——
//
// 桌面上有两份数据：Node 版（E:\个人工作台\data\workbench.db）和
// 桌面版（%APPDATA%\com.workbench.desktop\workbench.db）。当初刻意不让它们
// 共用一个文件（装进 Program Files 没写权限、卸载会删数据），代价就是
// 「搬不过去」。这个模块就是补这个代价的。
//
// 选「直接吃 .db 文件」而不是「先导出成 JSON/Markdown 再导入」的原因：
//
//   1. 两边表结构**逐字段完全一致**（见 db.rs 的 SCHEMA 和 server.js 的建表语句），
//      所以搬数据是纯复制，不用做字段映射，也就不会「搬过来少了几个字段」。
//   2. 现有的 /api/export 只导出笔记的 Markdown，任务和收藏根本没导出去 ——
//      走那条路等于永远搬不全。
//   3. 备份就是「那个文件本身」。用户不用理解两种格式。
//
// —— 合并而不是覆盖 ——
//
// 导入只加东西，从不删东西、不改已有内容。所以：
//   * 导入错文件 → 顶多多几条垃圾笔记，删掉就行，不会毁掉已有数据。
//   * 同一个文件导入两次 → 第二次全部跳过（靠下面的去重规则）。
//   * 桌面版已有的测试数据、原数据 → 一条都不动。
//
// —— 去重规则（每种类型一条，认「内容一样就算同一个」）——
//
//   笔记：标题 + 正文 都一样   → 跳过
//   任务：标题 + 状态 都一样   → 跳过
//   收藏：链接一样             → 跳过
//
// 去重**只看没被软删除的**。已删除的行不参与比较，否则会出现
// 「我明明删过它，怎么又冒出来了」的错觉。
//
// —— 软删除的行不导入 ——
//
// 源文件里 deleted_at 不为空的行直接跳过。它们在界面里根本看不见，
// 搬过来也只是躺在库里占地方；真正要紧的是别让它们干扰去重（见上）。
//
// ============================================================================

use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// SQLite 文件的开头 16 字节。所有 .db 文件都有，别的文件没有。
/// 先看这个再动手，能挡掉「选了个 txt / 图片 / 压缩包」这类情况。
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// 三张表。必须三张都在，少一张就说明这不是工作台的数据文件。
const TABLES: [&str; 3] = ["notes", "tasks", "bookmarks"];

// 各表往目标库写哪些列。
//
// 刻意**不写 id**：id 是自增的，写死会跟现有的行撞号。
// 让数据库自己发新号，导入一百次也不会撞。
const NOTE_COLS: [&str; 4] = ["title", "body", "created_at", "updated_at"];
const TASK_COLS: [&str; 6] = ["title", "status", "due_date", "completed_at", "created_at", "updated_at"];
const BM_COLS: [&str; 8] = [
    "url", "title", "created_at", "updated_at",
    "content", "summary", "fetched_at", "fetch_error",
];

/// 导入的对外入口。`bytes` 是浏览器传上来的那个 .db 文件的原始字节。
pub fn run(target_db: &Path, bytes: &[u8]) -> Result<Value, String> {
    if bytes.is_empty() {
        return Err("这个文件是空的。".to_string());
    }
    if !bytes.starts_with(SQLITE_MAGIC) {
        // 说清楚「选错文件了」，而不是甩一个 SQLite 的英文报错
        // （用户看到 "file is not a database" 只会以为软件坏了）。
        return Err("这不是工作台的数据文件（选到的是别的文件）。".to_string());
    }

    // 先落一个临时文件再打开。
    //
    // 为什么不用 ATTACH 或者直接把字节喂给 SQLite：
    // 浏览器给的是内存里的一段字节，SQLite 只认文件。
    // 而落到临时文件还顺手解决了「源文件正被 Node 版开着」的问题 ——
    // 我们读的是副本，源文件那边开不开、加不加锁，一点都不影响。
    let tmp = temp_path(target_db);
    std::fs::write(&tmp, bytes).map_err(|e| format!("存临时文件失败：{}", e))?;

    // 无论成功失败，临时文件都必须删掉。
    // 留着的话，用户每导一次就多一个几十 KB 的垃圾文件在数据目录里堆着。
    let result = merge(target_db, &tmp);
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 临时文件名带上进程号：本机同时发两个导入也不会互相覆盖。
fn temp_path(target_db: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut name = target_db.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".import-{}-{}.tmp", std::process::id(), stamp));
    target_db.with_file_name(name)
}

/// 真正干活的地方：读源库 → 查重 → 写进目标库（全程一个事务）。
fn merge(target_db: &Path, src_path: &Path) -> Result<Value, String> {
    let src = Connection::open(src_path).map_err(|e| format!("读不出这个文件：{}", e))?;

    // ---- 先验三张表都在 ----
    //
    // 这一步挡的是「随便一个 sqlite 文件」：随手新建的空库、
    // 别的软件的库、BrowserHistory 这种，都会在这一步被拒。
    // 没有这一步的话，下面的 SELECT 会报一句英文的 "no such table"，
    // 用户完全看不懂。
    for t in TABLES {
        let found: Option<i64> = src
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [t],
                |r| r.get(0),
            )
            .ok();
        if found.is_none() {
            return Err(format!("这不是工作台的数据文件（里面没有 {} 表）。", t));
        }
    }

    // 源库三张表的实际列名。用来兼容「很老的库还没加某些列」的情况。
    let src_note_cols = columns(&src, "notes")?;
    let src_task_cols = columns(&src, "tasks")?;
    let src_bm_cols = columns(&src, "bookmarks")?;

    let notes = read_rows(&src, "notes", &src_note_cols)?;
    let tasks = read_rows(&src, "tasks", &src_task_cols)?;
    let bms = read_rows(&src, "bookmarks", &src_bm_cols)?;

    let mut conn = Connection::open(target_db).map_err(|e| format!("打开本机数据库失败：{}", e))?;
    let tx = conn.transaction().map_err(|e| format!("开事务失败：{}", e))?;

    // ---- 全程一个事务 ----
    //
    // 中途失败（比如第 3 条笔记写坏了）就整笔回滚，不会留下「导了一半」的库。
    // 「导了一半」比「没导」糟糕得多：用户分不清哪些是导进来的、
    // 哪些本来就是自己的，重复导入又会因为去重规则而补不回来。
    let mut n_notes = 0usize;
    let mut n_tasks = 0usize;
    let mut n_bms = 0usize;
    let mut skipped = 0usize;

    for row in &notes {
        if exists(
            &tx,
            "SELECT 1 FROM notes WHERE deleted_at IS NULL AND title=?1 AND body=?2 LIMIT 1",
            &[cell(row, &src_note_cols, "title"), cell(row, &src_note_cols, "body")],
        )? {
            skipped += 1;
            continue;
        }
        insert(&tx, "notes", &NOTE_COLS, row, &src_note_cols, &[])?;
        n_notes += 1;
    }

    for row in &tasks {
        let status = match cell(row, &src_task_cols, "status") {
            SqlValue::Text(ref s) if !s.is_empty() => s.clone(),
            // 老库里可能没有 status，缺了就当待办 —— 跟建表默认值一样。
            _ => "todo".to_string(),
        };
        if exists(
            &tx,
            "SELECT 1 FROM tasks WHERE deleted_at IS NULL AND title=?1 AND status=?2 LIMIT 1",
            &[SqlValue::Text(cell_text(row, &src_task_cols, "title")), SqlValue::Text(status.clone())],
        )? {
            skipped += 1;
            continue;
        }
        let mut values = vec![
            SqlValue::Text(cell_text(row, &src_task_cols, "title")),
            SqlValue::Text(status),
            SqlValue::Text(cell_text(row, &src_task_cols, "due_date")),
            SqlValue::Text(cell_text(row, &src_task_cols, "completed_at")),
            SqlValue::Text(cell_text(row, &src_task_cols, "created_at")),
            SqlValue::Text(cell_text(row, &src_task_cols, "updated_at")),
        ];
        // due_date / completed_at 在表里是允许 NULL 的，
        // 空字符串跟 NULL 对这两列不等价，存错会让「有截止日期」的显示跑偏。
        for i in [2usize, 3] {
            if let SqlValue::Text(s) = &values[i] {
                if s.is_empty() {
                    values[i] = SqlValue::Null;
                }
            }
        }
        insert_values(&tx, "tasks", &TASK_COLS, &values)?;
        n_tasks += 1;
    }

    for row in &bms {
        let url = cell_text(row, &src_bm_cols, "url");
        if url.is_empty() {
            skipped += 1;
            continue;
        }
        if exists(
            &tx,
            "SELECT 1 FROM bookmarks WHERE deleted_at IS NULL AND url=?1 LIMIT 1",
            &[SqlValue::Text(url.clone())],
        )? {
            skipped += 1;
            continue;
        }
        insert(&tx, "bookmarks", &BM_COLS, row, &src_bm_cols, &["fetched_at", "fetch_error"])?;
        n_bms += 1;
    }

    tx.commit().map_err(|e| format!("写入失败：{}", e))?;

    Ok(json!({
        "notes": n_notes,
        "tasks": n_tasks,
        "bookmarks": n_bms,
        "skipped": skipped,
    }))
}

/// 表里实际有哪些列。
///
/// 为什么要查：老的 workbench.db 可能还没有 bookmarks.content 这几列
/// （v2.4 才加的）。直接 SELECT 不存在的列会报错，整份导入直接失败；
// 查一下再决定读什么，就能连老库一起导进来。
fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    // table 是上面 TABLES 里写死的三个名字，不含用户输入，拼进 SQL 是安全的。
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|e| format!("读表结构失败：{}", e))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("读表结构失败：{}", e))?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(|e| format!("读表结构失败：{}", e))?);
    }
    Ok(out)
}

/// 读出这张表里所有「没被软删除」的行。
fn read_rows(
    conn: &Connection,
    table: &str,
    cols: &HashSet<String>,
) -> Result<Vec<std::collections::HashMap<String, String>>, String> {
    // 老库没有 deleted_at 的话，就别带这个条件（否则 SQL 直接语法错）。
    let sql = if cols.contains("deleted_at") {
        format!("SELECT * FROM {} WHERE deleted_at IS NULL", table)
    } else {
        format!("SELECT * FROM {}", table)
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读源数据失败：{}", e))?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map([], |r| {
            let mut map = std::collections::HashMap::new();
            for name in &names {
                // 读成字符串：这几张表全是文本，NULL 统一当空串处理，
                // 省掉一整套 Option 分支。
                let v: Option<String> = r.get(name.as_str()).unwrap_or(None);
                map.insert(name.clone(), v.unwrap_or_default());
            }
            Ok(map)
        })
        .map_err(|e| format!("读源数据失败：{}", e))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("读源数据失败：{}", e))?);
    }
    Ok(out)
}

/// 取一列的值：源表没这列就当空串。
fn cell_text(row: &std::collections::HashMap<String, String>, cols: &HashSet<String>, name: &str) -> String {
    if cols.contains(name) {
        row.get(name).cloned().unwrap_or_default()
    } else {
        String::new()
    }
}

/// 同上，但空字符串给 NULL（用在「可以是空」的列上）。
fn cell(row: &std::collections::HashMap<String, String>, cols: &HashSet<String>, name: &str) -> SqlValue {
    let s = cell_text(row, cols, name);
    if s.is_empty() {
        SqlValue::Null
    } else {
        SqlValue::Text(s)
    }
}

/// 查一行在不在。
fn exists(conn: &Connection, sql: &str, args: &[SqlValue]) -> Result<bool, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("查重失败：{}", e))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(args.iter()))
        .map_err(|e| format!("查重失败：{}", e))?;
    Ok(rows.next().map_err(|e| format!("查重失败：{}", e))?.is_some())
}

/// 按目标列名插入一行（源表缺列就补空串）。
///
/// `nullable` 里列出来的，空值存成 NULL 而不是空串。
/// 为什么要分这个：这两列「没抓过」和「抓了个空的」在表里必须是两回事，
/// 界面判断「这条收藏到底抓没抓过」看的就是 NULL。存成空串属于把两件事混成一件，
/// 现在界面上看不出差别，但将来要按抓取历史做点什么的时候就会咬人。
fn insert(
    conn: &Connection,
    table: &str,
    target_cols: &[&str],
    row: &std::collections::HashMap<String, String>,
    src_cols: &HashSet<String>,
    nullable: &[&str],
) -> Result<(), String> {
    let values: Vec<SqlValue> = target_cols
        .iter()
        .map(|c| {
            let v = cell_text(row, src_cols, c);
            if v.is_empty() && nullable.contains(c) {
                SqlValue::Null
            } else {
                SqlValue::Text(v)
            }
        })
        .collect();
    insert_values(conn, table, target_cols, &values)
}

fn insert_values(conn: &Connection, table: &str, cols: &[&str], values: &[SqlValue]) -> Result<(), String> {
    let names = cols.join(", ");
    let holes: Vec<String> = (1..=cols.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table, names, holes.join(", "));
    conn.execute(&sql, rusqlite::params_from_iter(values.iter()))
        .map_err(|e| format!("写入失败：{}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wb-import-test-{}-{}.db", tag, std::process::id()))
    }

    /// 源库和目标库都用它建。
    ///
    /// 不直接跑 db::SCHEMA：SCHEMA 建出来的 notes 表没有 deleted_at
    /// （那列是 migrate() 补的），而本模块的去重 SQL 带着 `deleted_at IS NULL`，
    /// 在裸 SCHEMA 的库上会直接报错。
    ///
    /// 而且任何一份真的 workbench.db 都是 init() 建的 ——
    /// 拿个裸 SCHEMA 的库当源文件，测的是一份从来不会存在的数据库。
    fn make_target(path: &Path) {
        let _ = std::fs::remove_file(path);
        crate::db::init(path).unwrap();
    }

    fn fill(path: &Path, sql: &str, args: &[&str]) {
        let conn = Connection::open(path).unwrap();
        conn.execute(sql, rusqlite::params_from_iter(args.iter())).unwrap();
    }

    fn count(path: &Path, sql: &str) -> i64 {
        Connection::open(path).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn 不是数据库文件会被挡掉() {
        let target = tmp_path("bad");
        make_target(&target);
        let err = run(&target, b"this is not a database at all").unwrap_err();
        assert!(err.contains("不是工作台的数据文件"));
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn 是数据库但没有工作台的表_也要挡掉() {
        // 光是「是 SQLite 文件」还不够：三张表必须都在。
        // 随手新建的空库、别的软件的历史库，都该在这一步被拒，
        // 而不是让用户看到一句英文的 no such table。
        let alien = tmp_path("alien");
        let _ = std::fs::remove_file(&alien);
        Connection::open(&alien)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (x INTEGER)")
            .unwrap();
        let target = tmp_path("alien-target");
        make_target(&target);

        let bytes = std::fs::read(&alien).unwrap();
        let err = run(&target, &bytes).unwrap_err();
        assert!(err.contains("没有 notes 表"));
        let _ = std::fs::remove_file(&alien);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn 导入_去重_跳过软删除() {
        let src = tmp_path("src");
        let target = tmp_path("target");
        make_target(&src);
        make_target(&target);

        fill(&src, "INSERT INTO notes (title,body,created_at) VALUES ('甲','正文一','2020-01-01 00:00:00')", &[]);
        fill(&src, "INSERT INTO notes (title,body,created_at) VALUES ('乙','正文二','2020-01-02 00:00:00')", &[]);
        // 源库里那条已删除的，不该被搬过来
        fill(&src, "INSERT INTO notes (title,body,deleted_at) VALUES ('丙','正文三','2020-01-03 00:00:00')", &[]);
        // 目标库里已经有的同名同正文
        fill(&target, "INSERT INTO notes (title,body) VALUES ('甲','正文一')", &[]);

        let bytes = std::fs::read(&src).unwrap();
        let out = run(&target, &bytes).unwrap();
        assert_eq!(out["notes"], 1, "只该导入「乙」");
        assert_eq!(out["skipped"], 1, "只有「甲」算重复（已删除的丙根本读不出来，不计数）");

        // 再导一次：一条都不许多
        let out2 = run(&target, &bytes).unwrap();
        assert_eq!(out2["notes"], 0);
        assert_eq!(out2["skipped"], 2, "这次甲和乙都成了重复");
        assert_eq!(count(&target, "SELECT COUNT(*) FROM notes WHERE deleted_at IS NULL"), 2);
        assert_eq!(count(&target, "SELECT COUNT(*) FROM notes WHERE title='丙'"), 0, "已删除的不能被搬回来");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn 导入_保留原时间_不给空日期() {
        let src = tmp_path("src2");
        let target = tmp_path("target2");
        make_target(&src);
        make_target(&target);

        fill(&src, "INSERT INTO tasks (title,status,due_date,completed_at,created_at) VALUES ('买菜','done',NULL,'2024-05-06 07:08:09','2019-01-01 00:00:00')", &[]);
        let bytes = std::fs::read(&src).unwrap();
        let out = run(&target, &bytes).unwrap();
        assert_eq!(out["tasks"], 1);

        let conn = Connection::open(&target).unwrap();
        let (created, completed, due): (String, Option<String>, Option<String>) = conn
            .query_row("SELECT created_at, completed_at, due_date FROM tasks", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(created, "2019-01-01 00:00:00", "创建时间要原样搬过来");
        assert_eq!(completed.as_deref(), Some("2024-05-06 07:08:09"), "完成时间也要");
        assert_eq!(due, None, "没设截止日期就该是 NULL，不能是空串");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn 导入_没抓过的收藏_fetched_at_是_null_不是空串() {
        // 「没抓过」和「抓了个空的」在表里必须能区分开。
        // 界面判断「这条到底抓没抓过」看的是 NULL；
        // 存成空串的话现在看不出差别，将来按抓取历史做东西就会咬人。
        let src = tmp_path("src3");
        let target = tmp_path("target3");
        make_target(&src);
        make_target(&target);

        fill(&src, "INSERT INTO bookmarks (url,title) VALUES ('https://never-fetched.example.com/','没抓过')", &[]);
        let bytes = std::fs::read(&src).unwrap();
        assert_eq!(run(&target, &bytes).unwrap()["bookmarks"], 1);

        let conn = Connection::open(&target).unwrap();
        let fetched: Option<String> = conn
            .query_row("SELECT fetched_at FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fetched, None);
        let err: Option<String> = conn
            .query_row("SELECT fetch_error FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(err, None);

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn 导入_老的没有抓取字段的库也能导() {
        // v2.4 之前的 bookmarks 没有 content/summary/fetched_at/fetch_error。
        // 这种老库必须照样能导进去，不能报错。
        let src = tmp_path("old");
        let target = tmp_path("old-target");
        let _ = std::fs::remove_file(&src);
        Connection::open(&src)
            .unwrap()
            .execute_batch(
                "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');
                 CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo', due_date TEXT, completed_at TEXT, created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');
                 CREATE TABLE bookmarks (id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');",
            )
            .unwrap();
        make_target(&target);

        fill(&src, "INSERT INTO bookmarks (url,title) VALUES ('https://old.example.com','老收藏')", &[]);
        fill(&src, "INSERT INTO tasks (title) VALUES ('老任务')", &[]);

        let bytes = std::fs::read(&src).unwrap();
        let out = run(&target, &bytes).unwrap();
        assert_eq!(out["bookmarks"], 1);
        assert_eq!(out["tasks"], 1);

        let conn = Connection::open(&target).unwrap();
        let content: String = conn
            .query_row("SELECT content FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(content, "", "缺的字段补空串");
        let status: String = conn
            .query_row("SELECT status FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "todo", "没有 status 的老库按待办算");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&target);
    }
}