// 一次性脚本：给 public/index.html 加「检查更新」。
//
// 和上次加「使用指引」同一套路：
//   · 精确匹配 + 断言唯一命中（匹配到 0 处或 2 处都直接抛错，不猜）
//   · 比对前把 needle 刷成 CRLF —— 这个脚本是 LF 存的，index.html 是 CRLF
//   · 写盘前把整篇刷成 CRLF，不留裸 LF
//   · 不写 BOM
const fs = require('fs');
const path = 'E:/个人工作台/public/index.html';

let s = fs.readFileSync(path, 'utf8');
const crlf = (str) => str.replace(/\r?\n/g, '\r\n');

function mustReplace(needle, replacement, expected) {
  const n2 = crlf(needle);
  const n = s.split(n2).length - 1;
  if (n !== expected) throw new Error(`匹配到 ${n} 处（预期 ${expected} 处）：\n${n2.slice(0, 70)}`);
  s = s.split(n2).join(replacement);
}

// ---------- 1. 顶栏按钮样式：并进已有的头部按钮那几组 ----------
mustReplace('#guide-btn, #import-btn, #export-btn, #ai-btn, #ask-btn {',
            '#guide-btn, #update-btn, #import-btn, #export-btn, #ai-btn, #ask-btn {', 1);
mustReplace('#guide-btn:hover, #import-btn:hover, #export-btn:hover, #ai-btn:hover, #ask-btn:hover',
            '#guide-btn:hover, #update-btn:hover, #import-btn:hover, #export-btn:hover, #ai-btn:hover, #ask-btn:hover', 1);
mustReplace('#guide-btn svg, #export-btn svg, #ai-btn svg, #ask-btn svg',
            '#guide-btn svg, #update-btn svg, #export-btn svg, #ai-btn svg, #ask-btn svg', 1);
mustReplace('#ask-btn, #ai-btn, #guide-btn, #import-btn, #export-btn {',
            '#ask-btn, #ai-btn, #guide-btn, #update-btn, #import-btn, #export-btn {', 1);

// ---------- 2. 顶栏按钮本体 ----------
// 放最后（导出备份之后）：它是最不常用的功能之一，
// 不该排在「使用指引」这种「不知道能干什么时点的」按钮前面。
const GUIDE_BTN_END = `        使用指引
      </button>`;
mustReplace(GUIDE_BTN_END, `${GUIDE_BTN_END}
      <!-- 检查更新。放最后：不常点，但一定要有入口，
           否则用户只能靠「有没有新版本」这种运气来决定要不要换。 -->
      <button id="update-btn" title="看看有没有新版本">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
             stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/>
        </svg>
        检查更新
      </button>`, 1);

// ---------- 3. 更新弹窗 ----------
// 复用 .ai-backdrop / .ai-dialog（同一个文件里已经有三套弹窗了，
// 再写一套样式，以后必然走样）。
const GUIDE_DIALOG_END = `<!-- v3：AI 设置弹窗。默认不渲染内容，打开时才填（避免每次开页面都去请求一次配置） -->`;
mustReplace(GUIDE_DIALOG_END, `<!-- 检查更新弹窗。打开就去查一次，用户不用再点「检查」 -->
<div id="update-backdrop" class="ai-backdrop hidden">
  <div class="ai-dialog" role="dialog" aria-modal="true" aria-labelledby="update-dialog-title">
    <h2 id="update-dialog-title">检查更新</h2>
    <p class="ai-sub">从发布页看有没有新版本。有的话下载安装，没有的话什么都不用做。</p>

    <div id="update-body"><p class="upd-line">正在检查…</p></div>

    <div class="ai-acts">
      <button class="btn" id="update-close">关闭</button>
      <button class="btn-primary" id="update-install">下载并安装</button>
    </div>
  </div>
</div>

${GUIDE_DIALOG_END}`, 1);

