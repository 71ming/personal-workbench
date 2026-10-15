// 个人工作台 · 桌面版 · 核心逻辑
//
// ============================================================================
// 为什么逻辑在 lib.rs 而不是 main.rs
// ============================================================================
//
// Tauri 的项目模板故意分成两个文件，这不是多此一举：
//
//   main.rs   只有薄薄几行，负责「启动」
//   lib.rs    全部真正���逻辑
//
// 原因在将来的**移动端**。Android/iOS 上根本没有 `main()` 这个概念，
// 系统的入口是 Tauri 自己生成的胶水代码，它会去调 lib 里的 `run()`。
// 如果逻辑全塞在 main.rs，加移动端时就得整段搬过来 ——
// 而且是那种「搬完编译报错、报错信息指向一个你刚删掉的函数」的搬法。
//
// 所以现在就按能扩展的结构写：以后加移动端，只是多一句 `mod` 的事。
//
// ============================================================================

pub mod ai;
pub mod backup;
pub mod db;
pub mod fetch;
pub mod http_server;
pub mod import;
pub mod open;
pub mod update;

use std::io::Write;
use std::path::PathBuf;
use tauri::Manager;

/// 启动整个应用。
///
/// 启动顺序（不能颠倒）：
///
///   1. 先找前端文件在哪          —— 找不到就早点报错，别等窗口白屏了才猜
///   2. 决定数据存哪              —— 桌面应用不能再用「程序旁边的 data/」，
///                                   那是绿色安装的前提；必须放系统应用数据目录
///   3. 备份数据库（开库之前）    —— 详见 backup.rs；装新版被卸载器删过数据，得自己留底
///   4. 起本地 HTTP 服务          —— 端口固定 8787，原因见 http_server.rs 顶部
///   5. 最后才让窗口打开这个地址   —— 顺序反了窗口会先加载一个死地址然后白屏
///
/// #[cfg_attr(mobile, tauri::mobile_entry_point)] 这个属性现在不起作用（不是移动端），
/// 但它标出了「移动端入口就是这一个函数」。将来加移动端时，
// 这一行会自动生效，不用改任何逻辑。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 自动更新插件。注册放在 setup 之前，而且只在桌面端注册：
    // 官方支持矩阵是 Windows / macOS / Linux，Android 和 iOS 不支持
    // （移动端要用应用商店那套机制）。不加这个条件，
    // 将来加移动端目标时会编译失败在一个看不出原因的地方。
    //
    // ⚠️ 这段**不能写成** `.plugin(...)` 直接串进下面那条链：
    // 属性 `#[cfg(desktop)]` 夹在方法链中间，解析器读到 `.` 找不到表达式，
    // 报出来的是「expected expression, found `.`」—— 跟「插件注册写错了」毫无关系，
    // 能让人查半天。所以这里拆成两个 builder 再 let 起来。
    #[cfg(desktop)]
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        // opener：**全平台都要注册**，不是只有桌面。
        // 「打开链接」在手机上没有 explorer.exe / open / xdg-open 可用，
        // 靠的就是这个插件走原生 Intent。见 open.rs 顶部。
        .plugin(tauri_plugin_opener::init());
    #[cfg(not(desktop))]
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init());

    let app = builder
        .setup(|app| {
            // 这里面任何一步失败都要走 report_failure()，**不能只用 `?` 往上抛**。
            //
            // 原因：release 版没有控制台（main.rs 里设了 windows_subsystem），
            // 所以 `?` 抛出的错误信息唯一的去处就是崩溃对话框，
            // 而实际上用户看到的只是「窗口闪一下就没了」——
            // 既不知道发生了什么，也不知道去哪儿看日志。
            //
            // 这是实测踩到的：装完双击，窗口开一下就关，什么提示都没有。
            // 而「没有错误提示」本身就是个 bug：出错了却不让用户知道怎么修，
            // 比出错更糟。
            match bootstrap(app) {
                Ok(()) => Ok(()),
                Err(e) => {
                    report_failure(app, &e);
                    // **在这里就退出，不把错误返回给 Tauri。**
                    //
                    // 实测踩过：本来写的是「报告完再 `Err(e.into())` 返回去，
                    // 然后在 run() 外面优雅退出」。结果退出码还是 -1073740791
                    // （panic）—— 说明 Tauri 收到 setup 的 Err 之后**内部自己又 panic 了**，
                    // 外层那个 `if let Err(e) = result` 压根没机会执行。
                    //
                    // 后果很具体：Windows 弹一个「已停止工作」的红框，
                    // 而那是个**模态对话框** —— 它会盖住 report_failure 刚打开的错误说明。
                    // 用户于是先看到一个红框，合理地推断「这软件崩了」，
                    // 根本不会去看旁边那篇清清楚楚的排查步骤。
                    //
                    // 所以顺序反过来：**先把错误说明摆到用户眼前，再退出**，
                    // 一秒都不要让那个崩溃框有机会抢镜。
                    std::process::exit(1);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Tauri 构建失败");

    // 窗口导航**必须放在 Ready 事件里，不能放在 setup 里**。
    //
    // ⚠️ 这不是风格问题，是真机踩出来的：Android 上放在 setup 里导航**静默无效**。
    //
    // 现象：手机上界面能渲染出来（HTML 是从 APK 里的 tauri:// 协议读的），
    // 但所有 /api/ 请求全挂，提示「连不上工作台服务」。
    // 用 `adb shell cat /proc/net/tcp` 抓过铁证：8787 端口只有一条 LISTEN，
    // **一条连接记录都没有** —— WebView 从头到尾没碰过我们这个服务。
    //
    // 原因链条：
    //   1. wry 在 Android 上调 load_url 是这么写的（src/android/mod.rs）：
    //        MainPipe::send(self.activity_id, WebViewMessage::LoadUrl(...));
    //        Ok(())
    //      **不管消息有没有送到，最后都返回 Ok()** —— 失败被吞掉了。
    //   2. setup 是被 Tauri 在 Ready 事件里调用的（app.rs:1442-1448），
    //      此刻事件循环刚起来、WebView 那条主线程消息通道还没就绪，
    //      导航消息发出去石沉大海，而 navigate() 照样返回成功。
    //
    // Ready 回调是在 setup 跑完之后紧接着触发的，这时窗口和消息通道都就绪了。
    //
    // 桌面端原来在 setup 里导航是能用的（窗口先于 setup 创建、通道也早就绪），
    // 现在统一挪到这里，**一条代码路径覆盖三端**，不用写平台分支。
    app.run(|app, event| {
        if let tauri::RunEvent::Ready = event {
            navigate_to_server(app);
        }
    });
}

/// 把窗口导航到本地 HTTP 服务。
///
/// 地址是 setup 阶段定下来的（`http_server::serve` 返回的实际端口），
/// 通过 managed state 传进来 —— setup 和 Ready 是两次不同的调用，闭包参数不共享。
fn navigate_to_server(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<ServerUrl>() else {
        log_line(app, "Ready 到了但没有服务地址，setup 应该失败了？");
        return;
    };
    let url = state.0.clone();

    let Some(window) = app.get_webview_window("main") else {
        // 桌面端从来没走到过这里。真走到说明 tauri.conf.json 里的窗口改名了，
        // 或者被人删了 —— 属于「配置和代码对不上」，必须喊出来而不是默默不动。
        log_line(app, "找不到名为 main 的窗口，导航跳过");
        return;
    };

    match url.parse::<tauri::Url>() {
        Ok(parsed) => match window.navigate(parsed) {
            Ok(()) => log_line(app, &format!("已请求导航到 {}", url)),
            Err(e) => log_line(app, &format!("导航失败：{}", e)),
        },
        Err(e) => log_line(app, &format!("地址解析失败 {}：{}", url, e)),
    }
}

/// 启动过程中定下来的服务地址。
struct ServerUrl(String);

/// 往启动日志里追加一行。
///
/// 为什么需要它：桌面上出错了会弹错误说明、有控制台，
/// **手机上这些全都没有**（report_failure 里开记事本那段是 `#[cfg(desktop)]`，
/// 见那个函数的注释）。没有日志的话，手机上「起不来」就只是一个白屏，
/// 排查只能靠猜。
///
/// 真出问题时用 `adb pull` 把它拉出来看。
fn log_line(app: &tauri::AppHandle, message: &str) {
    let Some(dir) = app.path().app_data_dir().ok() else {
        return;
    };
    let path = dir.join("启动日志.log");
    let line = format!("[{}] {}\n", chrono_now(), message);
    // **新建文件时带 UTF-8 BOM**，跟 report_failure 里那个错误日志同一个理由：
    // 记事本不认 UTF-8 就按系统 ANSI（中文 Windows 是 GBK）读，满屏乱码。
    // 追加时不能重复写 BOM，只在「文件本来不存在」的那一刻写。
    let is_new = !path.exists();
    // 日志是给人看的，写不进去不该拦住程序跑。
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        if is_new {
            let _ = f.write_all(&[0xEF, 0xBB, 0xBF]);
        }
        let _ = f.write_all(line.as_bytes());
    }
}

/// 真正的启动流程。抽出来是为了让「每一步出错都能被接住并报告」。
fn bootstrap(app: &tauri::App) -> Result<(), String> {
    // ---- 1. 前端目录 ----
    // 找不到不算错：会用编译进 exe 的那一份（见 http_server.rs 的 EMBEDDED_INDEX）。
    let public_dir = find_public_dir(app.handle())?;

    // ---- 2. 数据目录 ----
    //
    // 这里跟 server.js 有个**必须不一样**的地方：
    //
    // server.js 用 `path.join(__dirname, 'data')` —— 也就是「程序旁边的 data 目录」。
    // 那在「双击 bat 解压即用」的形态下没问题。
    // 但桌面应用会被装进 C:\Program Files\ 之类**没有写权限**的目录，
    // 往那儿建 data/ 会直接失败。所以必须问系统要一个「该放这儿」的位置：
    //
    //   Windows: C:\Users\<你>\AppData\Roaming\com.workbench.desktop\
    //   macOS:   ~/Library/Application Support/com.workbench.desktop/
    //   Linux:   ~/.local/share/com.workbench.desktop/
    //
    // 用户自己找得到、卸载软件也不会被顺手删掉 —— 这正是我们要的。
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("拿不到数据目录：{}", e))?;
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("建数据目录失败：{}", e))?;
    let db_path = data_dir.join("workbench.db");

    // ---- 3. 启动自动备份 ----
    //
    // 位置很要紧：**在打开数据库之前**。
    //
    // 起因是一次真实的数据丢失（装新版时卸载器把整个数据目录删了，
    // 详见 backup.rs 顶部）。既然安装器管不住，那就自己留一份：
    // 每次启动先把 workbench.db 原样复制到 backups/，带时间戳，保留最近 10 份。
    //
    // 取时间戳用的是 now_local_no_db() 而不是 now_local(连接) ——
    // 后者会先把空数据库文件建出来，那样首次运行也会留下一份 0 字节的假备份。
    backup::run_once(&db_path, &db::now_local_no_db());

    // ---- 4. 起 HTTP 服务 ----
    //
    // AppHandle 一起传进去：自动更新要靠它拿 updater（界面没有 Tauri IPC，
    // 只能在 Rust 侧调，详见 update.rs 顶部）。
    let port = http_server::serve(db_path, public_dir, Some(app.handle().clone()))?;

    // ---- 5. 把地址记下来，等窗口就绪了再导航 ----
    //
    // 这里**故意不导航**。原因写在 run() 里那段注释（真机踩出来的），
    // 一句话版：setup 阶段 WebView 的消息通道还没就绪，导航会静默丢失。
    let url = format!("http://127.0.0.1:{}", port);
    log_line(app.handle(), &format!("服务已启动：{}", url));
    app.manage(ServerUrl(url));

    Ok(())
}

