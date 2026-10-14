// 收藏接口的自动化测试（阶段 1b）
//
// 用法：node desktop/tools/test-bookmarks.js http://127.0.0.1:8795
//
// 为什么单独一个文件而不塞进 test-tasks.js：
// 每个阶段一个脚本，出问题时一眼看出是哪块。而且阶段 1b 不该依赖
// 阶段 1a 的测试数据 —— 独立跑才干净。

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

  // ---------------- 链接归一化（最重要的安全闸） ----------------
  console.log('【链接校验】');
  const badUrls = [
    'javascript:alert(1)',
    'data:text/html,<script>alert(1)</script>',
    'file:///C:/Windows/System32/config',
    'ftp://example.com/a',
    'vbscript:msgbox(1)',
    '  ',
    '',
  ];
  for (const u of badUrls) {
    const r = await req('POST', '/api/bookmarks', { url: u, title: 'x' });
    ok(`拒绝 "${u.slice(0, 24)}"`, r.status === 400, `实际 ${r.status}`);
  }

  const goodUrls = [
    ['http://example.com', 'http://example.com'],
    ['https://example.com/a/b', 'https://example.com/a/b'],
    ['HTTPS://EXAMPLE.COM/x', 'HTTPS://EXAMPLE.COM/x'],   // 大写协议也要收
    ['  https://example.com/  ', 'https://example.com/'],  // 两边空格 trim 掉
  ];
  for (const [input, expect] of goodUrls) {
    const r = await req('POST', '/api/bookmarks', { url: input, title: 't' });
    ok(`收下 "${input.slice(0, 28)}"`, r.body.url === expect,
      `存成 "${r.body.url}"，期望 "${expect}"`);
  }

  // ---------------- 标题兜底 ----------------
  console.log('\n【标题兜底】');
  const noTitle = await req('POST', '/api/bookmarks', { url: 'https://example.com/no-title', title: '' });
  ok('标题留空时用链接本身兜底', noTitle.body.title === 'https://example.com/no-title',
    `实际="${noTitle.body.title}"`);

  const spaces = await req('POST', '/api/bookmarks', { url: 'https://example.com/spaces', title: '   ' });
  ok('纯空格的标题也兜底', spaces.body.title === 'https://example.com/spaces',
    `实际="${spaces.body.title}"`);

  const own = await req('POST', '/api/bookmarks', { url: 'https://example.com/mine', title: '我自己起的名字' });
  ok('填了标题就用填的', own.body.title === '我自己起的名字');

  // ---------------- 抓取字段（1c 才用，但现在就得在） ----------------
  console.log('\n【字段完整性】');
  const want = ['id', 'url', 'title', 'created_at', 'updated_at', 'deleted_at',
    'content', 'summary', 'fetched_at', 'fetch_error'];
  const missing = want.filter((k) => !(k in noTitle.body));
  ok('新建返回全部 10 个字段', missing.length === 0, `缺：${missing.join(',')}`);

  // ---------------- 更新 ----------------
  console.log('\n【更新】');
  const target = (await req('POST', '/api/bookmarks', {
    url: 'https://example.com/original', title: '原标题',
  })).body;

  const renamed = await req('PUT', `/api/bookmarks/${target.id}`, {
    url: 'https://example.com/original', title: '新标题',
  });
  ok('只改标题，链接不变', renamed.body.title === '新标题' && renamed.body.url === 'https://example.com/original');

  const linkChanged = await req('PUT', `/api/bookmarks/${target.id}`, {
    url: 'https://example.com/changed', title: '新标题',
  });
  ok('改链接成功', linkChanged.body.url === 'https://example.com/changed');

  // 这条最容易漏：链接变了，正文/摘要必须跟着作废，
  // 否则会出现「链接是 A、内容是 B」——最难自己发现的一类错数据。
  const brokeUrl = await req('PUT', `/api/bookmarks/${target.id}`, {
    url: 'not-a-url', title: '还在',
  });
  ok('链接改坏了保留原来的', brokeUrl.body.url === 'https://example.com/changed',
    `实际="${brokeUrl.body.url}"`);

  // ---------------- 搜索 ----------------
  console.log('\n【搜索】');
  const kw = encodeURIComponent('搜索用的关键词');
  const byTitle = (await req('POST', '/api/bookmarks', {
    url: 'https://example.com/s1', title: '带搜索用的关键词的标题',
  })).body;

  const s1 = await req('GET', `/api/bookmarks?search=${kw}`);
  ok('按标题能搜到', s1.body.some((b) => b.id === byTitle.id), `搜到 ${s1.body.length} 条`);

  const s2 = await req('GET', `/api/bookmarks?search=${encodeURIComponent('example.com')}`);
  ok('按链接能搜到', s2.body.length > 0, `搜到 ${s2.body.length} 条`);

  const s3 = await req('GET', `/api/bookmarks?search=${encodeURIComponent('搜不到的词zzz')}`);
  ok('搜不存在的返回空数组', Array.isArray(s3.body) && s3.body.length === 0);

  // ---------------- 软删除与恢复 ----------------
  console.log('\n【软删除与恢复】');
  // 关键词带上时间戳：测试库里往往还留着上一轮跑出来的同名数据，
  // 不隔离的话「搜不到」这条会假红（第二轮就栽在这，搜出来的是上轮恢复回来的那条）。
  const uniq = `待删除的收藏-${Date.now()}`;
  const victim = (await req('POST', '/api/bookmarks', {
    url: 'https://example.com/victim', title: uniq,
  })).body;

  const del = await req('DELETE', `/api/bookmarks/${victim.id}`);
  ok('DELETE 返回 {ok:true}', del.body.ok === true, JSON.stringify(del.body));

  const afterDel = (await req('GET', '/api/bookmarks')).body;
  ok('列表里已消失', !afterDel.some((b) => b.id === victim.id));

  const searchAfter = await req('GET', `/api/bookmarks?search=${encodeURIComponent(uniq)}`);
  ok('搜索里也搜不到', searchAfter.body.length === 0, `搜到 ${searchAfter.body.length} 条`);

  const putDeleted = await req('PUT', `/api/bookmarks/${victim.id}`, {
    url: 'https://example.com/victim', title: '改一下',
  });
  ok('已删除的不能改（404）', putDeleted.status === 404, `实际 ${putDeleted.status}`);

  const restored = await req('POST', `/api/bookmarks/${victim.id}/restore`);
  ok('恢复后回来了', restored.body.id === victim.id, JSON.stringify(restored.body));

  const searchRestored = await req('GET', `/api/bookmarks?search=${encodeURIComponent(uniq)}`);
  ok('恢复后又能搜到了', searchRestored.body.length === 1,
    `搜到 ${searchRestored.body.length} 条`);

  // ---------------- 400 / 404 ----------------
  console.log('\n【状态码】');
  const nf = await req('GET', '/api/bookmarks/999999');
  ok('不存在的 id 返回 404', nf.status === 404, `实际 ${nf.status}`);

  const bad = await req('GET', '/api/bookmarks/abc');
  ok('非数字 id 返回 400 bad id', bad.status === 400 && bad.body.error === 'bad id',
    `实际 ${bad.status} ${JSON.stringify(bad.body)}`);

  // 1c 才实现的抓取接口，现在应该 404 而不是崩
  const fetch1c = await req('POST', `/api/bookmarks/${target.id}/fetch`);
  ok('抓取接口还没做（404，不是 500）', fetch1c.status === 404, `实际 ${fetch1c.status}`);

  // ---------------- 统一搜索 ----------------
  console.log('\n【统一搜索】');
  const all = (await req('GET', `/api/search?q=${kw}`)).body;
  ok('bookmarks 段不再是空壳', Array.isArray(all.bookmarks) && all.bookmarks.length > 0,
    `bookmarks=${all.bookmarks.length}`);
  ok('四段 key 都齐全',
    'keyword' in all && 'notes' in all && 'tasks' in all && 'bookmarks' in all,
    Object.keys(all).join(','));

  // ---------------- 中文标题（防 UTF-8 切片 panic） ----------------
  //
  // Rust 的字符串是 UTF-8，len() 数的是字节不是字符。
  // 后端拿用户标题去比对协议头时，切在了中文字符中间会直接 panic，
  // 表现是「导出发送请求 → 整个接口 500」。
  // 这组用例就是钉死这件事的：标题里必须能放中文。
  console.log('\n【中文标题】');
  const cnTitles = [
    '自己',
    '己',
    '自己起的名字',
    '中文标题测试一二三四五',
    '混合 English 中文',
    '短',
  ];
  for (const t of cnTitles) {
    const r = await req('POST', '/api/bookmarks', {
      url: 'https://example.com/cn', title: t,
    });
    ok(`标题 "${t}" 存得进去`, r.status === 200 && r.body.title === t,
      `实际 ${r.status} "${r.body.title}"`);
  }

  // 导出是最容易踩到切片的地方 —— 它会把每个标题都拿去和协议头比对
  const mdCn = await req('GET', '/api/export');
  ok('有中文标题时导出也不 500', mdCn.status === 200, `实际 ${mdCn.status}`);
  ok('中文标题原样出现在导出里', mdCn.body.includes('中文标题测试一二三四五'));

  // ---------------- 导出 ----------------
  console.log('\n【Markdown 导出】');
  const md = await req('GET', '/api/export');
  ok('导出 200', md.status === 200, `实际 ${md.status}`);
  ok('有「# 收藏」小节', md.body.includes('# 收藏'));
  ok('收藏写成可点的 Markdown 链接', /^- \[.+\]\(https?:\/\/.+\)$/m.test(md.body));

  console.log(`\n${'─'.repeat(46)}`);
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('测试脚本自己崩了：', e);
  process.exit(2);
});