// ---------- 4. Esc：更新弹窗也要能被 Esc 关掉 ----------
const ESC_HANDLER = `document.addEventListener('keydown', (e) => {
  // 引导在最上层，先关它。
  // **顺手补上 stopPropagation()**：以前按 Esc 关掉 AI 弹窗之后，
  // 事件还会继续往下走，把正文编辑区也一起关了（连按两下 Esc 等于「关弹窗 + 退出一层」，
  // 跟提示里写的「Esc 逐层退出」对不上）。这里一并修掉。
  if (e.key === 'Escape' && !$('guide-backdrop').classList.contains('hidden')) {
    closeGuide();
    e.stopPropagation();
    return;
  }
  if (e.key === 'Escape' && !$('ai-backdrop').classList.contains('hidden')) {
    closeAiDialog();
    e.stopPropagation();
  }
}, true);   // capture：要在页面自己那个 Esc 处理之前跑`;
mustReplace(ESC_HANDLER, `document.addEventListener('keydown', (e) => {
  // 从上往下：引导 > 更新 > AI 弹窗。
  // 每关掉一层就 stopPropagation()，不让它继续传到「Esc 逐层退出」那个处理里
  // —— 否则关弹窗会连带把正文编辑区也关掉。
  if (e.key === 'Escape' && !$('guide-backdrop').classList.contains('hidden')) {
    closeGuide(); e.stopPropagation(); return;
  }
  if (e.key === 'Escape' && !$('update-backdrop').classList.contains('hidden')) {
    closeUpdateDialog(); e.stopPropagation(); return;
  }
  if (e.key === 'Escape' && !$('ai-backdrop').classList.contains('hidden')) {
    closeAiDialog(); e.stopPropagation();
  }
}, true);   // capture：要在页面自己那个 Esc 处理之前跑`, 1);

// ---------- 5. 逻辑 ----------
const GUIDE_START = '// ============ 首次启动引导 ============';
mustReplace(GUIDE_START, `// ============ 检查更新 ============
//
// 两个接口的分工（后端那边是 Rust 的 update.rs）：
//   GET  /api/update/check    查有没有新版本
//   POST /api/update/install  下载、装上、然后重启
//
// **两个后端都要有这两个路径**，哪怕 Node 版只会回答「不支持」：
// 界面是共用的，缺一边就会在那边一点就报「接口出错」。
//
// 后端一律用 200 + ok:false + kind 表达失败（不是 4xx）——
// 前端 api() 遇非 2xx 会抛异常，用户只能看到「接口出错」，
// 精心写的那句人话提示他根本看不到。这条规矩在 AI 那块就吃过一次亏。

function closeUpdateDialog() {
  $('update-backdrop').classList.add('hidden');
}
function updateShow(html) {
  $('update-body').innerHTML = html;
}
function updateFail(d) {
  // 不同原因给不同说法。特别是 kind === 'signature'：
  // 签名对不上意味着有人在伪造更新包，**必须原话说出来，不能糊过去**。
  if (d.kind === 'unsupported') {
    updateShow('<p class="upd-line">' + escHtml(d.message || '这个版本不支持自动更新') + '</p>');
  } else if (d.kind === 'signature') {
    updateShow('<p class="upd-line upd-bad"><b>更新包签名不对，没有安装。</b><br>' +
      escHtml(d.message || '') + '这通常意味着下载到的不是我们发布的版本，为了安全已停止。</p>');
  } else {
    updateShow('<p class="upd-line upd-bad">' + escHtml(d.message || '检查失败') + '</p>');
  }
  $('update-install').classList.add('hidden');
}

async function openUpdateDialog() {
  $('update-backdrop').classList.remove('hidden');
  $('update-install').classList.add('hidden');
  updateShow('<p class="upd-line">正在检查…</p>');
  let d;
  try {
    d = await api('/api/update/check');
  } catch (e) {
    updateShow('<p class="upd-line upd-bad">连不上工作台服务：' + escHtml(e.message) + '</p>');
    return;
  }
  if (!d || d.ok !== true) return updateFail(d || {});

  if (!d.available) {
    updateShow('<p class="upd-line">已经是最新版本（' + escHtml(d.current || '') + '）。</p>');
    return;
  }
  const notes = (d.notes || '').trim();
  updateShow(
    '<p class="upd-line"><b>有新版本：' + escHtml(d.version) + '</b>' +
    (d.date ? '<span class="upd-date">' + escHtml(d.date) + '</span>' : '') +
    '（当前 ' + escHtml(d.current || '') + '）</p>' +
    (notes ? '<pre class="upd-notes">' + escHtml(notes) + '</pre>' : '') +
    '<p class="upd-line upd-mute">安装包约 2MB，下载完会自动重启一次。数据不会受影响。</p>');
  $('update-install').classList.remove('hidden');
}

$('update-btn').addEventListener('click', openUpdateDialog);
$('update-close').addEventListener('click', closeUpdateDialog);
$('update-backdrop').addEventListener('mousedown', (e) => {
  if (e.target === $('update-backdrop')) closeUpdateDialog();
});
$('update-install').addEventListener('click', async () => {
  const btn = $('update-install');
  btn.disabled = true;
  btn.textContent = '正在下载…';
  updateShow('<p class="upd-line">正在下载并安装，大概几秒钟。装完会自动重启。</p>');
  let d;
  try {
    d = await api('/api/update/install', { method: 'POST' });
  } catch (e) {
    btn.disabled = false;
    btn.textContent = '下载并安装';
    updateShow('<p class="upd-line upd-bad">安装失败：' + escHtml(e.message) + '</p>');
    return;
  }
  // 成功的话进程已经被换掉了，走不到这里。
  btn.disabled = false;
  btn.textContent = '下载并安装';
  updateFail(d || {});
});

${GUIDE_START}`, 1);