/// 启动失败时：**写日志 + 当场打开日志文件**。
///
/// 为什么不用弹窗：`MessageBox` 得引 winapi 或 tauri-plugin-dialog 才做得到，
/// 而这个项目一路的取舍都是「不加不必要的依赖」。
/// 用系统自带的记事本打开日志，零依赖、信息还更全（带时间戳和上下文）。
///
/// 挑在**出错当场**打开，而不是「下次启动再显示」——
/// 用户正等着双击看结果，这时候把原因摆到眼前才有意义。
fn report_failure(app: &tauri::App, message: &str) {
    // 先拿到数据目录才能写日志。拿不到就退到临时目录 —— 总比什么都不留强。
    let log_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("启动错误.log"))
        .unwrap_or_else(|| {
            std::env::temp_dir().join("workbench-启动错误.log")
        });

    let body = format!(
        "个人工作台启动失败\n{}\n\n\
         原因：\n{}\n\n\
         ---\n\
         这个软件的数据在 AppData 目录里，不会因为启动失败丢东西，可以放心重试。\n\
         如果反复出现，请把本文件发给开发者。\n",
        chrono_now(),
        message
    );

    // 写日志时**带 UTF-8 BOM**。
    // 不带的话记事本会按系统 ANSI（中文 Windows 是 GBK）去读，
    // 结果满屏「涓汉宸ヤ綔鍙」这种乱码 —— 错误信息等于白写。
    // 这跟方案.md 里记的「启动.bat 必须是 UTF-8 BOM」是同一个坑的另一个方向：
    // 那边是 cmd 不看 chchcp，这边是记事本不看编码标记。
    let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
    bytes.extend_from_slice(body.as_bytes());
    let _ = std::fs::write(&log_path, &bytes);

    // 桌面端：当场用记事本打开日志。
    //
    // ⚠️ **手机端没有这一段，而且这是必须的**：Android/iOS 沙箱里
    // 「从本程序调另一个程序打开一个文件」这件事根本不成立
    // （iOS 连进程派生都没有）。硬写在这儿的下场是**编译失败**，
    // 不是「手机上打不开日志」那么温和。
    //
    // 手机上用户在 app 内看到的是一个白屏，什么提示都收不到 ——
    // 这个短板要靠真机测试时盯紧点，别假设它跟桌面一样。
    #[cfg(desktop)]
    {
        // 用 `start` 而不是直接调 notepad.exe：
        // notepad.exe 在较新 Windows 上是个应用别名，直接调可能找不到，
        // `cmd /c start` 走 shell 的文件关联，稳定得多。
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", "", log_path.to_str().unwrap_or("")])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&log_path).spawn();
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            let _ = std::process::Command::new("xdg-open").arg(&log_path).spawn();
        }
    }
}

