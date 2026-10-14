// Tauri 的构建脚本。
// 它负责在编译期生成平台相关的代码（Windows 的资源清单、macOS 的 Info.plist 等）。
// 内容固定是这三行，一般不需要改。
fn main() {
    tauri_build::build()
}
