// 个人工作台 · 桌面版 · 程序入口
//
// 这个文件故意只有这么点内容 —— 真正的逻辑都在 lib.rs 里，
// 原因写在 lib.rs 顶部（简单说：将来加移动端时，Tauri 要从这里进）。
//
// 第一行的 `#![cfg_attr(...)]` 留在 main.rs 而不是搬到 lib.rs：
// 它是**给链接器看的**属性，标在最终的可执行入口上才生效，
// 搬到 lib 里会报 warning（甚至在某些配置下不生效）。

// 发布版不弹黑色命令行窗口。
//
// 现在双击「启动.bat」会弹一个黑窗口，窗口关掉服务就停。
// 封装成软件之后不该再有这个东西 —— 它的全部价值就是让你知道服务在跑，
// 而桌面应用有窗口、任务栏图标，用户本来就知道自己开着它。
//
// `windows_subsystem = "windows"` 的意思是「这是个 GUI 程序，不要控制台」。
// 但**只在发布版生效**（not(debug_assertions)）：
// 开发时我们还想在终端里看 println! 的日志和 panic 报错，全关掉就没法调试了。
// 这个条件很容易写错 —— 去掉 debug_assertions 那半边，
// 开发时会遇到「改了代码但什么都没发生」，因为报错信息全被吞了。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    workbench_lib::run()
}