/// 一个不依赖 chrono 的时间戳。
/// 引入 chrono 就为了打一行日志不值当 —— 这个项目连 SQLite 都是 bundled 编进去的。
fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("时间戳(Unix): {}", secs)
}

/// 找到 public/index.html 在哪。**找不到不算致命。**
///
/// 返回 `Ok(None)` 的意思是「磁盘上没有，用编译进 exe 的那一份」。
/// 真正的修法在 http_server.rs 的 `EMBEDDED_INDEX` —— 那才是「双击闪退」的解药。
///
/// 为什么还要留这个函数、还要去磁盘上找：
/// 开发时磁盘上有 public/，改完 index.html 刷新窗口就生效，不用重编译 Rust。
/// 这点很值这几行代码。
///
/// ⚠️ 这里记一个踩过的坑：第一版写了个 `WORKBENCH_STRICT_RESOURCES` 诊断开关，
/// 本意是「设了就别回退到源码树，用来模拟发布环境」，
/// 结果判断写反了 —— 变量叫 strict，条件却是「允许回退」。
/// 于是设了 =1 反而回退，诊断显示「一切正常」，我差点据此写下
/// 「资源查找没问题」的错误结论。
///
/// **判断反了的标志：一开诊断开关，故障就消失了。**
/// 以后写这类开关，先确认「开关关掉时行为和原来一致」。
fn find_public_dir(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    let allow_source_fallback = cfg!(debug_assertions)
        && std::env::var("WORKBENCH_STRICT_RESOURCES").as_deref() != Ok("1");
    if allow_source_fallback {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("public");
        if dev.join("index.html").exists() {
            return Ok(Some(dev));
        }
    }

    // 装好的版本里，资源在 exe 旁边的 resources 目录下。
    if let Ok(res) = app.path().resource_dir() {
        let candidate = res.join("public");
        if candidate.join("index.html").exists() {
            return Ok(Some(candidate));
        }
    }

    // 磁盘上都没有 —— 不是错误，交给内嵌的那份。
    Ok(None)
}
