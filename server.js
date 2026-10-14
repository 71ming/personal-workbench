// 个人工作台 · 服务端
// 零依赖：只用 Node 内置 http + node:sqlite
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const PORT = 8787;
const ROOT = __dirname;
const DB_PATH = path.join(ROOT, 'data', 'workbench.db');

// ---------- 启动自动备份 ----------
//
// 和桌面版 backup.rs 是同一件事、同一个规矩：**每次启动先把 workbench.db
// 整份复制到 data/backups/，带时间戳，保留最近 10 份。**
//
// 为什么两个后端都得有：界面是共用的 public/index.html，
// 备份目录的位置虽然各存各的，但「启动前留一份底」这个行为不能只有一个有 ——
//
//   否则就出现很别扭的情况：Node 版从没被安装器碰过（它压根不在 Program Files 里），
//   偏偏是「安全」的那个忘了备份，而真正有风险的那个记得备份。
//   规矩得是一套，不然下一个人照着 Node 版改，会以为备份已经有了。
//
// 起因是一次真实的数据丢失：装桌面新版时旧版卸载器把整个数据目录删了。
// 教训不是「卸载时别勾删除数据」——那要靠人记得。教训是数据得有一份
// 不由安装器管的副本。完整说明见 desktop/src-tauri/src/backup.rs 顶部。
//
// 放在打开数据库**之前**：先开库会把空文件建出来，
// 那样首次运行也会留下一份 0 字节的假备份。
function backupOnceOnStartup() {
  const KEEP = 10;                    // 留几份，和桌面版一致
  const BACKUP_DIR = path.join(path.dirname(DB_PATH), 'backups');

  // 库还不存在 = 首次运行，本来就没有东西可丢，跳过。
  if (!fs.existsSync(DB_PATH)) return;

  // 本地时间，格式跟库里其它时间一样：YYYY-MM-DD HH:MM:SS
  const p = (n) => String(n).padStart(2, '0');
  const d = new Date();
  const ts = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
             `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
  // 文件名里的空格和冒号换成 - 和空：Windows 文件名中冒号是非法字符，
  // 直接用会创建失败 —— 而失败会被当成「备份没做」静悄悄跳过。
  const name = `workbench-${ts.replace(/[: ]/g, '-')}.db`;

  try {
    fs.mkdirSync(BACKUP_DIR, { recursive: true });
    fs.copyFileSync(DB_PATH, path.join(BACKUP_DIR, name));

    // 清理老备份：文件名里的时间戳定长可比，排序就等于按时间排序。
    // 删失败不致命 —— 备份本身已经成了。
    const olds = fs.readdirSync(BACKUP_DIR)
      .filter(n => n.startsWith('workbench-') && n.endsWith('.db') && n !== name)
      .sort();
    for (const old of olds.slice(0, Math.max(0, olds.length - KEEP))) {
      try { fs.unlinkSync(path.join(BACKUP_DIR, old)); } catch { /* 算它已经不在了 */ }
    }
    writeLog(`${ts}  已备份 → ${path.join(BACKUP_DIR, name)}\n`);
  } catch (e) {
    // 备份失败绝不能让程序起不来：数据库还在、用户还能干活。
    // 因为「备份失败」就把程序卡在启动阶段是本末倒置。
    try { writeLog(`${ts}  备份失败：${e.message}\n`); }
    catch { /* 日志都写不了，那就只能算了 */ }
  }
}

// 写备份日志，**开头带 UTF-8 BOM**。
//
// 这个文件是给**人**看的：双击用记事本打开，「上次启动到底备份没备份」
// 是第一个要回答的问题。不带 BOM 的话记事本按系统 ANSI（中文 Windows 是 GBK）
// 去读，满屏乱码，等于没写。桌面版 backup.rs 里是同一个理由，
// 跟「启动.bat 必须 UTF-8 BOM」「启动错误.log 必须 BOM」也是同一个坑。
//
// BOM 写成 '\uFEFF' 而不是把真正的 BOM 字符打进源码 —— 后者在编辑器里
// 是看不见的，复制粘贴会丢，存盘时还可能被顺手清掉。实测踩过这种「隐形字符」。
function writeLog(text) {
  fs.writeFileSync(path.join(path.dirname(DB_PATH), '备份.log'), '\uFEFF' + text);
}
backupOnceOnStartup();

// ---------- 数据库 ----------
fs.mkdirSync(path.dirname(DB_PATH), { recursive: true });
const db = new DatabaseSync(DB_PATH);
db.exec(`
  CREATE TABLE IF NOT EXISTS notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
  );
`);

// 迁移：v1.1 加了软删除字段。SQLite 的 CREATE TABLE IF NOT EXISTS 不会改已有表，
// 所以老库要单独补一列。这里先查表结构再决定要不要加，跑多少次都安全。
const cols = db.prepare('PRAGMA table_info(notes)').all().map(c => c.name);
if (!cols.includes('deleted_at')) {
  db.exec("ALTER TABLE notes ADD COLUMN deleted_at TEXT");
  console.log('已升级数据库：新增 deleted_at 字段（软删除）');
}

// ============ v2 新增：任务（独立的表） ============
// 为什么单独建表而不往 notes 里加字段：
// 一张表一个概念。任务是「要做的事」，笔记是「写下来的东西」，
// 字段、排序方式、生命周期都不一样。等 v3 要做关联时，混在一张表里就会乱。
// （这条是方案.md 里明确写的设计约定）
db.exec(`
  CREATE TABLE IF NOT EXISTS tasks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    title        TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT 'todo',          -- 'todo' 待办 / 'done' 已完成
    due_date     TEXT,                                 -- 截止日期 'YYYY-MM-DD'，空表示不设
    completed_at TEXT,                                 -- 完成时间，取消完成就清空
    created_at   TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    deleted_at   TEXT                                  -- 软删除，和笔记一个套路
  );
`);

// 任务也一样做软删除：任务往往比笔记更误删（手滑就点掉了），留着后悔药
const tq = {
  list: db.prepare(`
    SELECT * FROM tasks WHERE deleted_at IS NULL
    ORDER BY (status = 'done') ASC,          -- 未完成排前面
             (due_date IS NULL) ASC,        -- 有截止日期的排前面
             due_date ASC,                   -- 越早到期越靠前，逾期的自然浮到最上面
             created_at ASC`),
  search: db.prepare(`
    SELECT * FROM tasks WHERE deleted_at IS NULL AND title LIKE ?
    ORDER BY (status = 'done') ASC, (due_date IS NULL) ASC, due_date ASC, created_at ASC`),
  get:      db.prepare('SELECT * FROM tasks WHERE id = ?'),
  create:   db.prepare("INSERT INTO tasks (title,due_date) VALUES (?,?)"),
  // completed_at 的处理写进 SQL：完成时保留原完成时间（重复保存不会刷时间），
  // 取消完成就置空。放在 SQL 里做，避免先读后写时序出错。
  update:   db.prepare(`UPDATE tasks SET title=?, status=?, due_date=?,
                        completed_at = CASE WHEN ?='done'
                          THEN COALESCE(completed_at, datetime('now','localtime'))
                          ELSE NULL END,
                        updated_at=datetime('now','localtime')
                        WHERE id=? AND deleted_at IS NULL`),
  softDel:  db.prepare("UPDATE tasks SET deleted_at=datetime('now','localtime') WHERE id=? AND deleted_at IS NULL"),
  restore:  db.prepare('UPDATE tasks SET deleted_at=NULL WHERE id=?'),
  allForExport: db.prepare("SELECT title,status,due_date,completed_at,created_at FROM tasks WHERE deleted_at IS NULL ORDER BY (status='done') ASC, (due_date IS NULL) ASC, due_date ASC"),
};

// ============ 收藏：独立的表 ============
// 继续守「一张表一个概念」。收藏存的是「一个链接 + 我对它的称呼」，
// 不是笔记也不是任务，不和它们混在一起。
db.exec(`
  CREATE TABLE IF NOT EXISTS bookmarks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    url        TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    deleted_at TEXT
  );
