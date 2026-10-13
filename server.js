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
  const today = new Date();
  const stamp = `${today.getFullYear()}${String(today.getMonth()+1).padStart(2,'0')}${String(today.getDate()).padStart(2,'0')}`;
  const doneCount = tasks.filter((t) => t.status === 'done').length;
  const lines = [
    `# 个人工作台 · 导出备份`,
    ``,
    `> 导出时间：${today.toLocaleString('zh-CN')}`,
    `> 共 ${notes.length} 条笔记${tasks.length ? `，${tasks.length} 条任务（已完成 ${doneCount}）` : ''}`,
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
  if (!s) return send(res, 200, { keyword: '', notes: [], tasks: [] });
  const kw = `%${s}%`;
  return send(res, 200, {
    keyword: s,
    // 复用已经测过的两条查询：笔记搜标题和正文，任务搜标题
    notes: q.search.all(kw, kw),
    tasks: tq.search.all(kw),
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