// ---------- 6. 更新弹窗用到的样式 ----------
// 锚点要够长：单写 '.guide-grid {' 会命中两处（主规则 + 窄屏覆盖那一条），
// 断言直接抛错就是在这儿救了一次。带上下两行内容就唯一了。
mustReplace('  .guide-grid {\n    display: grid; grid-template-columns: 1fr 1fr; gap: 10px;', `  .upd-line { margin: 0; font-size: 13px; line-height: 1.7; color: var(--text-sub); }
  .upd-line + .upd-line { margin-top: 8px; }
  .upd-bad { color: var(--danger); }
  .upd-mute { color: var(--text-mute); font-size: 12px; }
  .upd-date { color: var(--text-mute); font-size: 12px; margin-left: 8px; }
  .upd-notes {
    margin: 10px 0 0; padding: 10px 12px; max-height: 160px; overflow-y: auto;
    background: #f7f8fa; border: 1px solid var(--border-soft); border-radius: var(--radius);
    font-family: ui-monospace, Consolas, monospace; font-size: 12px; line-height: 1.6;
    white-space: pre-wrap; word-break: break-word; color: var(--text-sub);
  }
  .guide-grid {
    display: grid; grid-template-columns: 1fr 1fr; gap: 10px;`, 1);

// ---------- 7. 行尾统一 + 校验 ----------
s = s.replace(/\r?\n/g, '\r\n');
fs.writeFileSync(path, s, 'utf8');

const buf = fs.readFileSync(path);
const text = buf.toString('utf8');
const bareLf = (text.match(/(?<!\r)\n/g) || []).length;
console.log(`字节 ${buf.length}；CRLF ${(text.match(/\r\n/g) || []).length} 行，裸 LF ${bareLf} 行，BOM ${buf[0] === 0xEF}`);
console.log('update-btn', (text.match(/id="update-btn"/g) || []).length,
            '| update-backdrop', (text.match(/id="update-backdrop"/g) || []).length,
            '| openUpdateDialog', (text.match(/function openUpdateDialog/g) || []).length);
if (bareLf !== 0) throw new Error('出现了裸 LF');
if (buf[0] === 0xEF) throw new Error('出现了 BOM');