`);

// v2.4 迁移：自动抓正文和摘要要存东西。跟 notes 一样，先查表结构再补列，
// 跑多少次都安全，老库第一次启动会自动升级。
const bmCols = db.prepare('PRAGMA table_info(bookmarks)').all().map(c => c.name);
for (const [name, ddl] of [
  ['content',    "ALTER TABLE bookmarks ADD COLUMN content TEXT NOT NULL DEFAULT ''"],   // 抓下来的正文（纯文本）
  ['summary',    "ALTER TABLE bookmarks ADD COLUMN summary TEXT NOT NULL DEFAULT ''"],   // 一两句话的摘要
  ['fetched_at', "ALTER TABLE bookmarks ADD COLUMN fetched_at TEXT"],                     // 上次成功抓取的时间
  ['fetch_error',"ALTER TABLE bookmarks ADD COLUMN fetch_error TEXT"],                    // 上次抓失败的原因
]) {
  if (!bmCols.includes(name)) {
    db.exec(ddl);
    console.log('已升级数据库：bookmarks 新增 ' + name + ' 字段');
  }
}

// 列表里带上 summary（列表上要显示一行）和 content（编辑区要显示正文）。
// 正文最多 20KB，几十条收藏也就几百 KB，本地服务扛得住，换来的是前端一次请求就能显示全部。
const BM_FIELDS = 'id,url,title,summary,content,fetched_at,fetch_error,created_at,updated_at';

const bmq = {
  list:   db.prepare(`SELECT ${BM_FIELDS} FROM bookmarks WHERE deleted_at IS NULL ORDER BY updated_at DESC`),
  // 标题、链接、正文、摘要都能搜到：
  // 有时你只记得「那篇文章里写过什么」，想不起标题也记不清在哪个站。
  search: db.prepare(`SELECT ${BM_FIELDS} FROM bookmarks WHERE deleted_at IS NULL
                      AND (title LIKE ? OR url LIKE ? OR content LIKE ? OR summary LIKE ?)
                      ORDER BY updated_at DESC`),
  get:     db.prepare('SELECT * FROM bookmarks WHERE id = ?'),
  create:  db.prepare('INSERT INTO bookmarks (url,title) VALUES (?,?)'),
  // 改链接等于换了内容，旧的正文/摘要立刻作废，否则会出现「链接是 A、内容是 B」。
  update:  db.prepare(`UPDATE bookmarks SET url=?, title=?,
                         content=CASE WHEN url<>? THEN '' ELSE content END,
                         summary=CASE WHEN url<>? THEN '' ELSE summary END,
                         fetched_at=CASE WHEN url<>? THEN NULL ELSE fetched_at END,
                         fetch_error=CASE WHEN url<>? THEN NULL ELSE fetch_error END,
                         updated_at=datetime('now','localtime')
                       WHERE id=? AND deleted_at IS NULL`),
  // 抓取成功后写回：标题只在「原来就是自动生成的」时才替换，用户自己起的名字不许动。
  applyFetch: db.prepare(`UPDATE bookmarks SET title=?, content=?, summary=?,
                          fetched_at=datetime('now','localtime'), fetch_error=NULL,
                          updated_at=datetime('now','localtime')
                        WHERE id=? AND deleted_at IS NULL`),
  // 只写失败原因，不动标题和正文
  markFetchFail: db.prepare("UPDATE bookmarks SET fetch_error=?, updated_at=datetime('now','localtime') WHERE id=? AND deleted_at IS NULL"),
  clearFetch: db.prepare("UPDATE bookmarks SET content='', summary='', fetched_at=NULL, fetch_error=NULL WHERE id=? AND deleted_at IS NULL"),
  softDel: db.prepare("UPDATE bookmarks SET deleted_at=datetime('now','localtime') WHERE id=? AND deleted_at IS NULL"),
  restore: db.prepare('UPDATE bookmarks SET deleted_at=NULL WHERE id=?'),
  // 导出要多带两个字段：抓取状态决定这份备份能不能告诉你「哪些网页其实没存下来内容」。
  // 只在失败时输出提示行，所以绝大多数条目（抓取成功的）不会因此变啰嗦。
  allForExport: db.prepare('SELECT url,title,summary,fetched_at,fetch_error,created_at FROM bookmarks WHERE deleted_at IS NULL ORDER BY created_at ASC'),
};

// 软删除：不真删，只打一个删除时间戳，撤销就是把时间戳清空。
// 这样误删之后还能捞回来 —— 这是个人笔记库最需要的一个保险。
const q = {
  list:     db.prepare('SELECT id,title,body,updated_at FROM notes WHERE deleted_at IS NULL ORDER BY updated_at DESC'),
  search:   db.prepare('SELECT id,title,body,updated_at FROM notes WHERE deleted_at IS NULL AND (title LIKE ? OR body LIKE ?) ORDER BY updated_at DESC'),
  get:      db.prepare('SELECT id,title,body,created_at,updated_at,deleted_at FROM notes WHERE id = ?'),
  create:   db.prepare('INSERT INTO notes (title,body) VALUES (?,?)'),
  update:   db.prepare('UPDATE notes SET title=?, body=?, updated_at=datetime(\'now\',\'localtime\') WHERE id=? AND deleted_at IS NULL'),
  softDel:  db.prepare("UPDATE notes SET deleted_at=datetime('now','localtime') WHERE id=? AND deleted_at IS NULL"),
  restore:  db.prepare('UPDATE notes SET deleted_at=NULL WHERE id=?'),
  allForExport: db.prepare('SELECT title,body,created_at,updated_at FROM notes WHERE deleted_at IS NULL ORDER BY created_at ASC'),
};

// ---------- 工具 ----------
// 截止日期归一化：只认 YYYY-MM-DD，其余一律当成「不设截止」。
// 前端 <input type="date"> 交上来就是这个格式，但接口是公开的，别直接拼进 SQL。
function normDue(v) {
  const s = String(v ?? '').trim();
  return /^\d{4}-\d{2}-\d{2}$/.test(s) ? s : null;
}

// 链接归一化：**只放行 http / https**，其它一律拒绝（返回 null）。
// 这不是洁癖：前端会把收藏的链接渲染成可点的 <a>。
// 如果放行 javascript:alert(1)，用户在列表里点一下，
// 脚本就在他自己工作台的页面上下文里执行了 —— 存进来的是个数据，读出来却成了代码执行。
function normUrl(v) {
  const s = String(v ?? '').trim();
  if (!/^https?:\/\//i.test(s)) return null;
  try { new URL(s); } catch { return null; }   // 再让内置解析器验一次格式
  return s;
}

// 从网址里推出一个能看的标题，纯字符串处理，不发任何请求。
// 跟前端 titleFromUrl() 是同一套逻辑，两边各留一份的原因和 normUrl/safeLink 一样：
// 前端这个是为了输入框里实时提示，后端这个是为了导出时兜底 ——
// 标题为空时直接写进 Markdown 会变成 `- [](https://…)`，那是**语法错误的链接**，
// 在任何 Markdown 编辑器里都渲染不出来，等于这条收藏在备份里丢了。
function titleFromUrl(raw) {
  let u;
  try { u = new URL(String(raw ?? '').trim()); } catch { return ''; }
  const host = u.hostname.replace(/^www\./, '');
  // 解码路径末段（维基百科那种 %E7%91%9E... 能解出中文），
  // 但残缺的百分号序列会让 decodeURIComponent 直接抛异常，必须单独接住。
  const lastSeg = u.pathname.split('/').filter(Boolean).pop() || '';
  let seg = lastSeg;
  try { seg = decodeURIComponent(lastSeg); } catch { /* 解不了就用原文，不比空强 */ }
  const tail = seg.replace(/[-_]+/g, ' ').trim();
  if (host && tail) return `${tail} · ${host}`;
  return host || '';
}

// ============ v2.4：抓网页内容 ============
// 零依赖，全部用 Node 自带的 fetch + 手写字符串处理。
// 这一块和「存两条数据」完全不是一回事，所以单独一节。
//
// —— 安全：为什么必须拦内网地址 ——
// 这个服务会替你去访问用户给的任意网址。如果没有这道拦截，
// 粘一条 http://127.0.0.1:8787/api/notes 就能让服务把自己数据库里的笔记读出来；
// 粘 http://169.254.169.254/ 还能碰到云主机的元数据接口。
// 单机小工具也一样要拦，不然「贴个链接」就等于开了一条内网通道。
function isPrivateHost(hostname) {
  const h = String(hostname).toLowerCase().replace(/^\[|\]$/g, '');   // 去 IPv6 的方括号
  if (h === 'localhost' || h.endsWith('.localhost')) return true;
  if (h === '::1' || h.startsWith('fc') || h.startsWith('fd') || h.startsWith('fe80')) return true;  // IPv6 内网
  // IPv4：按四段数字判断，别用字符串前缀（10.0.0.1 是内网，100.64.0.1 不是）
  const m = h.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!m) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  if (a === 127) return true;                 // 回环
  if (a === 10) return true;                  // 10.0.0.0/8
  if (a === 172 && b >= 16 && b <= 31) return true;   // 172.16.0.0/12
  if (a === 192 && b === 168) return true;    // 192.168.0.0/16
  if (a === 169 && b === 254) return true;    // 链路本地（含云元数据 169.254.169.254）
  if (a === 0) return true;                   // 0.0.0.0/8「本机」
  return false;
}

// HTTP 实体反转义。网页里的 &amp; &nbsp; &#39; 这些不解出来，正文会脏。
const ENTITIES = {
  amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ', ensp: ' ', emsp: ' ',
  mdash: '—', ndash: '–', hellip: '…', ldquo: '“', rdquo: '”', lsquo: '‘', rsquo: '’', middot: '·',
};
function decodeEntities(s) {
  return s.replace(/&(#x?[0-9a-f]+|[a-z]+);/gi, (m, g) => {
    if (g[0] === '#') {
      const n = g[1] === 'x' || g[1] === 'X' ? parseInt(g.slice(2), 16) : parseInt(g.slice(1), 10);
      // 转成码点再转成字符；非法码点直接丢掉，免得打出乱码控制符
      return Number.isFinite(n) && n > 0 && n <= 0x10ffff ? String.fromCodePoint(n) : '';
    }
    const v = ENTITIES[g.toLowerCase()];
    return v === undefined ? m : v;
  });
}

