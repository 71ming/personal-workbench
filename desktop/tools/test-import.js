// 导入功能的自动化测试（阶段 2a）
//
// 用法：node desktop/tools/test-import.js http://127.0.0.1:8795
//       node desktop/tools/test-import.js http://127.0.0.1:8787   ← Node 版
//
// 这份测试能同时验证两个后端，因为两边做的是同一件事、同一套返回字段。
// 一份测试打两遍，比写两份测试更容易发现「两边行为不一致」。
//
// 测的重点有三类：
//   1. 搬得对不对（字段、时间、软删除、空日期）
//   2. 会不会造重复（同一个文件导两次）
//   3. 选错文件会不会出事（不是数据库、别的软件的文件、太大的文件）

const fs = require('fs');
const os = require('os');
const path = require('path');
const { DatabaseSync } = require('node:sqlite');

const BASE = process.argv[2] || 'http://127.0.0.1:8795';

// 每轮测试用带时间戳的关键词，测完按它清理。
// 不带时间戳的话，历史测试数据留在库里会让计数对不上（测过这个坑）。
const KW = `导入测试${Date.now()}`;

// 收藏的域名也带时间戳。
//
// 为什么连域名也要唯一：**上一轮测试崩了来不及清理时，残留会让下一轮全线变红**，
// 而且失败的样子长得像「功能坏了」，实际上只是上一轮忘了收摊（实测踩过）。
// 域名唯一之后，每轮的数据天然跟别轮不沾边，崩了也只是这一轮难看一点。
const HOST = `import-test-${Date.now()}.example.com`;

let pass = 0;
let fail = 0;

function ok(name, cond, detail) {
  if (cond) {
    pass++;
    console.log(`  ✅ ${name}`);
  } else {
    fail++;
    console.log(`  ❌ ${name}${detail !== undefined ? `  → ${detail}` : ''}`);
  }
}

async function req(method, p, body) {
  const res = await fetch(`${BASE}${p}`, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = text;
  }
  return { status: res.status, body: json };
}

// 上传一个 .db 文件。body 是 Buffer —— 走的正是浏览器传 File 的那条路。
async function upload(buffer) {
  const res = await fetch(`${BASE}/api/import`, { method: 'POST', body: buffer });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = text;
  }
  return { status: res.status, body: json };
}

// 建一份「源库」：模拟别人（或你自己另一个版本）导出来的 workbench.db。
function makeSourceFile() {
  const file = path.join(os.tmpdir(), `wb-import-src-${Date.now()}.db`);
  const db = new DatabaseSync(file);
  db.exec(`
    CREATE TABLE notes (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '',
      created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE tasks (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      title TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo',
      due_date TEXT, completed_at TEXT,
      created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '',
      deleted_at TEXT
    );
    CREATE TABLE bookmarks (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      url TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '',
      created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '',
      deleted_at TEXT
    );
  `);
  // 老版本库还缺 notes.deleted_at，下面的插入就正好能测到「老库也能导」。
  db.exec("ALTER TABLE notes ADD COLUMN deleted_at TEXT");
  db.exec("ALTER TABLE bookmarks ADD COLUMN content TEXT NOT NULL DEFAULT ''");
  db.exec("ALTER TABLE bookmarks ADD COLUMN summary TEXT NOT NULL DEFAULT ''");
  db.exec("ALTER TABLE bookmarks ADD COLUMN fetched_at TEXT");
  db.exec("ALTER TABLE bookmarks ADD COLUMN fetch_error TEXT");

  const ins = db.prepare('INSERT INTO notes (title,body,created_at,updated_at) VALUES (?,?,?,?)');
  ins.run(`${KW}甲`, '第一条正文', '2021-03-04 05:06:07', '2021-03-04 05:06:07');
  ins.run(`${KW}乙`, '第二条正文', '2022-07-08 09:10:11', '2022-07-08 09:10:11');
  // 已删除的：不该被搬过去
  ins.run(`${KW}丙`, '已删掉的', '2023-01-01 00:00:00', '2023-01-01 00:00:00');
  db.prepare("UPDATE notes SET deleted_at='2023-02-02 00:00:00' WHERE title=?").run(`${KW}丙`);

  db.prepare('INSERT INTO tasks (title,status,due_date,completed_at,created_at,updated_at) VALUES (?,?,?,?,?,?)')
    .run(`${KW}已完成的任务`, 'done', null, '2023-05-06 07:08:09', '2022-01-01 00:00:00', '2023-05-06 07:08:09');
  db.prepare('INSERT INTO tasks (title,status,due_date,created_at,updated_at) VALUES (?,?,?,?,?)')
    .run(`${KW}有截止的任务`, 'todo', '2025-12-31', '2024-01-01 00:00:00', '2024-01-01 00:00:00');

  const insBm = db.prepare(
    'INSERT INTO bookmarks (url,title,created_at,updated_at,content,summary,fetched_at) VALUES (?,?,?,?,?,?,?)'
  );
  // a 的链接在目标库里已经有了，导入时会被判重复跳过（用来测去重），
  // 所以**正文内容要挂在 b 上** —— 否则测「抓到的正文有没有搬过来」就没东西可测。
  insBm.run(`https://${HOST}/a`, `${KW}收藏一`,
    '2021-05-05 05:05:05', '2021-05-05 05:05:05', '', '', null);
  insBm.run(`https://${HOST}/b`, `${KW}收藏二`,
    '2021-05-05 05:05:05', '2021-05-05 05:05:05', '抓下来的正文内容', '一句摘要', '2021-05-06 06:06:06');

  db.close();
  return file;
}

