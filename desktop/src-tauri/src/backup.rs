// 个人工作台 · 启动自动备份
//
// ============================================================================
// 为什么需要这个
// ============================================================================
//
// 起因是一次真实的数据丢失：装 0.6.0 的时候安装器先跑了一遍旧版的卸载程序，
// 卸载确认页上「删除应用数据」那个勾被选上了，于是
//     RmDir /r "$APPDATA\com.workbench.desktop"
// 把整个数据目录（含 workbench.db）删了个干净，新版首次启动又建了个空库。
// 3 条笔记当场消失。
//
// **教训不是「别勾那个框」** —— 那要靠人记得，而人会忘。
// 教训是：**数据必须有一份不由安装器管的副本。**
//
// 所以规矩改成：每次启动先把数据库整个复制一份到 backups/，带时间戳，保留最近 10 份。
// 这样无论数据是被卸载器删的、被未来的某个 bug 删的、还是你手滑删错了，
// 都能从最近一次启动时的状态捞回来。
//
// ============================================================================
// 为什么用「复制文件」而不是 SQLite 的 VACUUM INTO
// ============================================================================
//
// VACUUM INTO 能拿到一致性快照，但它是**在另一个连接里**跑的，
// 意味着要先打开数据库 —— 那就晚了一步：万一打开之前文件已经被删了，
// 我们连救的机会都没有。
//
// 复制文件是在**碰数据库之前**做的，没文件就没得救，有文件就一定救得到。
// 安全性上也没问题：这个项目用默认的 rollback journal（不是 WAL），
// 没写事务的时候数据库文件本身就是一致的完整状态。
//
// ============================================================================
// 什么时候跑
// ============================================================================
//
// 启动流程里，**建表之前**。顺序很重要：
//     备份 → 建表 → 起服务
// 第一次运行（文件还不存在）直接跳过，那本来就没有东西可丢。

use std::fs;
use std::path::{Path, PathBuf};

/// 保留最近几份。
///
/// 10 份是拍出来的：如果每天开一次，那是十天的回溯；
/// 多留也没用（超过十份基本不会去翻），少留又不够。
const KEEP: usize = 10;

const BACKUP_DIR: &str = "backups";
const LOG_FILE: &str = "备份.log";

/// 备份文件名。`ts` 是 `db::now_local()` 给的 "2026-10-14 21:45:03"。
///
/// 时间里的空格和冒号换成 - 和空：Windows 文件名里冒号是**非法字符**，
/// 直接用原样会创建失败 —— 而失败会被当成「备份没做」静悄悄跳过。
pub fn backup_name(ts: &str) -> String {
    let safe: String = ts
        .chars()
        .map(|c| if c == ':' || c == ' ' { '-' } else { c })
        .collect();
    format!("workbench-{}.db", safe)
}

/// 从一堆备份里挑出「该删的」。
///
/// 挑法：按文件名排序（文件名里的时间戳是定长且可比较的，等于按时间排），
/// 留下最后 KEEP 份，前面的都是更老的。
///
/// 纯函数，单独测 —— 「留几份」这种事测不动就只能靠肉眼看目录。
pub fn to_delete(mut files: Vec<PathBuf>, keep: usize) -> Vec<PathBuf> {
    files.sort();
    if files.len() <= keep {
        return Vec::new();
    }
    files.truncate(files.len() - keep);
    files
}

/// 只认我们自己生成的备份文件，别的不碰。
fn is_backup(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("workbench-") && n.ends_with(".db"))
        .unwrap_or(false)
}