// 有些页面要登录才看得到内容。我们这边不带 cookie（也不该带，见下），
// 抓回来的其实是登录框。这种结果必须**当成失败**处理：
// 不然标题会被改成「登录 XXX」，正文变成一堆登录按钮，比不抓还糟。
//
// 判断条件写保守点，只在「页面很短」时才下结论 ——
// 一篇专门讲登录/密码的文章也会命中关键词，但它正文很长，不会被误判。
//
// 为什么不去存 cookie：那就等于在你的数据库里存网站登录凭据，
// 泄露风险和「记密码」是一个级别的事。真要支持「登录过的页面」，
// 应该做成独立功能单独评估，不该顺手塞进收藏里。
function looksLikeLoginWall(html, content, title) {
  if (content.length > 1200) return false;
  const hasPasswordBox = /<input[^>]+type=["']?password/i.test(html);
  const kw = /(登录|请先登录|需要登录|尚未登录|sign\s?in|log\s?in)/i;
  return hasPasswordBox || (kw.test(title) && kw.test(content));
}

// 判断一个标题是不是「我们自己按网址拼出来的那个自动标题」。
// 只有这种标题才允许被抓回来的网页真标题替换掉 —— 用户自己起的名字一个字都不能动。
function isAutoTitle(title, url) {
  const t = String(title || '').trim();
  if (!t) return true;
  try {
    const u = new URL(url);
    const host = u.hostname.replace(/^www\./, '');
    if (t === host || t.endsWith(' · ' + host)) return true;
    // 还有一种「自动」情况：标题整个就是链接本身。
    // 新建时如果没填标题，后端会用链接兜底当标题显示，
    // 这种也算自动生成的，应该允许被网页真标题替换。
    const bare = (s) => String(s).replace(/^https?:\/\//i, '').replace(/\/+$/, '');
    return bare(t) === bare(u.toString()) || bare(t) === bare(u.hostname + u.pathname);
  } catch { return false; }
}

// 从 HTML 里抠出「标题 / 描述 / 正文」。没有第三方解析器可用，
// 所以走最朴素但够稳的一条路：先扔掉不该看的块，再把剩下的标签换成换行，最后收空白。
const DROP_BLOCKS = 'script|style|noscript|svg|iframe|head|nav|header|footer|aside|form|template|noscript';
function extractFromHtml(html, baseUrl) {
  const text = String(html);

  // 标题：og:title（网站自己声明的，最准）→ <title>
  let title = '';
  const og = text.match(/<meta[^>]+property=["']og:title["'][^>]*content=["']([^"']*)["']/i)
          || text.match(/<meta[^>]+content=["']([^"']*)["'][^>]*property=["']og:title["']/i);
  if (og) title = og[1];
  if (!title) {
    const t = text.match(/<title[^>]*>([\s\S]*?)<\/title>/i);
    if (t) title = t[1];
  }
  title = decodeEntities(title).replace(/\s+/g, ' ').trim().slice(0, 200);

  // 描述：og:description → meta[name=description]
  let summary = '';
  const ogd = text.match(/<meta[^>]+property=["']og:description["'][^>]*content=["']([^"']*)["']/i)
           || text.match(/<meta[^>]+content=["']([^"']*)["'][^>]*property=["']og:description["']/i);
  if (ogd) summary = ogd[1];
  if (!summary) {
    const d = text.match(/<meta[^>]+name=["']description["'][^>]*content=["']([^"']*)["']/i)
           || text.match(/<meta[^>]+content=["']([^"']*)["'][^>]*name=["']description["']/i);
    if (d) summary = d[1];
  }
  summary = decodeEntities(summary).replace(/\s+/g, ' ').trim();

  // 正文
  let body = text
    .replace(/<!--[\s\S]*?-->/g, '')                      // 注释
    .replace(new RegExp(`<(${DROP_BLOCKS})\\b[\\s\\S]*?</\\1>`, 'gi'), '\n')
    .replace(new RegExp(`<(${DROP_BLOCKS})\\b[^>]*/?>`, 'gi'), '')   // 没闭合的空标签
    .replace(/<(br|hr)\s*\/?>/gi, '\n')
    // 这些标签在版面上就是「另起一行」，换成换行符比全塞进一行好读得多
    .replace(/<\/?(p|div|li|tr|h[1-6]|section|article|blockquote|pre|td|dd|dt|figcaption)\b[^>]*>/gi, '\n')
    .replace(/<[^>]+>/g, ' ');                            // 剩下的标签全删
  body = decodeEntities(body);

  const lines = body.split('\n').map(s => s.replace(/\s+/g, ' ').trim())
    .filter(s => s.length > 1)          // 丢掉只剩标点的碎行
    .filter(s => !/^[\s|·•\-–—*_=›»→]+$/.test(s));  // 丢掉纯装饰线
  let content = lines.join('\n');

  if (content.length > 20000) content = content.slice(0, 20000) + '\n…（正文太长，已截断）';

  // 摘要：网页没写 description 就拿正文开头，截在句号/换行处，别拦腰截半句话
  if (!summary && content) {
    const head = content.split('\n')[0].slice(0, 200);
    const cut = Math.max(head.lastIndexOf('。'), head.lastIndexOf('.'), head.lastIndexOf('！'), head.lastIndexOf('？'));
    summary = (cut > 30 ? head.slice(0, cut + 1) : head).trim();
  }
  if (summary.length > 300) summary = summary.slice(0, 300) + '…';

  return { title, summary, content };
}

// 抓一个网址。手动处理重定向（每跳一次都重新验一次是不是内网），
// 限制读取体积，8 秒超时，网络错误和 5xx 重试一次。
const FETCH_TIMEOUT = 8000;
const MAX_BYTES = 2 * 1024 * 1024;   // 2MB：正文用不了这么多，超了直接放弃

async function fetchPage(startUrl) {
  let target = startUrl;
  for (let hop = 0; hop <= 5; hop++) {
    const u = new URL(target);
    if (u.protocol !== 'http:' && u.protocol !== 'https:') throw new Error('只支持 http/https');
    if (isPrivateHost(u.hostname)) throw new Error('不抓内网地址（' + u.hostname + '）');

    const res = await fetch(target, {
      redirect: 'manual',                       // 自己跟，便于每跳都验一次
      signal: AbortSignal.timeout(FETCH_TIMEOUT),
      headers: {
        // 老实报自己是本地小工具，别装成浏览器（很多站会直接拒绝假的 UA）
        'user-agent': 'PersonalWorkbench/1.0 (local bookmark fetcher)',
        accept: 'text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5',
      },
    });

    if (res.status >= 300 && res.status < 400 && res.headers.get('location')) {
      target = new URL(res.headers.get('location'), target).toString();
      continue;                                  // 继续循环，重新做内网检查
    }
    if (!res.ok) {
      const e = new Error('对方返回 ' + res.status + (res.statusText ? ' ' + res.statusText : ''));
      e.retryable = res.status >= 500 || res.status === 429;   // 4xx 是我们自己不该重试
      throw e;
    }

    const ct = (res.headers.get('content-type') || '').toLowerCase();
    if (!/text\/html|text\/plain|application\/xhtml/.test(ct)) {
      throw new Error('不是网页（' + (ct.split(';')[0] || '未知类型') + '）');
    }

    // 边读边累计，超了就掐断 —— 不然一个 200MB 的文件能把内存吃满
    const reader = res.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.length;
        if (size > MAX_BYTES) { await reader.cancel(); throw new Error('网页太大（超过 2MB），已放弃'); }
        chunks.push(value);
      }
    } finally {
      reader.releaseLock?.();
    }

    const buf = Buffer.concat(chunks);
    // 少数老站点还是 GBK/GB2312，按响应头声明的编码解，解不了就当 UTF-8
    const cs = (ct.match(/charset=([\w-]+)/) || [])[1] || 'utf-8';
    let html;
    try { html = new TextDecoder(cs, { fatal: false }).decode(buf); }
    catch { html = new TextDecoder('utf-8', { fatal: false }).decode(buf); }

    return { html, finalUrl: target };
  }
  throw new Error('重定向跳的次数太多');
}

async function fetchBookmarkContent(url) {
  let lastErr;
  // 只重试一次，而且**超时不再重试**：
  // 8 秒都没回应的站点，多等一轮只是让用户对着转圈多生 8 秒的气。
  // 值得重试的是「连接被拒 / DNS 抖动」这类瞬时错误。
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      return await fetchPage(url);
    } catch (e) {
      lastErr = e;
      const timedOut = e?.name === 'TimeoutError' || e?.name === 'AbortError';
      if (e.retryable === false || timedOut) break;
    }
  }
  const msg = lastErr?.name === 'TimeoutError' || lastErr?.name === 'AbortError'
    ? `超时（${FETCH_TIMEOUT / 1000} 秒没响应）`
    : (lastErr?.message || '抓取失败');
  throw new Error(msg);
}

// 读**二进制**请求体（导入的 .db 文件用它）。
//
// 不能用上面的 readBody：那个把 chunk 当字符串拼，
// 遇到 SQLite 文件里的非法 UTF-8 字节会被替换掉（U+FFFD）—— 文件当场损坏，
// 表现是「导入报格式错」，跟真实原因完全对不上。
//
// 计数也要按字节来：字符串的 length 是字符数，中文文件能差出一倍多。
const readBytes = (req) => new Promise((resolve, reject) => {
  const chunks = [];
  let total = 0;
  let over = false;
  req.on('data', c => {
    total += c.length;
    // 超限之后**只标记、不再收，也不要 destroy**。
    //
    // 一开始写的是「超了就 reject + req.destroy()」，结果 400 还没发出去，
    // 连接已经被自己掐了：客户端拿到的是 ECONNRESET，而不是那句
    // 「文件太大了」。实测才暴露出来 —— 接口看着有错误处理，
    // 实际上一句都送不到用户面前。
    if (total > 5e6) { over = true; return; }
    // 兜底：真的不给停（客户端一直发）时，50MB 强制掐断，
    // 不然内存会一直涨。这是本地单人应用，正常永远走不到这一条。
    if (total > 50e6) { req.destroy(); reject(new Error('文件太大了（上限 5 MB）。')); }
    chunks.push(c);
  });
  req.on('end', () => {
    if (over) return reject(new Error('文件太大了（上限 5 MB）。'));
    resolve(Buffer.concat(chunks));
  });
  req.on('error', reject);
});

const send = (res, code, data) => {
  const body = JSON.stringify(data);
  res.writeHead(code, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(body);
};

const readBody = (req) => new Promise((resolve, reject) => {
  let buf = '';
  req.on('data', c => {
    buf += c;
    if (buf.length > 5e6) { reject(new Error('body too large')); req.destroy(); }
  });
  req.on('end', () => {
    try { resolve(buf ? JSON.parse(buf) : {}); }
    catch { reject(new Error('invalid json')); }
  });
  req.on('error', reject);
});

// ============ v3：AI 服务商配置 ============
// 这一步只做「配置存哪、怎么读、怎么写」，不做真正的调用（那是下一步）。
//
// —— 为什么配置放文件而不是放进 SQLite ——
// 1) .gitignore 已经整条忽略 data/，API key 天然不会跟着代码进版本库；
// 2) 这份文件里只有服务商、模型、key 三样东西，跟「笔记 / 任务 / 收藏」这些
//    真正的数据完全不是一回事。塞进数据库就要跟着备份导出走，
//    而 key 绝对不该出现在你可能发给别人看的备份文件里。
//
// —— 为什么 key 只在服务端，永不回传给前端 ——
// 前端是浏览器里的 JavaScript，页面上的一切用户都能在控制台里看到。
// 只要把完整 key 发回页面，它就等于公开了。所以接口只回传
// 「有没有存 key」和一段打码的提示（sk-a****xyz），永远不回传原文。

const AI_CONFIG_PATH = path.join(ROOT, 'data', 'ai-config.json');

// 服务商目录。**这份清单是「查证过的快照」，不是永久正确的。**
// 查证日期：2026-10。厂商改模型名很频繁 —— DeepSeek 就在 2026-09-10 把
// deepseek-v4-flash 换成了 deepseek-flash，2026-07-24 停用了 deepseek-chat。
// 所以模型名做成「建议值」而不是写死的默认值，用户可以填目录里没有的任何名字。
const AI_PROVIDERS = [
  {
    id: 'deepseek', name: 'DeepSeek', protocol: 'openai',
    base_url: 'https://api.deepseek.com',
    key_url: 'https://platform.deepseek.com',
    // 便宜的 flash 档就够「回答我的笔记里写了什么」这种活，不需要 pro
    models: [
      { id: 'deepseek-flash', note: '便宜、够用。2026-09-10 起用这个名字（V4.1）' },
      { id: 'deepseek-v4-pro', note: '更强，也更贵' },
    ],
    retired: 'deepseek-chat / deepseek-reasoner 已于 2026-07-24 停用',
    note: '国内直连不用代理。有峰谷定价，闲时价格减半。',
  },
  {
    id: 'doubao', name: '豆包（火山方舟）', protocol: 'openai',
    base_url: 'https://ark.cn-beijing.volces.com/api/v3',
    key_url: 'https://console.volcengine.com/ark',
    // 豆包的「模型名」其实是方舟控制台里的推理接入点 id（ep- 开头），
    // 每个人不一样，没法预设默认值，只能让用户自己去控制台抄。
    models: [],
    modelHint: '填火山方舟控制台里的「接入点 ID」（ep- 开头）',
    note: '国内最便宜的一档，实测适合「读笔记答问」这种短问答。',
  },
  {
    id: 'qwen', name: '通义千问（阿里云百炼）', protocol: 'openai',
    base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    key_url: 'https://bailian.console.aliyun.com',
    models: [
      { id: 'qwen3.8-flash', note: '便宜，上下文 100 万' },
      { id: 'qwen3.7-plus', note: '均衡' },
      { id: 'qwen3.8-max', note: '最强，也最贵' },
    ],
    note: '新账号通常有免费额度，可以先拿它试。',
  },
  {
    id: 'kimi', name: 'Kimi（月之暗面）', protocol: 'openai',
    base_url: 'https://api.moonshot.cn/v1',
    key_url: 'https://platform.moonshot.cn',
    models: [
      { id: 'kimi-k3', note: '当前主力' },
      { id: 'kimi-k2.6', note: '上一代，长上下文' },
    ],
    note: '长文档处理见长。',
  },
  {
    id: 'glm', name: '智谱 GLM', protocol: 'openai',
    base_url: 'https://open.bigmodel.cn/api/paas/v4',
    key_url: 'https://open.bigmodel.cn',
    models: [
      { id: 'glm-5.2', note: '' },
    ],
    modelHint: '模型名更新较频繁，不确定就填控制台里列出的最新一个',
    note: '有免费额度的新用户活动。',
  },
  {
    id: 'openai', name: 'OpenAI', protocol: 'openai',
    base_url: 'https://api.openai.com/v1',
    key_url: 'https://platform.openai.com',
    models: [
      { id: 'gpt-6-astra', note: '通用' },
      { id: 'gpt-6-sol', note: '偏推理' },
    ],
    retired: 'gpt-4o / o3 / o4-mini 官方标注已不推荐',
    note: '国内直连需要代理，本工作台不内置代理功能（见方案.md）。',
  },
];

// Claude 单独列出来但**不接**：它用的是 Anthropic Messages 协议，
// 请求体结构跟 OpenAI 那套不一样（system 是顶层字段、max_tokens 必填、
// 鉴权头是 x-api-key 不是 Authorization）。留到 v3.1 单独写，
// 宁可少一个选项，也不要一个看起来能选、一调就报错的选项。
const AI_PENDING = [
  { id: 'claude', name: 'Claude', why: '协议不同，需要单独写一套调用（v3.1 再做）' },
];

// 打码：只露头 3 个字符和尾 4 个，中间固定几个点。
// 定这个规矩是因为不少 key 是「sk-」开头，头部露出来够你确认存的是哪一家的，
// 又不至于让人看着像在显示完整 key。
function maskKey(key) {
  const k = String(key || '');
  if (k.length <= 8) return '********';   // 太短的直接全盖
  return k.slice(0, 3) + '****' + k.slice(-4);
}

// 读配置。文件不存在是**正常状态**（还没设置过），不是错误。
function readAiConfig() {
  try {
    const raw = JSON.parse(fs.readFileSync(AI_CONFIG_PATH, 'utf8'));
    return raw && typeof raw === 'object' ? raw : {};
  } catch {
    return {};   // 不存在 / 坏了 / 手改坏了，都当「没设置」，不该让工作台起不来
  }
}

// 写配置。先写临时文件再改名 —— 直接覆盖的话，中途断电会留下半截 JSON，
// 下次启动就配置全没了（本地小工具最常见的翻车方式）。
function writeAiConfig(cfg) {
  fs.mkdirSync(path.dirname(AI_CONFIG_PATH), { recursive: true });
  const tmp = AI_CONFIG_PATH + '.tmp';
  fs.writeFileSync(tmp, JSON.stringify(cfg, null, 2), 'utf8');
  fs.renameSync(tmp, AI_CONFIG_PATH);
  try { fs.chmodSync(AI_CONFIG_PATH, 0o600); } catch { /* Windows 上不支持就算了 */ }
}

// 找一个服务商。找不到返回 null —— 调用方要靠它拒绝掉瞎填的 provider id。
function findProvider(id) {
  return AI_PROVIDERS.find(p => p.id === id) || null;
}

// 给前端看的配置：**没有 key 原文**，只有「有没有」和打码提示。
function aiConfigPublic() {
  const cfg = readAiConfig();
  const p = findProvider(cfg.provider);
  return {
    provider: cfg.provider || '',
    model: cfg.model || '',
    has_key: !!cfg.apiKey,
    key_preview: cfg.apiKey ? maskKey(cfg.apiKey) : '',
    // 下面这个是给「能不能问」用的判断依据，前端拿它决定要不要先引导去设置
    ready: !!(p && cfg.apiKey && cfg.model),
    updated_at: cfg.updatedAt || '',
  };
}

function handleAiGetConfig(res) {
  return send(res, 200, {
    providers: AI_PROVIDERS,
    pending: AI_PENDING,
    config: aiConfigPublic(),
  });
}

async function handleAiPutConfig(req, res) {
  const body = await readBody(req);

  // provider 必须是目录里的一个。不认的 id 直接拒 —— 否则调用器会拿着
  // 一个用户瞎填的 base_url 去发请求，那既是安全问题也是用户自己的问题。
  const provider = String(body.provider ?? '').trim();
  if (provider && !findProvider(provider)) {
    return send(res, 400, { error: '不认识的服务商：' + provider });
  }

  // 模型名不校验「对不对」，只校验「填没填」。理由同上：厂商改名比本程序更新快，
  // 拿一份可能过期的白名单去卡用户，只会让正确的新模型名用不了。
  const model = String(body.model ?? '').trim();
  if (model.length > 120) return send(res, 400, { error: '模型名太长了' });

  // reset 是显式的「全部清空」开关。
  // 为什么需要它：下面两行是 `if (provider)`，空字符串会被当成「没传」跳过，
  // 所以光靠传空串永远清不掉服务商和模型，末尾那段「三项都空就删文件」也就成了走不到的死代码。
  // 用一个显式字段表达「我真的要清空」，不靠猜。
  if (body.reset === true) {
    try { fs.unlinkSync(AI_CONFIG_PATH); } catch { /* 本来就没有，忽略 */ }
    return send(res, 200, { config: aiConfigPublic() });
  }

  const cfg = readAiConfig();

  // key 三种情况，用一个显式字段区分，不靠猜：
  //   不传 api_key    → 保留原来的（前端只在用户真的输入了新 key 时才带这个字段）
  //   传 clear_key    → 清空
  //   传 api_key 文本 → 换新的
  if (body.clear_key === true) {
    delete cfg.apiKey;
  } else if (typeof body.api_key === 'string' && body.api_key.trim() !== '') {
    const k = body.api_key.trim();
    // 非英文字符一律拒绝，而不是存下去。
    //
    // **HTTP 头只能放 ASCII**，一个带中文（或全角括号、中文空格）的密钥
    // 发出去会被逐字节当成别的字符，拿到的是一串看不懂的 401。
    // 而这种密钥几乎总是「复制时混进了看不见的东西」，
    // 用户看到的现象是「明明从官网复制的，怎么就是不对」——
    // 直接告诉他「密钥里有非英文字符」比让他去猜快得多。
    // （Rust 版 import.rs 同理，这条是两个后端必须一致的地方之一。）
    if (/[^\x20-\x7E]/.test(k)) {
      return send(res, 400, { error: '密钥里有非英文字符。多半是复制时混进了全角符号或中文空格，请重新复制一次。' });
    }
    cfg.apiKey = k;
  }

  if (provider) cfg.provider = provider;
  if (model) cfg.model = model;

  // 现在仍然清不空（比如只发了 clear_key），但留着这个判断作为兜底：
  // 万一以后加了别的清空路径，这里保证不会留一份空壳文件在磁盘上。
  if (!cfg.provider && !cfg.model && !cfg.apiKey) {
    try { fs.unlinkSync(AI_CONFIG_PATH); } catch { /* 本来就没有，忽略 */ }
  } else {
    cfg.updatedAt = new Date().toLocaleString('zh-CN');
    writeAiConfig(cfg);
  }

  return send(res, 200, { config: aiConfigPublic() });
}

// ============ v3：列出服务商当前可用的模型 ============
// 为什么要「探测」而不是在目录里写死模型名：
// 厂商改模型名太频繁 —— DeepSeek 2026-09-10 才把 deepseek-v4-flash 换成
// deepseek-flash，2026-07-24 停用了 deepseek-chat。只要目录里的名字落后一步，
// 用户就会撞上「我明明买了这个模型，工作台却不认」。
// 各家都有 GET {base_url}/models（OpenAI 兼容协议的一部分），直接问它最准。
//
// 这条接口**不做聊天**，只列名字：不产生 token 花费，也不用等模型生成。
const AI_PROBE_TIMEOUT = 12000;

// 从返回的模型列表里剔掉明显不是用来对话的。
// 但这些关键词不准确（判断错了就是把能用的模型藏起来了），所以前端
// 同时提供一个「显示全部」开关 —— 宁可多列出来让人自己挑，也别静默吞掉。
const NON_CHAT_HINTS = /embedding|embed|rerank|whisper|tts|speech|moderation|dall-?e|stable-diffusion|(^|[-_/])clip($|[-_/])|ocr|asr|transcribe|image|video|audio/i;

async function handleAiModels(req, res) {
  const body = await readBody(req);

  // base_url 只从目录里取，绝不从请求里读 —— 否则这个接口就成了
  // 「让本机去访问任意地址」的跳板（粘贴抓取功能已经踩过这个坑，见 isPrivateHost）。
  const p = findProvider(String(body.provider ?? '').trim());
  // 原来这里 return send(res, 400, ...)。但前端的 api() 一遇到非 2xx 就抛异常，
  // 用户只能看到一句「接口出错：400」，而下面这句「不认识的服务商」他根本看不到。
  // 这条接口的所有失败都是 200 + ok:false（超时、连不上、401、格式看不懂…都是），
  // 这里单独回 400 属于自己给自己开了个后门。桌面版也是 200。
  if (!p) return send(res, 200, { ok: false, error: '不认识的服务商' });

  // 密钥：优先用这次请求里带来的（用户刚贴上还没保存的），
  // 没有就用已保存的。**这条路径不写盘** —— 探测不等于保存，
  // 用户可能只是试一下，不打算留下密钥。
  const key = String(body.api_key ?? '').trim() || (readAiConfig().apiKey || '');
  if (!key) return send(res, 200, { ok: false, error: '还没有密钥，先填一个再检测' });

  let resp;
  try {
    resp = await fetch(p.base_url.replace(/\/+$/, '') + '/models', {
      signal: AbortSignal.timeout(AI_PROBE_TIMEOUT),
      headers: { 'authorization': `Bearer ${key}`, accept: 'application/json' },
    });
  } catch (e) {
    const timeout = e?.name === 'TimeoutError' || e?.name === 'AbortError';
    // 连不上要说清是「网络/被拦」还是「超时」，不然用户会去查 key 白查半天
    return send(res, 200, {
      ok: false,
      error: timeout
        ? `连 ${p.name} 超时（${AI_PROBE_TIMEOUT / 1000} 秒没响应）`
        : `连不上 ${p.name}：${e.message || '网络错误'}`,
    });
  }

  if (!resp.ok) {
    // 把对方给的错误原文尽量带出来。401/403 单独说，因为那几乎总是 key 错了，
    // 而不是「模型名不对」—— 两件事的排查方向完全不同。
    let detail = '';
    try {
      const j = await resp.json();
      detail = j?.error?.message || j?.message || '';
    } catch { /* 不是 JSON 就算了 */ }
    const hint = (resp.status === 401 || resp.status === 403)
      ? '密钥可能填错了，或者没激活'
      : (resp.status === 404 ? '这家可能不支持列出模型' : '');
    return send(res, 200, { ok: false, error: `${p.name} 返回 ${resp.status}${hint ? '（' + hint + '）' : ''}${detail ? '：' + detail : ''}` });
  }

  let list;
  try {
    const j = await resp.json();
    list = Array.isArray(j?.data) ? j.data : (Array.isArray(j?.models) ? j.models : null);
  } catch { list = null; }

  if (!list) {
    // 返回格式不认识。不报错，但要老实说「我不知道列出来的是什么」，
    // 免得前端显示一个空下拉框，用户以为是「这家没有模型可用」。
    return send(res, 200, { ok: false, error: `${p.name} 返回的格式看不懂，没法自动列出模型`, raw: true });
  }

  const all = list
    .map(m => (typeof m === 'string' ? m : (m?.id || m?.name || '')))
    .filter(Boolean)
    .filter((v, i, a) => a.indexOf(v) === i)      // 去重（有些家会重复列）
    .sort();
  const chat = all.filter(m => !NON_CHAT_HINTS.test(m));

  // 一条都没有的话，宁可把原始列表交出去（total 全量 + filtered 空）
  // 也不要显示「没有可用模型」—— 那会让用户以为自己买错了。
  return send(res, 200, {
    ok: true,
    provider: p.id,
    total: all.length,
    models: chat.length ? chat : all,
    // 这个标记告诉前端「过滤掉了一些，可能是误伤」，好让它显示「显示全部」开关
    filtered: chat.length > 0 ? all.length - chat.length : 0,
    hidden: all.length - (chat.length ? chat.length : all.length),
    all_models: all,
  });
}

// ============ v3：调用模型（OpenAI 兼容协议）===========
// 这一步写的是**一个**调用器，六家共用。原因是这六家都实现了同一套协议：
//   POST {base_url}/chat/completions
//   Authorization: Bearer <key>
//   { model, messages: [{role, content}], max_tokens }
//   返回 { choices:[{message:{content}}], usage:{prompt_tokens, completion_tokens} }
// 厂商切换因此是「改一个配置项」，不是「换一套代码」。这是当初选型时的主要理由。
//
// —— 为什么一律不重试 ——
// 抓网页那次是**幂等**的（GET，重复抓一次不花钱），所以重试无脑做。
// 调模型是**生成类**的：请求已经送到对方服务器，网络在半路断掉时，
// 对方很可能已经算完并计费了。再发一次就是花两次钱拿一次回答。
// 这里只对「连接都没建起来」的情况重试一次（DNS 抖动、连接被拒）——
// 那种情况对方肯定没收到，不会重复计费。
//
// —— 为什么不写流式 ——
// 流式要处理 SSE 分片、重连、以及「断了一半怎么接着显示」。
// 本地小工具一问一答，用不着；先把非流式的正确性做扎实。

const AI_CHAT_TIMEOUT = 90000;   // 90 秒：模型要思考，比列模型慢得多

// max_tokens 必须给，而且要**小**。
// 不给的话，某些模型会一路写下去直到撞上下文上限 —— 那是拿你的钱赌。
// 问笔记不需要长篇大论，2000 字足够塞下好几个问题的答案。
const AI_MAX_TOKENS = 2000;

// 价格表。**这是一份会过期的快照，不要当权威。**
// 这次查证（2026-10）就发现同一个 GLM-4.7-Flash 在不同站点上写着
// $0.07/$0.40 和「免费」两种价，明显有一处已经不准了。
// 所以这里的处理方式是：
//   - 查到了就估算，并标出是哪天查的、提醒以账单为准；
//   - 查不到就说「价格未收录」，**只报 token 数**，绝不编一个数字出来。
// token 数来自服务商返回的 usage，那才是准确的；价格只是折算，错了也不影响判断。
const AI_PRICES = {
  // 元 / 百万 tokens。查证日期：2026-10-14。来源：各家官方定价页。
  'deepseek-flash':   { in: 1.01, out: 2.02, date: '2026-10-14', note: '闲时（低峰期）价格减半' },
  'deepseek-v4-pro':  { in: 3.13, out: 6.26, date: '2026-10-14', note: '闲时价格减半' },
  'qwen3.8-flash':    { in: 0.80, out: 2.70, date: '2026-10-14' },
  'qwen3.7-plus':     { in: 1.20, out: 3.60, date: '2026-10-14' },
  'qwen3.8-max':      { in: 14.0, out: 42.0, date: '2026-10-14' },
  'doubao-seed-2.0-mini': { in: 0.20, out: 2.00, date: '2026-10-14', note: '豆包按接入点计费，此为同档参考价' },
  'kimi-k3':          { in: 4.00, out: 20.0, date: '2026-10-14' },
  'glm-5.3-flash':    { in: 0.65, out: 2.00, date: '2026-10-14' },
  // GLM-4.7-Flash 暂不收录：查到两个互相矛盾的价格（$0.07/$0.40 vs 免费），
  // 与其赌一个，不如让界面只报 token 数。
};

// 估算这次调用花了多少钱（元）。返回 null = 这个模型的价格没收录。
function estimateCost(model, promptTokens, completionTokens) {
  const pr = AI_PRICES[String(model || '').trim().toLowerCase()];
  if (!pr) return null;
  // 除以 100 万是因为价格单位是「每百万 tokens」
  const yuan = (promptTokens / 1e6) * pr.in + (completionTokens / 1e6) * pr.out;
  return {
    yuan: Number(yuan.toFixed(6)),
    price_date: pr.date,
    note: pr.note || '',
  };
}

// 把服务商的 HTTP 错误翻译成人能看懂的话，并分好类。
// 分类的意义：401 是「密钥错了，去设置里改」，模型不对是「重新检测一下」，
// 超时是「网络慢，再试一次」。几种情况的处理完全不同，不能都报一句「失败了」。
//
// 有个坑：**各家对「模型不存在」用的状态码不一样**。
// 智谱返回的是 400 + 中文「模型不存在」，不是 404。
// 所以光看状态码会漏掉这一类，必须也看错误内容 —— 这是第一次实测抓出来的。
function aiClassifyError(status, detail) {
  const d = String(detail || '').trim();
  if (status === 401 || status === 403) {
    return { kind: 'auth', text: '密钥无效或没权限。去「AI」设置里检查一下。' + (d ? '（' + d + '）' : '') };
  }
  // 模型名不对：404 是标准做法，但也有厂商用 400 + 一句中文说明。
  if (status === 404 || /模型不存在|无可用模型|model .{0,20}not (found|exist)|invalid model|unknown model/i.test(d)) {
    return {
      kind: 'model',
      text: '模型名不对。点「检测模型」重新选一个。' + (d ? '（' + d + '）' : ''),
    };
  }
  if (status === 429) {
    return { kind: 'rate', text: '被限流或余额不足，等一会儿再试。' + (d ? '（' + d + '）' : '') };
  }
  if (status >= 500) {
    return { kind: 'server', text: '对方服务出问题了（' + status + '），这是他们那边的问题，可以重试。' };
  }
  return { kind: 'http', text: '对方返回 ' + status + (d ? '：' + d : '') };
}

// 真正发一次请求。
// 返回 { ok:true, content, usage, cost } 或 { ok:false, kind, error }。
// 注意：**key 从配置里读，调用方不传**。就算接口是公开的，也不能让密钥从参数进来 ——
// 那意味着任何能访问这个接口的东西都能用它去调模型。
async function callModel(providerId, model, systemPrompt, userPrompt) {
  const p = findProvider(providerId);
  if (!p) return { ok: false, kind: 'config', error: '不认识的服务商' };

  const cfg = readAiConfig();
  const key = cfg.apiKey;
  if (!key) return { ok: false, kind: 'config', error: '还没保存密钥' };
  if (!model) return { ok: false, kind: 'config', error: '还没填模型名' };

  const body = {
    model,
    messages: [
      { role: 'system', content: systemPrompt },
      { role: 'user', content: userPrompt },
    ],
    max_tokens: AI_MAX_TOKENS,
    stream: false,
  };

  // 请求参数只写一处。下面要重试，同样的东西不该抄两遍 ——
  // 抄两遍就一定会有一遍漏改（这次就漏了 method: 'POST'，
  // Node 的 fetch 默认 GET，带 body 直接被服务端拒绝）。
  const request = () => fetch(p.base_url.replace(/\/+$/, '') + '/chat/completions', {
    method: 'POST',                      // 不写默认是 GET，GET 不能带 body
    signal: AbortSignal.timeout(AI_CHAT_TIMEOUT),
    headers: {
      'authorization': `Bearer ${key}`,
      'content-type': 'application/json',
      accept: 'application/json',
    },
    body: JSON.stringify(body),
  });

  let resp;
  try {
    resp = await request();
  } catch (e) {
    const timedOut = e?.name === 'TimeoutError' || e?.name === 'AbortError';
    if (timedOut) {
      return {
        ok: false, kind: 'timeout',
        error: `等了 ${AI_CHAT_TIMEOUT / 1000} 秒还没回。可能是模型在思考很久，也可能网络断了。`,
      };
    }
    // 连接都没建起来，对方肯定没收到请求，重试一次不花钱
    try {
      resp = await request();
    } catch (e2) {
      return { ok: false, kind: 'network', error: '连不上 ' + p.name + '：' + (e2.message || '网络错误') };
    }
  }

  let json = null;
  let rawText = '';
  try {
    rawText = await resp.text();
    json = JSON.parse(rawText);
  } catch { /* 不是 JSON 就当错误信息处理 */ }

  if (!resp.ok) {
    const detail = json?.error?.message || json?.message || rawText.slice(0, 200);
    const e = aiClassifyError(resp.status, detail);
    return { ok: false, kind: e.kind, error: e.text, status: resp.status };
  }

  const content = json?.choices?.[0]?.message?.content;
  if (typeof content !== 'string' || !content.trim()) {
    // 返回了 200 但没有内容。这通常意味着模型触发了内容过滤，
    // 或者请求里少了 messages。两种都不该假装成功。
    return {
      ok: false, kind: 'empty',
      error: '对方返回了 200 但没有内容' +
        (json?.error?.message ? '（' + json.error.message + '）' : '，可能是内容被过滤了'),
    };
  }

  const usage = {
    prompt_tokens: Number(json?.usage?.prompt_tokens) || 0,
    completion_tokens: Number(json?.usage?.completion_tokens) || 0,
  };
  usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;

  return {
    ok: true,
    content,
    usage,
    cost: estimateCost(model, usage.prompt_tokens, usage.completion_tokens),
    // 对方认的这个模型名（有时它会把它映射到具体版本），
    // 记下来让界面能显示「你填的是 A，其实跑的是 B」
    returned_model: json?.model || model,
    finish_reason: json?.choices?.[0]?.finish_reason || '',
  };
}

// 试一句最短的话，用来验证「密钥 + 模型名 + 网络」这三样通不通。
// 刻意不喂笔记、不带任何用户数据 —— 这个按钮存在的意义只是连通性测试。
const AI_TEST_PROMPT =
  '你好。这是一条连通性测试，你只需要回复四个字：连接正常。不要多说别的。';

async function handleAiTest(req, res) {
  const cfg = readAiConfig();

  // 这里必须用**页面上现在填着的**服务商和模型，不能一律读已保存的配置。
  // 原因：「测试连接」最自然的用法就是「换个模型先试一下」，
  // 而换模型只改了输入框、没点保存。要是这里只读磁盘上的配置，
  // 测出来的永远是上一个模型，回执写「实际跑的模型 glm-5.3-flash」——
  // 人只会以为「11 个模型怎么选都是这一个」，其实是自己填的名字压根没发出去。
  // 没传就回落到已保存的配置，老的调用方式照样能用。
  const body = await readBody(req);
  const provider = String(body.provider ?? '').trim() || cfg.provider;
  const model = String(body.model ?? '').trim() || cfg.model;

  const r = await callModel(provider, model, '你是一个只需要简短回答的助手。', AI_TEST_PROMPT);
  if (!r.ok) return send(res, 200, { ...r, tested_model: model, config: aiConfigPublic() });

  // 判断「填的和实际跑的对不上」时，要拿**发出去的那个**名字比。
  // 拿已保存的比是没用的：既然测试用的就是已保存的配置，
  // 这两项本来就会相等，mismatch 永远不会亮。
  // 这次修好之后，它才对得起注释里说的用途 ——
  // 厂商把小模型名悄悄映射到大模型时给出警告，避免成本算低了。
  const mismatch = r.returned_model && r.returned_model.toLowerCase() !== String(model).toLowerCase();

  // tested_model 一起回传，好让界面能摆出「你填的」和「实际跑的」两个名字。
  return send(res, 200, { ...r, mismatch, tested_model: model, config: aiConfigPublic() });
}

// ============ v3：问 AI（把勾选的笔记交给模型回答）===========
// 这一步要守的四条（每一条都是「用户凭什么敢点这个按钮」）：
//   1. 只发**用户勾选**的笔记，一条没勾就什么都不发；
//   2. 只有主动点「问 AI」才发，保存笔记、打开笔记都不发；
//   3. 要求模型用 [笔记N] 标出处，我们据此生成可点回原笔记的引用；
//   4. 明确告诉模型「笔记里没有的，就说没有」，杜绝它靠通用知识编答案。
//
// 为什么引用要靠模型自己标，而不是我们事后猜：
// 事后比对「答案里的句子像不像某条笔记」是猜的，会出现张冠李戴 —
// 看着有引用，其实引错了地方，那比没有引用更糟（用户会信错的那条）。
// 让模型在生成时就标，标错也是它的责任，而它的责任我们看得见。

const AI_MAX_NOTES = 30;         // 一次最多带多少条笔记
const AI_MAX_NOTE_CHARS = 4000;  // 每条笔记最多带多少字

// 把勾选的笔记拼成一段带编号的正文。
// 编号（[笔记1]）同时用于两件事：给模型引用用，以及后面把引用换回链接用。
function buildNoteContext(notes) {
  const parts = [];
  for (let i = 0; i < notes.length; i++) {
    const n = notes[i];
    let body = String(n.body || '').trim();
    if (body.length > AI_MAX_NOTE_CHARS) {
      body = body.slice(0, AI_MAX_NOTE_CHARS) + '\n…（这条太长了，后面的没给）';
    }
    parts.push(`[笔记${i + 1}] 标题：${String(n.title || '无标题')}\n${body}`);
  }
  return parts.join('\n\n---\n\n');
}

// 提示词。写法上的两个要点：
// - 规则放在最前面、说得极其具体，因为小模型对「请基于以下内容」的遵守度
//   远不如对「你要严格做到以下 4 条」的遵守度；
// - 明确允许它说「没提」，并且说明这是**正确行为而不是失败** ——
//   不然模型会倾向于给一个泛泛的通用答案（这正是我们要防的事）。
function aiSystemPrompt(noteCount) {
  return [
    `你在帮用户查阅他自己的笔记。你只能依据下面提供的 ${noteCount} 条笔记回答。`,
    ``,
    `必须严格做到这 4 点：`,
    `1. 每说完一个来自笔记的内容，就在后面用 [笔记N] 标出它来自哪一条。N 是下面笔记的编号。`,
    `2. 如果 ${noteCount} 条笔记里根本没有相关信息，就直接说「这些笔记里没有提到」，然后停下。`,
    `   绝对不要用你自己的知识来补充，也不要根据常识推测。`,
    `3. 如果笔记里的说法互相矛盾，把矛盾如实列出来，不要替用户判断哪个对。`,
    `4. 用中文回答，尽量简洁，不要复述整个笔记内容，只答问题相关的部分。`,
    ``,
    `特别提醒：说「没有提到」是**正确且被鼓励**的行为。`,
    `编一个听起来合理的答案才是错误。如果你觉得该说的笔记里真没有，就这么答。`,
  ].join('\n');
}

// 从模型回答里把 [笔记3] 这类标记抠出来，换成能点回原笔记的引用。
// 返回 { text, cited: [笔记id...] }。
//
// 注意「引用了但我没发这条」的情况：那说明模型在编。
// 这种情况**保留原样并记下来**，但不生成链接 —— 显示层会提示「模型引用了不存在的笔记」。
function parseCitations(answer, notes) {
  const used = new Set();
  const badNums = [];
  // 捕获组**只圈数字**：[笔记1] → num = '1'。
  // （第一版写成 \[(笔记\s*\d+)\]，把「笔记」两个字也圈进去了，
  //  于是 Number('笔记1') = NaN，notes[NaN-1] 是 undefined，
  //  每一条引用都被误判成「模型引用了不存在的笔记」。）
  const text = String(answer).replace(/\[笔记\s*(\d+)\]/g, (m, num) => {
    const idx = Number(num);
    const note = notes[idx - 1];
    if (!note) {
      // 编号越界 = 模型在编一条我没发过的笔记。
      // 之前这里是「原样保留」，结果页面上 [笔记4] 就是一段混在句子里的普通文字，
      // 扫一眼根本看不出来，只能靠回答底下那条汇总警告 —— 但警告说「第几处」，
      // 用户还得自己回去数。这里改成 \u0003 占位，让前端**在正文原位**把它标红。
      badNums.push(idx);
      return `\u0003${idx}\u0003`;
    }
    used.add(idx);
    // 换成内部标记，前端再渲染成可点回原笔记的按钮。
    //
    // 这里必须用 \u0002 控制符包住编号，**不能**写成 CITE1 这种看得见的字样：
    // 模型完全可能自己吐出 "CITE1"（笔记正文里正好写着内部约定时尤其容易）。
    // 前端若是靠 /CITE\d+/ 认标记，这种原文会被当成真引用；
    // 而编号对不上引用表，那个词就被渲染成空 —— 回答里凭空少一块，用户还以为模型漏答了。
    // \u0002 是控制字符，模型生成不出来，永远撞不了车。
    return `\u0002${idx}\u0002`;
  });
  // badNums 是模型编出来的编号（可能有重复，去重后返回）；
  // 前端警告文案要靠它说清「你只勾了几条」，而不是干巴巴一句「引用了 N 处」。
  return { text, cited: [...used], bad: badNums.length, bad_nums: [...new Set(badNums)] };
}

// 供前端调用的问答接口。
async function handleAiAsk(req, res) {
  const { question = '', note_ids = [] } = await readBody(req);
  // 局部变量别叫 q：文件里 q 是笔记的查询对象（q.get / q.list）。
  // 这里原本写成 `const q = String(question).trim()`，把它整个遮住了，
  // 下面 q.get.get(id) 直接抛 "Cannot read properties of undefined"。
  const questionText = String(question).trim();

  if (!questionText) return send(res, 400, { error: '问题不能空' });
  if (questionText.length > 500) return send(res, 400, { error: '问题太长了' });
  if (!Array.isArray(note_ids) || !note_ids.length) {
    // 这条要专门强调：**一条笔记都不勾，是不发送任何东西**
    return send(res, 400, { error: '一条笔记都没勾。不会勾就不该发出去 —— 请先勾选想让它看的笔记。' });
  }

  const ids = note_ids.map(Number).filter(Number.isInteger).slice(0, AI_MAX_NOTES);

  // 只读勾选的那些。q.get 是按 id 取，且这里已经过滤过 deleted_at。
  const notes = [];
  for (const id of ids) {
    const n = q.get.get(id);
    if (n && !n.deleted_at) notes.push({ id: n.id, title: n.title, body: n.body });
  }
  if (!notes.length) return send(res, 400, { error: '勾选的笔记都找不到了' });

  const t0 = Date.now();
  const r = await callModel(
    readAiConfig().provider,
    readAiConfig().model,
    aiSystemPrompt(notes.length),
    `以下是我的笔记：\n\n${buildNoteContext(notes)}\n\n---\n\n我的问题：${questionText}`,
  );
  const ms = Date.now() - t0;

  if (!r.ok) {
    return send(res, 200, { ...r, sent_notes: notes.length, ms });
  }

  const { text, cited, bad, bad_nums } = parseCitations(r.content, notes);

  return send(res, 200, {
    ok: true,
    question: questionText,
    answer: text,
    // 引用存的是笔记 id，前端拿它渲染成「打开这条笔记」
    cited: cited.map(i => ({ index: i, id: notes[i - 1].id, title: notes[i - 1].title })),
    // 模型引用了不存在的编号 = 它在编。必须告诉用户。
    bad_citations: bad,
    // 具体是哪几个编号，前端拿它把正文里那几处标红
    bad_nums,
    sent_notes: notes.map(n => ({ id: n.id, title: n.title })),
    usage: r.usage,
    cost: r.cost,
    returned_model: r.returned_model,
    finish_reason: r.finish_reason,
    ms,
  });
}

// ---------- 导出：把全部笔记拼成一个 Markdown 文件 ----------
// 为什么用 Markdown 而不是自定义格式：任何编辑器、记事本、Git 都能直接看，
// 以后想迁到别的工具也读得懂。这是你唯一一份「不在数据库里」的副本。
function buildMarkdown() {
  const notes = q.allForExport.all();
  const tasks = tq.allForExport.all();
  const marks = bmq.allForExport.all();
  const today = new Date();
  const stamp = `${today.getFullYear()}${String(today.getMonth()+1).padStart(2,'0')}${String(today.getDate()).padStart(2,'0')}`;
  const doneCount = tasks.filter((t) => t.status === 'done').length;
  // 开头就把「有几条收藏其实没存下内容」说出来 —— 不用翻到收藏那节去找。
  const bmFailCount = marks.filter((b) => b.fetch_error).length;
  const lines = [
    `# 个人工作台 · 导出备份`,
    ``,
    `> 导出时间：${today.toLocaleString('zh-CN')}`,
    `> 共 ${notes.length} 条笔记` +
       `${tasks.length ? `，${tasks.length} 条任务（已完成 ${doneCount}）` : ''}` +
       `${marks.length ? `，${marks.length} 条收藏` : ''}` +
       `${bmFailCount ? `（其中 ${bmFailCount} 条没抓到内容，原因见收藏那节）` : ''}`,
    ``,
    `---`,
    ``,
  ];
  for (const n of notes) {
    // 标题里的换行会破坏 Markdown 结构，统一压成空格
    lines.push(`# ${n.title.replace(/\s*\n\s*/g, ' ')}`, ``);
    lines.push(`> 创建：${n.created_at}　修改：${n.updated_at}`, ``);
    lines.push(n.body || '', ``, `---`, ``);
  }

  // 任务也必须进备份。这里用 Markdown 的任务列表语法：
  // - [ ] 是待办，- [x] 是已完成，勾选框在记事本/大多数编辑器里都能认出来。
  // 一条任务都没有时就不写这一节，导出内容和加任务之前完全一样。
  if (tasks.length) {
    lines.push(`# 任务`, ``);
    for (const t of tasks) {
      const box = t.status === 'done' ? '[x]' : '[ ]';
      const due = t.due_date ? `　（截止 ${t.due_date}）` : '';
      const done = t.status === 'done' && t.completed_at ? `　（完成于 ${t.completed_at}）` : '';
      lines.push(`- ${box} ${t.title}${due}${done}`);
    }
    lines.push(``, `---`, ``);
  }

  // 收藏用最朴素的「- [标题](链接)」，任何编辑器点得开。
  // URL 存进来之前已经被 normUrl 验过是 http/https 了，这里不用再担心协议。
  // 抓到摘要的就在下面缩进一行；没抓到就只一行，输出和抓取功能上线前完全一样。
  if (marks.length) {
    lines.push(`# 收藏`, ``);
    for (const b of marks) {
      // 标题这一格显示什么。
      //
      // 先说清楚一件事：**链接本身永远不截断**，截了就点不开了，那一行长就长着。
      // 真正的噪音是标题那一格 —— 建收藏时标题留空，后端会拿链接本身当标题兜底（见 POST /api/bookmarks），
      // 于是导出来变成 `- [<189 字的网址>](<189 字的网址>)`，一屏放不下半行。
      //
      // 所以这里复用 isAutoTitle()：标题是「我们自己按链接拼出来的」就用推导标题顶上，
      // 用户自己起的一个字都不动。这跟抓取时替换标题用的是同一把尺子，行为一致。
      const rawTitle = String(b.title || '').trim();
      const title = (!rawTitle || isAutoTitle(rawTitle, b.url))
        ? (titleFromUrl(b.url) || b.url)
        : rawTitle;
      lines.push(`- [${title.replace(/\s*\n\s*/g, ' ')}](${b.url})`);
      if (b.summary) lines.push(`  > ${b.summary.replace(/\s*\n\s*/g, ' ')}`);
      // 抓取状态也写进备份。**抓成功的不写** —— 那是默认状态，每条都写一遍全是噪音。
      // 但「没抓到」和「还没抓」必须写：这份文件是你唯一的备份，
      // 要是看不出来，过一个月回看会以为那几个网页本来就没内容，白重抓一遍。
      if (b.fetch_error) {
        lines.push(`  > 没抓到内容：${b.fetch_error.replace(/\s*\n\s*/g, ' ')}`);
      } else if (!b.fetched_at) {
        lines.push(`  > 还没抓取内容`);
      }
    }
    lines.push(``, `---`, ``);
  }
  return { text: lines.join('\n'), stamp };
}

function handleExport(req, res) {
  const { text, stamp } = buildMarkdown();
  const filename = `工作台备份-${stamp}.md`;
  res.writeHead(200, {
    'Content-Type': 'text/markdown; charset=utf-8',
    // 告诉浏览器这是下载，且文件名用 UTF-8 方式传，避免中文乱码
    'Content-Disposition': `attachment; filename="workbench-${stamp}.md"; filename*=UTF-8''${encodeURIComponent(filename)}`,
    'Content-Length': Buffer.byteLength(text),
  });
  res.end(text);
}

// ---------- 统一搜索 ----------
// 一次把笔记和任务都搜出来，前端只用发一个请求、只等一次响应。
// 「哪些东西能被搜到」这个决定收在后端这一处：
// 以后加了收藏表，这里加一段查询、返回里加一个 key，前端只管渲染。
function handleSearch(res, keyword) {
  const s = keyword.trim();
  if (!s) return send(res, 200, { keyword: '', notes: [], tasks: [], bookmarks: [] });
  const kw = `%${s}%`;
  return send(res, 200, {
    keyword: s,
    // 复用已经测过的查询：笔记搜标题和正文，任务搜标题，收藏搜标题/链接/摘要/正文
    notes: q.search.all(kw, kw),
    tasks: tq.search.all(kw),
    bookmarks: bmq.search.all(kw, kw, kw, kw),
  });
}

// ---------- 接口 ----------
async function api(req, res, url) {
  const seg = url.pathname.split('/').filter(Boolean); // ['api','notes',id?]

  // 导出全部笔记为 Markdown 文件
  if (req.method === 'GET' && url.pathname === '/api/export') {
    return handleExport(req, res);
  }

  // 统一搜索：笔记 + 任务一起搜
  if (req.method === 'GET' && url.pathname === '/api/search') {
    return handleSearch(res, url.searchParams.get('q') || '');
  }

  // ============ AI 配置（v3）===========
  // 注意这里只有「配置」接口，没有「调用」接口。
  // 调用要发真实网络请求、要花钱、会带上笔记内容，那是下一步单独做、单独测的事。
  if (req.method === 'GET' && url.pathname === '/api/ai/config') {
    return handleAiGetConfig(res);
  }
  if (req.method === 'PUT' && url.pathname === '/api/ai/config') {
    return await handleAiPutConfig(req, res);
  }

  // 列出这家服务商当前可用的模型（要密钥，不花钱，不产生对话内容）
  if (req.method === 'POST' && url.pathname === '/api/ai/models') {
    return await handleAiModels(req, res);
  }

  // 测试连通性：发一句最短的话，不带任何笔记内容
  if (req.method === 'POST' && url.pathname === '/api/ai/test') {
    return await handleAiTest(req, res);
  }

  // 问 AI：只发用户勾选的笔记
if (req.method === 'POST' && url.pathname === '/api/ai/ask') {
    return await handleAiAsk(req, res);
  }

  // ============ 自动更新 ============
  //
  // 这两个接口在 Node 版**永远回答「不支持」**。
  //
  // 为什么要有：界面 public/index.html 是两个后端共用的，
  // 上面加了「检查更新」按钮，Node 版不接这个路径就会 404，
  // 一点就报「接口出错」—— 那是假的，程序没坏，是这个版本压根没有这个能力。
  //
  // 为什么答 200 而不是 404：前端 api() 遇非 2xx 会抛异常，
  // 用户只能看到「接口出错：404」，看不到「这个版本不支持自动更新」这句能照做的说明。
  //
  // 自动更新本体只在桌面版（Tauri）里有 —— 那要 Rust 侧的 updater 插件，
  // Node 这里没有对应的东西，也不该去实现一个。
  const UNSUPPORTED_UPDATE = {
    ok: false,
    kind: 'unsupported',
    message: 'Node 版不支持自动更新，要换新版本请重新下载安装包。',
    // 这个仓库是零依赖的，连 package.json 都没有，**没有版本号可读**。
    // 与其编一个数字出来（编错了用户会拿它跟安装包对，对不上更困惑），
    // 不如照实说这是 Node 版。界面只需要拿它显示一行字。
    current: 'Node 版',
  };
  if (req.method === 'GET' && url.pathname === '/api/update/check') {
    return send(res, 200, UNSUPPORTED_UPDATE);
  }
  if (req.method === 'POST' && url.pathname === '/api/update/install') {
    return send(res, 200, UNSUPPORTED_UPDATE);
  }

  // 笔记列表 / 搜索
  if (req.method === 'GET' && url.pathname === '/api/notes') {
    const s = url.searchParams.get('search')?.trim();
    if (s) return send(res, 200, q.search.all(`%${s}%`, `%${s}%`));
    return send(res, 200, q.list.all());
  }

  // 新建
  if (req.method === 'POST' && url.pathname === '/api/notes') {
    const { title = '', body = '' } = await readBody(req);
    const info = q.create.run(title.trim() || '无标题', body);
    return send(res, 200, q.get.get(info.lastInsertRowid));
  }

  // 读取 / 更新 / 删除单条
  if (seg[0] === 'api' && seg[1] === 'notes' && seg[2]) {
    const id = Number(seg[2]);
    if (!Number.isInteger(id)) return send(res, 400, { error: 'bad id' });

    if (req.method === 'GET') {
      const note = q.get.get(id);
      return note ? send(res, 200, note) : send(res, 404, { error: 'not found' });
    }
    if (req.method === 'PUT') {
      const { title = '', body = '' } = await readBody(req);
      q.update.run(title.trim() || '无标题', body, id);
      const note = q.get.get(id);
      // 已删除的笔记不让再改，否则撤销回来会带着一串意外内容
      if (!note || note.deleted_at) return send(res, 404, { error: 'not found' });
      return send(res, 200, note);
    }
    // 撤销删除：把删除时间戳清空就回来了
    if (req.method === 'POST' && seg[3] === 'restore') {
      q.restore.run(id);
      const note = q.get.get(id);
      if (!note) return send(res, 404, { error: 'not found' });
      return send(res, 200, note);
    }
    // 软删除
    if (req.method === 'DELETE') {
      q.softDel.run(id);
      return send(res, 200, { ok: true });
    }
  }

  // ============ 任务接口 ============
  // 结构和笔记一模一样，刻意保持一致：以后加功能时照着抄一遍就行，不用重新想。

  // 任务列表 / 搜索
  if (req.method === 'GET' && url.pathname === '/api/tasks') {
    const s = url.searchParams.get('search')?.trim();
    if (s) return send(res, 200, tq.search.all(`%${s}%`));
    return send(res, 200, tq.list.all());
  }

  // 新建任务
  if (req.method === 'POST' && url.pathname === '/api/tasks') {
    const { title = '', due_date = '' } = await readBody(req);
    const info = tq.create.run(title.trim() || '未命名任务', normDue(due_date));
    return send(res, 200, tq.get.get(info.lastInsertRowid));
  }

  // 读取 / 更新 / 删除单条任务
  if (seg[0] === 'api' && seg[1] === 'tasks' && seg[2]) {
    const id = Number(seg[2]);
    if (!Number.isInteger(id)) return send(res, 400, { error: 'bad id' });

    if (req.method === 'GET') {
      const task = tq.get.get(id);
      return task ? send(res, 200, task) : send(res, 404, { error: 'not found' });
    }
    // 更新：标题、完成状态、截止日期一起存
    // 前端自动保存每次都带全三个字段，所以这里不做局部更新，避免漏字段
    if (req.method === 'PUT') {
      const { title = '', status = 'todo', due_date = '' } = await readBody(req);
      const st = status === 'done' ? 'done' : 'todo';   // 只认这两个值
      tq.update.run(title.trim() || '未命名任务', st, normDue(due_date), st, id);
      const task = tq.get.get(id);
      if (!task || task.deleted_at) return send(res, 404, { error: 'not found' });
      return send(res, 200, task);
    }
    // 撤销删除
    if (req.method === 'POST' && seg[3] === 'restore') {
      tq.restore.run(id);
      const task = tq.get.get(id);
      if (!task) return send(res, 404, { error: 'not found' });
      return send(res, 200, task);
    }
    // 软删除
    if (req.method === 'DELETE') {
      tq.softDel.run(id);
      return send(res, 200, { ok: true });
    }
  }

  // ============ 收藏接口 ============
  // 同样照抄任务的结构。这里多一条规矩：链接必须是合法的 http/https 才收。

  // 收藏列表 / 搜索
  if (req.method === 'GET' && url.pathname === '/api/bookmarks') {
    const s = url.searchParams.get('search')?.trim();
    if (s) return send(res, 200, bmq.search.all(`%${s}%`, `%${s}%`, `%${s}%`, `%${s}%`));
    return send(res, 200, bmq.list.all());
  }

  // 新建收藏
  if (req.method === 'POST' && url.pathname === '/api/bookmarks') {
    const { url = '', title = '' } = await readBody(req);
    const u = normUrl(url);
    // 链接不合法就别入库。这条收藏本身也没意义，留个空壳更难删。
    if (!u) return send(res, 400, { error: '需要一个 http:// 或 https:// 开头的链接' });
    const info = bmq.create.run(u, title.trim() || u);
    return send(res, 200, bmq.get.get(info.lastInsertRowid));
  }

  // 读取 / 更新 / 删除单条收藏
  if (seg[0] === 'api' && seg[1] === 'bookmarks' && seg[2]) {
    const id = Number(seg[2]);
    if (!Number.isInteger(id)) return send(res, 400, { error: 'bad id' });

    if (req.method === 'GET') {
      const b = bmq.get.get(id);
      return b ? send(res, 200, b) : send(res, 404, { error: 'not found' });
    }
    if (req.method === 'PUT') {
      const cur = bmq.get.get(id);
      if (!cur || cur.deleted_at) return send(res, 404, { error: 'not found' });
      const { url = cur.url, title = '' } = await readBody(req);
      const u = normUrl(url) || cur.url;      // 链接改坏了就保留原来的，别把收藏弄废
      // 后面 4 个 u 是给「链接变了就把正文/摘要清掉」那几个 CASE WHEN 用的
      bmq.update.run(u, title.trim() || u, u, u, u, u, id);
      return send(res, 200, bmq.get.get(id));
    }

    // ---------- v2.4：抓内容（自动 + 手动重试都走这一个接口） ----------
    // 刻意做成「前端来调」而不是「保存时后端顺手抓」：
    // 后端顺手抓会让保存接口被一个 8 秒的网络请求卡住，
    // 对方站点慢一点，你连「保存链接」都要干等。
    if (req.method === 'POST' && seg[3] === 'fetch') {
      const cur = bmq.get.get(id);
      if (!cur || cur.deleted_at) return send(res, 404, { error: 'not found' });
      try {
        const { html } = await fetchBookmarkContent(cur.url);
        const picked = extractFromHtml(html, cur.url);
        // 抓到的是登录框 → 算失败，保留原来的标题和正文
        if (looksLikeLoginWall(html, picked.content, picked.title)) {
          throw new Error('这个页面要登录才能看，抓到的是登录页');
        }
        // 只在「原来的标题就是自动生成的那个」时才换成网页真标题。
        // 判断依据：标题为空，或者标题长得就像我们自动拼的（末段 · 域名）。
        // 用户自己起的名字永远不动。
        const auto = isAutoTitle(cur.title, cur.url);
        const newTitle = (picked.title && auto) ? picked.title : cur.title;
        bmq.applyFetch.run(newTitle || cur.title, picked.content, picked.summary, id);
        return send(res, 200, bmq.get.get(id));
      } catch (e) {
        // 抓不到不算错误操作：链接照样存着，只是没内容。记下原因给用户点重试。
        bmq.markFetchFail.run(e.message || '抓取失败', id);
        return send(res, 200, { ...bmq.get.get(id), fetch_error: e.message || '抓取失败' });
      }
    }

    if (req.method === 'DELETE' && seg[3] === 'fetch') {   // 丢掉已抓到的内容（换链接前的「手动重来」）
      // 原来这里是直接 200 + get(id) 的结果，可 id 不存在时 get 返回 undefined，
      // 于是发出去的是「200 + 一个空 body」—— 桌面版那边是 404。
      // 两个后端行为必须一样：这个 id 压根不存在，就是 not found。
      const cur = bmq.get.get(id);
      if (!cur || cur.deleted_at) return send(res, 404, { error: 'not found' });
      bmq.clearFetch.run(id);
      return send(res, 200, bmq.get.get(id));
    }

    if (req.method === 'POST' && seg[3] === 'restore') {
      bmq.restore.run(id);
      const b = bmq.get.get(id);
      if (!b) return send(res, 404, { error: 'not found' });
      return send(res, 200, b);
    }
    if (req.method === 'DELETE') {
      bmq.softDel.run(id);
      return send(res, 200, { ok: true });
    }
  }

  // ============ 导入：把另一份 workbench.db 合并进来 ============
  // 和桌面版 import.rs 是同一个东西，连返回字段、去重规则、提示文案都一样。
  // 界���是同一个 index.html，两边必须都认这个接口，否则在一边点一下就报错。
  //
  // 请求体是那个 .db 文件的原始字节，不是 JSON：SQLite 是二进制，
  // 包成 JSON 会被转义成 \u00xx，大小翻好几倍还多一层出错的可能。
  if (req.method === 'POST' && url.pathname === '/api/import') {
    let bytes;
    try {
      bytes = await readBytes(req);
    } catch (e) {
      return send(res, 400, { error: e.message });
    }
    try {
      return send(res, 200, importDb(bytes));
    } catch (e) {
      // 「这个文件不行」都是用户选错文件，不是服务器故障，所以是 400。
      return send(res, 400, { error: e.message });
    }
  }

  return send(res, 404, { error: 'no such endpoint' });
}

// ---------- 导入 ----------
//
// 规则全写在 Rust 版的 import.rs 顶部（那边是主实现，这里是对齐的副本）：
//   · 只加东西，不删不改已有数据
//   · 笔记按「标题+正文」、任务按「标题+状态」、收藏按「链接」判重，重复的跳过
//   · 源库里已删除的行不导入，也拿它们去判重
//   · 全程一个事务，中途出错整笔回滚，不留「导了一半」的库
//   · 三张表缺一张就拒收（挡住随便一个 SQLite 文件）
// SQLite 文件的开头 16 字节。
// 用 \0 转义写出来，**不要把真正的空字节打进源码文件** ——
// 那是个看不见、复制粘贴就会丢、编辑器还会顺手改掉的字符。
// 这种坑的特点是「现在能跑，半年后换台电脑就打不开了」，非常难查。
const SQLITE_MAGIC = Buffer.from('SQLite format 3\0', 'latin1');

function importDb(bytes) {
  if (!bytes.length) throw new Error('这个文件是空的。');
  if (bytes.length < SQLITE_MAGIC.length || !bytes.subarray(0, SQLITE_MAGIC.length).equals(SQLITE_MAGIC)) {
    throw new Error('这不是工作台的数据文件（选到的是别的文件）。');
  }

  // 先落临时文件再打开：node:sqlite 只认文件，而且落一份副本就不用管
  // 源文件是不是正被谁开着。
  const tmp = path.join(path.dirname(DB_PATH), `import-${process.pid}-${Date.now()}.tmp`);
  fs.writeFileSync(tmp, bytes);
  try {
    return mergeDb(tmp);
  } finally {
    // 成功失败都要删，否则每导一次数据目录里就多一个垃圾文件。
    try { fs.unlinkSync(tmp); } catch { /* 已经不在了就算了 */ }
  }
}

function mergeDb(tmp) {
  const src = new DatabaseSync(tmp);
  try {
    const tables = new Set(
      src.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r => r.name)
    );
    for (const t of ['notes', 'tasks', 'bookmarks']) {
      if (!tables.has(t)) throw new Error(`这不是工作台的数据文件（里面没有 ${t} 表）。`);
    }
    // 兼容很老的库：某张表可能还没有某几列，读的时候按实际有的列来。
    const cols = t => new Set(src.prepare(`PRAGMA table_info(${t})`).all().map(c => c.name));
    const srcNotes = cols('notes'), srcTasks = cols('tasks'), srcBms = cols('bookmarks');

    // 表名是上面写死的三个常量，不含用户输入，拼进 SQL 是安全的。
    const rows = (t, have) => src.prepare(
      have.has('deleted_at') ? `SELECT * FROM ${t} WHERE deleted_at IS NULL`
                            : `SELECT * FROM ${t}`
    ).all();

    let added = { notes: 0, tasks: 0, bookmarks: 0 }, skipped = 0;

    db.exec('BEGIN');   // 一个事务：中途失败整笔回滚，不留半截数据
    try {
      for (const r of rows('notes', srcNotes)) {
        const title = str(r.title), body = str(r.body);
        if (has('SELECT 1 FROM notes WHERE deleted_at IS NULL AND title=? AND body=? LIMIT 1', title, body)) {
          skipped++; continue;
        }
        db.prepare(`INSERT INTO notes (title,body,created_at,updated_at) VALUES (?,?,?,?)`)
          .run(title, body, str(r.created_at), str(r.updated_at));
        added.notes++;
      }

      for (const r of rows('tasks', srcTasks)) {
        const title = str(r.title);
        // 老库里可能没有 status，缺了就当待办（跟建表默认值一致）。
        const status = srcTasks.has('status') && r.status ? r.status : 'todo';
        if (has('SELECT 1 FROM tasks WHERE deleted_at IS NULL AND title=? AND status=? LIMIT 1', title, status)) {
          skipped++; continue;
        }
        // due_date / completed_at 空值要写成 NULL 而不是空串，
        // 不然「有截止日期」的显示会跑偏。
        db.prepare(`INSERT INTO tasks (title,status,due_date,completed_at,created_at,updated_at)
                    VALUES (?,?,?,?,?,?)`)
          .run(title, status, nullIfEmpty(r.due_date), nullIfEmpty(r.completed_at),
               str(r.created_at), str(r.updated_at));
        added.tasks++;
      }

      for (const r of rows('bookmarks', srcBms)) {
        const url = str(r.url);
        if (!url) { skipped++; continue; }
        if (has('SELECT 1 FROM bookmarks WHERE deleted_at IS NULL AND url=? LIMIT 1', url)) {
          skipped++; continue;
        }
        db.prepare(`INSERT INTO bookmarks (url,title,created_at,updated_at,content,summary,fetched_at,fetch_error)
                    VALUES (?,?,?,?,?,?,?,?)`)
          .run(url, str(r.title), str(r.created_at), str(r.updated_at), str(r.content),
               str(r.summary), nullIfEmpty(r.fetched_at), nullIfEmpty(r.fetch_error));
        added.bookmarks++;
      }
      db.exec('COMMIT');
    } catch (e) {
      db.exec('ROLLBACK');
      throw e;
    }

    return { ...added, skipped };
  } finally {
    src.close();
  }
}

const str = v => (v === null || v === undefined) ? '' : String(v);
const nullIfEmpty = v => { const s = str(v); return s === '' ? null : s; };
function has(sql, ...args) {
  return db.prepare(sql).get(...args) !== undefined;
}

// ---------- 静态文件 ----------
const MIME = { '.html':'text/html; charset=utf-8', '.css':'text/css; charset=utf-8',
               '.js':'text/javascript; charset=utf-8', '.svg':'image/svg+xml' };

function staticFile(res, pathname) {
  const rel = pathname === '/' ? '/index.html' : pathname;
  const file = path.join(ROOT, 'public', path.normalize(rel).replace(/^(\.\.[/\\])+/, ''));
  if (!file.startsWith(path.join(ROOT, 'public'))) { res.writeHead(403); return res.end(); }
  fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(404); return res.end('not found'); }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream' });
    res.end(data);
  });
}

// ---------- 启动 ----------
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  try {
    if (url.pathname.startsWith('/api/')) return await api(req, res, url);
    return staticFile(res, url.pathname);
  } catch (err) {
    console.error(err);
    send(res, 500, { error: String(err.message || err) });
  }
});

server.on('error', (err) => {
  if (err.code === 'EADDRINUSE') {
    console.log(`端口 ${PORT} 被占用，可能工作台已经在运行了。直接打开 http://localhost:${PORT} 试试。`);
    process.exit(0);
  }
  throw err;
});

server.listen(PORT, () => {
  console.log(`\n  个人工作台已启动 →  http://localhost:${PORT}\n  关掉这个窗口就会停止运行\n`);
});
