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

pub mod db;
pub mod http_server;

use std::path::PathBuf;
use tauri::Manager;

/// 启动整个应用。
///
/// 启动顺序（不能颠倒）：
///
///   1. 先找前端文件在哪          —— 找不到就早点报错，别等窗口白屏了才猜
///   2. 决定数据存哪              —— 桌面应用不能再用「程序旁边的 data/」，
///                                   那是绿色安装的前提；必须放系统应用数据目录
///   3. 起本地 HTTP 服务          —— 端口固定 8787，原因见 http_server.rs 顶部
///   4. 最后才让窗口打开这个地址   —— 顺序反了窗口会先加载一个死地址然后白屏
///
/// #[cfg_attr(mobile, tauri::mobile_entry_point)] 这个属性现在不起作用（不是移动端），
/// 但它标出了「移动端入口就是这一个函数」。将来加移动端时，
// 这一行会自动生效，不用改任何逻辑。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // ---- 1. 前端目录 ----
            let public_dir =
                find_public_dir(app.handle()).map_err(|e| format!("找不到界面文件：{}", e))?;

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
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("建数据目录失败：{}", e))?;
            let db_path = data_dir.join("workbench.db");

            // ---- 3. 起 HTTP 服务 ----
            let port = http_server::serve(db_path, public_dir)?;

            // ---- 4. 窗口指向真实地址 ----
            //
            // tauri.conf.json 里没配 url（配了会被当成默认地址先加载一次，
            // 然后我们再跳走，白白多一次失败请求）。
            // 这里在运行时直接把窗口导航到实际端口。
            let url = format!("http://127.0.0.1:{}", port);
            if let Some(window) = app.get_webview_window("main") {
                let parsed = url.parse().map_err(|_| "地址格式不对".to_string())?;
                window
                    .navigate(parsed)
                    .map_err(|e| format!("打开窗口失败：{}", e))?;
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("工作台启动失败");
}

/// 找到 public/index.html 在哪。
///
/// —— 这里有个「开发便利」和「发布正确性」的冲突，踩过才知道要分 ——
///
/// 第一个版本是无条件按「先 resource 再源码树」找的。实测拷到临时目录单独跑，
/// 服务起来了、接口也通，看起来没问题 —— 因为**这台机器上源码树还在**。
/// 但 `CARGO_MANIFEST_DIR` 是**编译期常量**，路径已经写死在 exe 里了：
/// 拷到别人电脑上，那条路径根本不存在，界面就是白屏，
/// 而且因为程序不崩、日志也看不到，很难判断是哪里错了。
///
/// 所以改成按构建类型区分：
///
///   debug 构建 → 认源码树。
///     改 index.html 刷新窗口就生效，不用重新编译 Rust。开发体验值这个。
///
///   release 构建 → **只认资源目录**，绝不回退到构建机路径。
///     因为那条路径在发布对象电脑上必然是失效的，
///     回退只会把「明确找不到」变成「莫名其妙白屏」。
///     真找不到就报一句人话，让用户知道是安装不完整。
///
/// 打包后 Tauri 会把 frontendDist 的内容放进安装包的 resources 目录，
/// `resource_dir()` 指的就是那儿 —— 这是发布环境下的正确来源。
fn find_public_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Ok(res) = app.path().resource_dir() {
        let candidate = res.join("public");
        if candidate.join("index.html").exists() {
            return Ok(candidate);
        }
    }

    // 只有开发构建才回退到源码树。
    if cfg!(debug_assertions) {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("public");
        if dev.join("index.html").exists() {
            return Ok(dev);
        }
    }

    Err(
        "找不到界面文件 public/index.html。\n\
         重新安装一次应该就好 —— 这通常说明安装包不完整，或者被杀毒软件删掉了一部分。"
            .to_string(),
    )
}
