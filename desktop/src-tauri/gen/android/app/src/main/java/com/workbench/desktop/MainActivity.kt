package com.workbench.desktop

import android.os.Bundle

// ⚠️ 故意**不调** `enableEdgeToEdge()`。Tauri 的模板里有这一句，我们把它删了。
//
// 为什么：真机实测，手机上顶栏（笔记/任务/收藏 + 搜索框）被系统状态栏压住了 ——
// 时间、信号、电量那一行直接盖在我们第一排字上面。
//
// 原因是 `enableEdgeToEdge()` 的作用就是把窗口铺到状态栏底下，
// 让内容一直延伸到屏幕最上沿。这在「应用自己画背景」的场景是对的，
// 但我们这个应用是「一张网页」，顶部还有一行真实内容要读，
// 铺上去就等于让它躲到状态栏后面去了。
//
// 去掉之后，系统会正常把 WebView 往状态栏下方缩，内容一格都不会被压。
//
// 为什么不在 CSS 里用 `env(safe-area-inset-top)` 绕：
// 那条路在 Android WebView 上能不能取到值并不确定（依赖显示切口支持），
// 而这一行是确定的 —— 系统自己算、自己贴，跨 Android 版本都稳。
//
// 这个文件是 `cargo tauri android init` 从模板生成的，平时 `build` 不会覆盖它
// （模板只在 init 时写一次）。但**万一以后重跑了 init，这里会被还原成带
// enableEdgeToEdge 的版本**，症状就是「手机上顶栏又被状态栏压住了」。
// 碰到那种情况，把这段注释连同那行代码一起补回来就行。
class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
  }
}
