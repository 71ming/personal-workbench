// 个人工作台 · 服务端
// 零依赖：只用 Node 内置 http + node:sqlite
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const PORT = 8787;
const ROOT = __dirname;
const DB_PATH = path.join(ROOT, 'data', 'workbench.db');

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
      // 标题为空是合法的（新建后没打字，或者从别处导入的脏数据）。
      // 这里必须兜住：`- [](url)` 是坏掉的 Markdown，导出文件里等于这条收藏没了。
      // 先试着用链接推一个，推不出就把链接本身当标题 —— 至少是合法链接，点得开。
      const title = String(b.title || '').trim() || titleFromUrl(b.url) || b.url;
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

  return send(res, 404, { error: 'no such endpoint' });
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
