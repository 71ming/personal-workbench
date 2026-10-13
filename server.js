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

const q = {
  list:     db.prepare('SELECT id,title,body,updated_at FROM notes ORDER BY updated_at DESC'),
  search:   db.prepare('SELECT id,title,body,updated_at FROM notes WHERE title LIKE ? OR body LIKE ? ORDER BY updated_at DESC'),
  get:      db.prepare('SELECT id,title,body,created_at,updated_at FROM notes WHERE id = ?'),
  create:   db.prepare('INSERT INTO notes (title,body) VALUES (?,?)'),
  update:   db.prepare('UPDATE notes SET title=?, body=?, updated_at=datetime(\'now\',\'localtime\') WHERE id=?'),
  del:      db.prepare('DELETE FROM notes WHERE id = ?'),
};

// ---------- 工具 ----------
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

// ---------- 接口 ----------
async function api(req, res, url) {
  const seg = url.pathname.split('/').filter(Boolean); // ['api','notes',id?]

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
      return send(res, 200, q.get.get(id));
    }
    if (req.method === 'DELETE') {
      q.del.run(id);
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