/// 备份一次。返回备份文件路径；数据库还不存在（比如首次运行）时返回 Ok(None)。
pub fn backup_once(db_path: &Path, ts: &str) -> Result<Option<PathBuf>, String> {
    if !db_path.exists() {
        return Ok(None);
    }

    let dir = db_path
        .parent()
        .ok_or_else(|| "拿不到数据库所在目录".to_string())?
        .join(BACKUP_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败：{}", e))?;

    let dest = dir.join(backup_name(ts));
    fs::copy(db_path, &dest).map_err(|e| format!("复制数据库失败：{}", e))?;

    // 清理老备份。删失败不是致命错误 —— 备份本身已经成了，
    // 只是目录里多几份而已，没必要因此让程序起不来。
    if let Ok(rd) = fs::read_dir(&dir) {
        let olds: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_backup(p) && p != &dest)
            .collect();
        for old in to_delete(olds, KEEP) {
            let _ = fs::remove_file(old);
        }
    }

    Ok(Some(dest))
}

/// 备份 + 写一行日志。
///
/// **备份失败绝不能让程序起不来** —— 数据库还在、用户还能干活，
/// 因为「备份失败」就把程序卡在启动阶段是本末倒置。
/// 所以这里返回 Ok，失败只记一笔日志（写日志本身失败也忽略）。
pub fn run_once(db_path: &Path, ts: &str) {
    let result = backup_once(db_path, ts);
    let line = match &result {
        Ok(Some(p)) => format!("{}  已备份 → {}\n", ts, p.display()),
        Ok(None) => return, // 首次运行，没东西可备，不用记
        Err(e) => format!("{}  备份失败：{}\n", ts, e),
    };
    if let Some(dir) = db_path.parent() {
        write_log(&dir.join(LOG_FILE), &line);
    }
}

/// 写日志，带 UTF-8 BOM。
///
/// 为什么加 BOM：这个日志是给**人**看的（双击用记事本打开，
/// 「上次启动到底备份没备份」是第一个要回答的问题）。
/// 不带 BOM 的话记事本按系统 ANSI（中文 Windows 是 GBK）去读，
/// 满屏「宸插浠」这种乱码 —— 等于没写。
///
/// 跟「启动错误.log」是同一个道理（lib.rs 里也有），也跟启动.bat
/// 必须是 UTF-8 BOM 一脉相承：**给程序读的文件可以不带 BOM，
/// 给 Windows 上最常见那个文本编辑器读的文件必须带。**
fn write_log(path: &Path, text: &str) {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.as_bytes());
    let _ = fs::write(path, &bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文件名里不能有冒号() {
        // Windows 文件名非法字符就是冒号。带冒号的文件名一定创建失败。
        let n = backup_name("2026-10-14 21:45:03");
        assert!(!n.contains(':'), "文件名不能带冒号：{}", n);
        assert_eq!(n, "workbench-2026-10-14-21-45-03.db");
    }

    #[test]
    fn 时间戳能看出先后() {
        // 文件名排序必须等于时间排序，否则「留最近 10 份」会留错。
        let early = backup_name("2026-10-14 09:00:00");
        let late = backup_name("2026-10-14 21:00:00");
        assert!(early < late, "{} 应该排在 {} 前面", early, late);
        let next_day = backup_name("2026-10-15 08:00:00");
        assert!(late < next_day);
    }

    #[test]
    fn 留最新的删最老的() {
        let files: Vec<PathBuf> = ["09:00:00", "10:00:00", "11:00:00", "12:00:00"]
            .iter()
            // 走 backup_name 生成，和真实运行时用的是同一条路径 ——
            // 手拼一遍文件名的话，测的就不是真正被排序的那个字符串了。
            .map(|t| PathBuf::from("/x").join(backup_name(&format!("2026-10-14 {}", t))))
            .collect();
        let del = to_delete(files, 2);
        assert_eq!(del.len(), 2);
        assert!(del[0].to_string_lossy().contains("09-00-00"));
        assert!(del[1].to_string_lossy().contains("10-00-00"));
    }

    #[test]
    fn 不到份数就一个都不删() {
        let files: Vec<PathBuf> = ["09:00:00", "10:00:00"]
            .iter()
            .map(|t| PathBuf::from("/x").join(backup_name(&format!("2026-10-14 {}", t))))
            .collect();
        assert!(to_delete(files, 10).is_empty());
    }
}