(async () => {
  console.log(`\n测试目标：${BASE}`);
  console.log(`测试关键词：${KW}\n`);

  // ==================== 前置：目标库里先造一条「已经有的」数据 ====================
  // 用来测去重：源库里的「甲」故意和这条一模一样。
  const existing = await req('POST', '/api/notes', { title: `${KW}甲`, body: '第一条正文' });
  ok('前置：目标库里已有一条同名同正文的笔记', existing.status === 200);
  const existingBm = await req('POST', '/api/bookmarks', {
    url: `https://${HOST}/a`, title: `${KW}目标里已有的收藏`,
  });
  ok('前置：目标库里已有一个同链接的收藏', existingBm.status === 200);

  // ==================== 第一组：正常导入 ====================
  console.log('【正常导入】');
  const srcFile = makeSourceFile();
  const srcBytes = fs.readFileSync(srcFile);

  const r1 = await upload(srcBytes);
  ok('导入返回 200', r1.status === 200, `状态 ${r1.status}`);
  // 源库有：笔记 3 条（其中 1 条已删）、任务 2 条、收藏 2 条
  // 目标库里已有：甲（重复）、收藏 a（同链接）
  // 所以应该是：笔记进 1（乙）、任务进 2、收藏进 1（b）
  ok('笔记只进 1 条（甲重复、丙已删除）', r1.body.notes === 1, `实际 ${r1.body.notes}`);
  ok('任务进 2 条', r1.body.tasks === 2, `实际 ${r1.body.tasks}`);
  ok('收藏只进 1 条（a 的链接已经有了）', r1.body.bookmarks === 1, `实际 ${r1.body.bookmarks}`);
  ok('跳过 2 条（甲重复、收藏 a 重复）', r1.body.skipped === 2, `实际 ${r1.body.skipped}`);

  // ==================== 第二组：搬过来的内容对不对 ====================
  console.log('【搬过来的字段】');

  const notesRes = await req('GET', `/api/notes?search=${encodeURIComponent(KW)}`);
  const myNotes = (notesRes.body || []).filter((n) => !n.deleted_at);
  const jia = myNotes.find((n) => n.title === `${KW}甲`);
  const yi = myNotes.find((n) => n.title === `${KW}乙`);
  ok('目标库里有甲（本来就有的那条）', !!jia);
  ok('目标库里有乙（新导入的）', !!yi);
  ok('正文原样搬过来', yi && yi.body === '第二条正文', yi && yi.body);
  // 这里**不查笔记的 created_at**：笔记列表接口从来就不返回它
  //（排序只用 id，见 Node 版 nq.list），接口里没有的东西，测试也测不到。
  // 创建时间有没有被保留，放在下面用任务来验 —— 任务接口是返回 created_at 的。
  ok('已删除的丙没有被搬过来', !myNotes.some((n) => n.title === `${KW}丙`));
  ok('同一条笔记没有被导成两份', myNotes.filter((n) => n.title === `${KW}甲`).length === 1);

  const tasksRes = await req('GET', `/api/tasks?search=${encodeURIComponent(KW)}`);
  const myTasks = (tasksRes.body || []).filter((t) => !t.deleted_at);
  const doneTask = myTasks.find((t) => t.title === `${KW}已完成的任务`);
  const dueTask = myTasks.find((t) => t.title === `${KW}有截止的任务`);
  ok('已完成的任务状态保留', doneTask && doneTask.status === 'done', doneTask && doneTask.status);
  ok('完成时间保留', doneTask && doneTask.completed_at === '2023-05-06 07:08:09', doneTask && doneTask.completed_at);
  ok('创建时间保留（不是导入时间）',
     doneTask && doneTask.created_at === '2022-01-01 00:00:00', doneTask && doneTask.created_at);
  ok('没设截止日期的是 null 而不是空串', doneTask && doneTask.due_date === null,
     doneTask && JSON.stringify(doneTask.due_date));
  ok('有截止日期的日期保留', dueTask && dueTask.due_date === '2025-12-31', dueTask && dueTask.due_date);

  const bmRes = await req('GET', `/api/bookmarks?search=${encodeURIComponent(KW)}`);
  const myBms = (bmRes.body || []).filter((b) => !b.deleted_at);
  const bmB = myBms.find((b) => b.title === `${KW}收藏二`);
  ok('新收藏进来了', !!bmB);
  ok('抓下来的正文保留下来（不用重新去网上抓一次）',
     !!bmB && bmB.content === '抓下来的正文内容', bmB && bmB.content);
  ok('摘要保留', !!bmB && bmB.summary === '一句摘要', bmB && bmB.summary);
  ok('抓取时间保留', !!bmB && bmB.fetched_at === '2021-05-06 06:06:06', bmB && bmB.fetched_at);
  const bmByUrl = (await req('GET', `/api/bookmarks?search=${encodeURIComponent(HOST)}`))
    .body.filter((b) => !b.deleted_at && b.url === `https://${HOST}/a`);
  ok('目标库里原有的那条没被覆盖（标题还是原来的）',
     bmByUrl.length === 1 && bmByUrl[0].title === `${KW}目标里已有的收藏`,
     JSON.stringify(bmByUrl.map((b) => b.title)));

  // ==================== 第三组：导两次不能造重复 ====================
  console.log('【重复导入是安全的】');
  const r2 = await upload(srcBytes);
  ok('第二次导入返回 200', r2.status === 200);
  ok('第二次一条都不新增', r2.body.notes === 0 && r2.body.tasks === 0 && r2.body.bookmarks === 0,
     JSON.stringify(r2.body));
  ok('第二次全部算重复跳过', r2.body.skipped === 6,
     `实际 ${r2.body.skipped}（甲乙 + 两条任务 + 两条收藏，全都成了重复；已删除的丙压根读不出来，不计数）`);

  const after = await req('GET', `/api/notes?search=${encodeURIComponent(KW)}`);
  ok('导入两次后笔记条数不变',
     (after.body || []).filter((n) => !n.deleted_at && n.title.startsWith(KW)).length === 2,
     `实际 ${(after.body || []).filter((n) => !n.deleted_at && n.title.startsWith(KW)).length}`);

  // ==================== 第四组：选错文件 ====================
  console.log('【选错文件不会出事】');

  const bad1 = await upload(Buffer.from('我根本不是一个数据库文件'));
  ok('选到 txt 之类 → 400', bad1.status === 400, `状态 ${bad1.status}`);
  ok('错误提示是中文的、说清原因',
     typeof bad1.body.error === 'string' && bad1.body.error.includes('不是工作台的数据文件'),
     JSON.stringify(bad1.body));

  const bad2 = await upload(Buffer.alloc(0));
  ok('空文件 → 400', bad2.status === 400, `状态 ${bad2.status}`);
  ok('空文件提示说「空」', bad2.body.error && bad2.body.error.includes('空'), JSON.stringify(bad2.body));

  // 一个真的是 SQLite、但没有工作台三张表的库
  const alienFile = path.join(os.tmpdir(), `wb-import-alien-${Date.now()}.db`);
  const alien = new DatabaseSync(alienFile);
  alien.exec('CREATE TABLE browsers (id INTEGER PRIMARY KEY, name TEXT)');
  alien.close();
  const bad3 = await upload(fs.readFileSync(alienFile));
  ok('别的软件的数据库 → 400', bad3.status === 400, `状态 ${bad3.status}`);
  ok('提示说清缺哪张表', bad3.body.error && bad3.body.error.includes('notes'), JSON.stringify(bad3.body));
  fs.unlinkSync(alienFile);

  // 超大文件
  const big = await upload(Buffer.alloc(6 * 1024 * 1024, 0x41));
  ok('超过 5MB 的文件 → 400', big.status === 400, `状态 ${big.status}`);
  ok('提示说「太大」', big.body.error && big.body.error.includes('太大'), JSON.stringify(big.body));

  // 导完之后目标库必须还是好的（前面几个错误请求不该把库弄坏）
  const sanity = await req('GET', '/api/notes?search=' + encodeURIComponent(KW));
  ok('一通乱试之后数据库还正常', sanity.status === 200 && Array.isArray(sanity.body));

  // ==================== 清理 ====================
  console.log('【清理测试数据】');
  for (const [kind, p] of [['笔记', 'notes'], ['任务', 'tasks'], ['收藏', 'bookmarks']]) {
    const list = (await req('GET', `/api/${p}?search=${encodeURIComponent(KW)}`)).body || [];
    let n = 0;
    for (const item of list) {
      if (item.deleted_at) continue;
      await req('DELETE', `/api/${p}/${item.id}`);
      n++;
    }
    console.log(`  清掉 ${kind} ${n} 条`);
  }
  for (const b of (await req('GET', `/api/bookmarks?search=${encodeURIComponent(HOST)}`)).body || []) {
    if (!b.deleted_at) await req('DELETE', `/api/bookmarks/${b.id}`);
  }
  fs.unlinkSync(srcFile);
  console.log('  源库临时文件已删除');

  console.log(`\n通过 ${pass}，失败 ${fail}\n`);
  process.exit(fail ? 1 : 0);
})();