// 任务接口的自动化测试（阶段 1a）
//
// 为什么用 Node 写而不是 PowerShell：
// PowerShell 5.1 处理嵌套的 JSON + 变量插值 + 引号，转义层级一多就出错
// （实测栽了好几次），而且报错信息指向的位置往往跟真正的问题没关系。
// Node 处理 JSON 是天生的，顺带还能做真正的断言而不是「打印出来人工看」。
//
// 用法：node desktop/tools/test-tasks.js http://127.0.0.1:8795

const BASE = process.argv[2] || 'http://127.0.0.1:8795';

let pass = 0;
let fail = 0;

function ok(name, cond, detail) {
  if (cond) {
    pass++;
    console.log(`  ✅ ${name}`);
  } else {
    fail++;
    console.log(`  ❌ ${name}${detail ? `  → ${detail}` : ''}`);
  }
}

async function req(method, path, body) {
  const res = await fetch(`${BASE}${path}`, {
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

(async () => {
  console.log(`\n测试目标：${BASE}\n`);

  // ---------------- 创建与兜底 ----------------
  console.log('【创建与兜底】');
  const empty = await req('POST', '/api/tasks', { title: '', due_date: '' });
  ok('空标题的任务存成「未命名任务」', empty.body.title === '未命名任务', `实际="${empty.body.title}"`);

  const spaces = await req('POST', '/api/tasks', { title: '    ', due_date: '' });
  ok('纯空格的标题也兜底', spaces.body.title === '未命名任务', `实际="${spaces.body.title}"`);

  const note = await req('POST', '/api/notes', { title: '', body: '正文' });
  ok('空标题的笔记存成「无标题」', note.body.title === '无标题', `实际="${note.body.title}"`);

  const keep = await req('POST', '/api/notes', { title: '  前后有空格  ', body: 'x' });
  ok('标题会 trim 但内容保留', keep.body.title === '前后有空格', `实际="${keep.body.title}"`);

  // 上面两条是为了测标题兜底而临时造的，测完立刻删掉。
  // 不删的话它们会一直躺在用户的笔记列表里 —— 而且「无标题」这种标题
  // 清理脚本不敢按名字去认（用户自己真写一条无标题笔记很正常），
  // 只能由造它的这个脚本自己负责收摊。
  await req('DELETE', `/api/notes/${note.body.id}`);
  await req('DELETE', `/api/notes/${keep.body.id}`);

  // ---------------- due_date 归一化 ----------------
  console.log('\n【due_date 归一化】');
  const cases = [
    ['2026-10-15', '2026-10-15', '合法日期原样保留'],
    ['2026/10/15', null, '斜杠格式拒绝'],
    // 注意：server.js 的 normDue() 只用正则看「形状像不像 YYYY-MM-DD」，
    // 不去校验这个日期真实存在不存在。所以 13 月 45 日照样原样存进去。
    // 这里跟着它一致 —— 两个后端行为必须一样，不然换版本时数据会对不上。
    ['2026-13-45', '2026-13-45', '月13日45这种假日期原样存（跟 server.js 一致）'],
    ['', null, '空字符串当成不设截止'],
    ['随便写的', null, '非日期文本拒绝'],
    ['2026-10-15 10:00', null, '带时间的也拒绝（只认纯日期）'],
  ];
  for (const [input, expect, name] of cases) {
    const r = await req('POST', '/api/tasks', { title: 't', due_date: input });
    const got = r.body.due_date;
    ok(name, got === expect, `传"${input}" → 存成 ${JSON.stringify(got)}，期望 ${JSON.stringify(expect)}`);
  }

  // ---------------- 排序 ----------------
  console.log('\n【排序规则】');
  // 清空已有的：全部软删掉，免得干扰
  const before = (await req('GET', '/api/tasks')).body;
  for (const t of before) await req('DELETE', `/api/tasks/${t.id}`);

  const mk = (title, due) => req('POST', '/api/tasks', { title, due_date: due });
  const a = (await mk('已完成无日期', '')).body;
  await mk('未完成无日期', '');
  await mk('未完成逾期', '2026-01-01');
  await mk('未完成将来', '2026-12-31');

  // 先把 a 标记完成再验排序：排序第一条规则就是「已完成的排最后」，
  // a 没标记完成之前它跟其它任务一样是待办，断言必然不成立。
  await req('PUT', `/api/tasks/${a.id}`, { title: a.title, status: 'done', due_date: '' });

  const list = (await req('GET', '/api/tasks')).body;
  const titles = list.map((t) => t.title);
  const expected = ['未完成逾期', '未完成将来', '未完成无日期', '已完成无日期'];
  ok('排序：未完成在前 → 逾期在前 → 无日期垫底 → 已完成最后',
    JSON.stringify(titles) === JSON.stringify(expected),
    `实际=${JSON.stringify(titles)}`);

  // 反向再验一次：取消完成后它应该回到未完成区的末尾（无日期垫底）
  await req('PUT', `/api/tasks/${a.id}`, { title: a.title, status: 'todo', due_date: '' });
  const list2 = (await req('GET', '/api/tasks')).body;
  const stillDone = list2.filter((t) => t.status === 'done');
  // 取消完成后它不再是 done，就得跟其它待办按 created_at 排队。
  // 它（id 14）比「未完成无日期」建得早，两者又都没有截止日期，所以排它前面。
  ok('取消完成后回到未完成区（不再垫底）',
    stillDone.length === 0 && list2[list2.length - 1].title !== '已完成无日期',
    `实际=${JSON.stringify(list2.map((t) => `${t.title}/${t.status}`))}`);
  await req('PUT', `/api/tasks/${a.id}`, { title: a.title, status: 'done', due_date: '' });

  // ---------------- completed_at 幂等 ----------------
  console.log('\n【完成时间幂等】');
  const t1 = (await mk('幂等测试', '')).body;
  const done1 = (await req('PUT', `/api/tasks/${t1.id}`, {
    title: t1.title, status: 'done', due_date: '',
  })).body;
  ok('完成后有 completed_at', !!done1.completed_at, `实际=${done1.completed_at}`);

  await new Promise((r) => setTimeout(r, 2000));
  const done2 = (await req('PUT', `/api/tasks/${t1.id}`, {
    title: t1.title, status: 'done', due_date: '',
  })).body;
  ok('重复保存已完成任务，完成时间不被刷新',
    done1.completed_at === done2.completed_at,
    `${done1.completed_at} vs ${done2.completed_at}`);

  const undone = (await req('PUT', `/api/tasks/${t1.id}`, {
    title: t1.title, status: 'todo', due_date: '',
  })).body;
  ok('取消完成后 completed_at 被清空', !undone.completed_at, `实际=${undone.completed_at}`);

  const redone = (await req('PUT', `/api/tasks/${t1.id}`, {
    title: t1.title, status: 'done', due_date: '',
  })).body;
  ok('重新完成后又有 completed_at', !!redone.completed_at, `实际=${redone.completed_at}`);

  // ---------------- status 白名单 ----------------
  console.log('\n【status 白名单】');
  for (const bad of ['DONE', '完成', 'Done', '', null, 'finished']) {
    const r = await req('PUT', `/api/tasks/${t1.id}`, {
      title: t1.title, status: bad, due_date: '',
    });
    ok(`status="${bad}" 被当成 todo`, r.body.status === 'todo', `实际=${r.body.status}`);
  }

  // ---------------- 软删除与恢复 ----------------
  console.log('\n【软删除与恢复】');
  const victim = (await mk('待删除', '2026-11-01')).body;
  const del = await req('DELETE', `/api/tasks/${victim.id}`);
  ok('DELETE 返回 {ok:true}', del.body.ok === true, JSON.stringify(del.body));

  const afterDel = (await req('GET', '/api/tasks')).body;
  ok('列表里已消失', !afterDel.some((t) => t.id === victim.id));

  const searchAfter = (await req('GET', `/api/tasks?search=${encodeURIComponent('待删除')}`)).body;
  ok('搜索里也搜不到', searchAfter.length === 0, `搜到 ${searchAfter.length} 条`);

  const putDeleted = await req('PUT', `/api/tasks/${victim.id}`, {
    title: '改一下', status: 'todo', due_date: '',
  });
  ok('已删除的不能改（404）', putDeleted.status === 404, `实际 ${putDeleted.status}`);

  const restored = (await req('POST', `/api/tasks/${victim.id}/restore`)).body;
  ok('恢复后回来了', restored.id === victim.id, JSON.stringify(restored));

  // ---------------- 搜索 ----------------
  console.log('\n【搜索】');
  // 用 encodeURIComponent 生成编码，不要手写 %XX —— 手写的很容易编错
  // （原版就编错过一次，搜「逾期」实际发出去的是另一个词，测出来是接口的锅）
  const s = await req('GET', `/api/tasks?search=${encodeURIComponent('逾期')}`);
  ok('中文搜索能命中（percent decode 生效）', s.body.length > 0, `搜到 ${s.body.length} 条`);
  const none = await req('GET', `/api/tasks?search=${encodeURIComponent('不存在的词zzz')}`);
  ok('搜不存在的返回空数组', Array.isArray(none.body) && none.body.length === 0);

  // ---------------- 统一搜索 ----------------
  console.log('\n【统一搜索】');
  const all = (await req('GET', `/api/search?q=${encodeURIComponent('逾期')}`)).body;
  ok('返回里 notes/tasks/bookmarks 三个 key 都在',
    'notes' in all && 'tasks' in all && 'bookmarks' in all,
    Object.keys(all).join(','));
  ok('tasks 里真的有内容（不再是空壳）', all.tasks.length > 0, `tasks=${all.tasks.length}`);

  // ---------------- 字段完整性 ----------------
  console.log('\n【返回字段】');
  const one = (await req('GET', `/api/tasks/${t1.id}`)).body;
  const want = ['id', 'title', 'status', 'due_date', 'completed_at', 'created_at', 'updated_at', 'deleted_at'];
  const missing = want.filter((k) => !(k in one));
  ok('单条返回全部 8 个字段', missing.length === 0, `缺：${missing.join(',')}`);

  // ---------------- 404 ----------------
  const nf = await req('GET', '/api/tasks/999999');
  ok('不存在的 id 返回 404', nf.status === 404, `实际 ${nf.status}`);
  // server.js 对非数字 id 返回 400 {error:'bad id'}（Number.isInteger 判断），
  // Rust 版跟着它一致。这是接口契约的一部分，两个后端不能一个 400 一个 404。
  const bad = await req('GET', '/api/tasks/abc');
  ok('非数字 id 返回 400 bad id', bad.status === 400 && bad.body.error === 'bad id',
    `实际 ${bad.status} ${JSON.stringify(bad.body)}`);

  console.log(`\n${'─'.repeat(46)}`);
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('测试脚本自己崩了：', e);
  process.exit(2);
});
