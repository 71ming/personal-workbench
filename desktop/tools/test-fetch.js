// 网页抓取的自动化测试（阶段 1c）
//
// 用法：node desktop/tools/test-fetch.js http://127.0.0.1:8795
//
// 这份测试的重点**不在「能不能抓到网页」**（那要看网），而在
// **「不该抓的绝对抓不到」**。抓取是这个应用里唯一一处
// 「用户给个地址、我替他去访问」的功能，拦不住就是安全漏洞。

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

// 建一条收藏并返回它
async function mkbm(url, title) {
  const r = await req('POST', '/api/bookmarks', { url, title });
  if (r.status !== 200) throw new Error(`建收藏失败 ${r.status}: ${JSON.stringify(r.body)}`);
  return r.body;
}

(async () => {
  console.log(`\n测试目标：${BASE}\n`);

  // ==================== 内网拦截（最重要的一组） ====================
  console.log('【内网地址必须拦得住】');

  // 这些都是「贴进来就能攻击自己」或「碰到内网」的地址。
  // 期望：链接照样存成功（存链接本身没问题），但抓取一定失败，
  // 且 fetch_error 里写明是「不抓内网地址」。
  const attacks = [
    ['http://127.0.0.1:8795/api/notes', '读工作台自己的笔记'],
    ['http://127.0.0.1:8795/api/bookmarks', '读工作台自己的收藏'],
    ['http://localhost:8795/api/export', '导出工作台数据库'],
    ['http://169.254.169.254/latest/meta-data/', '云主机元数据（含密钥）'],
    ['http://192.168.1.1/', '路由器后台'],
    ['http://10.0.0.5/', '内网主机'],
    ['http://[::1]:8795/api/notes', 'IPv6 回环'],
    ['http://[fd00::1]/', 'IPv6 内网'],
    ['http://[fe80::1]/', 'IPv6 链路本地'],
    ['http://0.0.0.0:8795/api/notes', '0.0.0.0 也是本机'],
    ['http://127.1.2.3/', '回环的非常规写法'],
    ['http://172.16.0.1/', '172.16/12 内网'],
  ];

  for (const [url, why] of attacks) {
    const bm = await mkbm(url, '攻击测试');
    const r = await req('POST', `/api/bookmarks/${bm.id}/fetch`);
    const err = r.body?.fetch_error || '';
    const blocked = r.status === 200
      && typeof err === 'string'
      && err.includes('不抓内网');
    ok(`拦住 ${url}\n       （${why}）`, blocked,
      `状态 ${r.status}，fetch_error=${JSON.stringify(err)}`);

    // 关键：**内容必须是空的**。拦住了却把内容写进去，等于没拦。
    const after = await req('GET', `/api/bookmarks/${bm.id}`);
    ok(`  ↳ 没抓到任何内容`, !after.body.content && !after.body.fetched_at,
      `content=${JSON.stringify(String(after.body.content).slice(0, 50))} fetched_at=${after.body.fetched_at}`);
  }

  // ==================== 协议限制 ====================
  console.log('\n【只放行 http/https】');
  for (const url of ['ftp://example.com/x', 'file:///C:/Windows/win.ini']) {
    // norm_url 在建收藏那一步就会拒，所以这里只能验到 400
    const r = await req('POST', '/api/bookmarks', { url, title: 'x' });
    ok(`建收藏时就拒绝 ${url}`, r.status === 400, `实际 ${r.status}`);
  }

  // ==================== 真实抓取 ====================
  // 联网的才跑。网络不通时跳过，不算失败 —— 那是环境问题不是代码问题。
  console.log('\n【真实抓取（需要联网）】');

  const target = await mkbm('https://example.com/', '');
  ok('标题留空时用链接兜底', target.title === 'https://example.com/',
    `实际="${target.title}"`);

  const fetched = await req('POST', `/api/bookmarks/${target.id}/fetch`);
  const fe = fetched.body?.fetch_error;

  if (fe) {
    console.log(`  ⚠️  抓取失败（跳过内容断言）：${fe}`);
    ok('失败时返回 200 而不是 500', fetched.status === 200, `实际 ${fetched.status}`);
    ok('失败原因写进 fetch_error', typeof fe === 'string' && fe.length > 0);
    const afterFail = await req('GET', `/api/bookmarks/${target.id}`);
    ok('抓失败也没把标题改掉', afterFail.body.title === 'https://example.com/',
      `实际="${afterFail.body.title}"`);
  } else {
    ok('抓到标题', !!fetched.body.title, JSON.stringify(fetched.body.title));
    // example.com 的正文很短，但应该有内容
    ok('抓到正文', !!fetched.body.content && fetched.body.content.length > 0,
      `content 长度 ${fetched.body?.content?.length}`);
    ok('抓到 fetched_at', !!fetched.body.fetched_at);
    ok('成功后 fetch_error 清空', !fetched.body.fetch_error);
    // 自动标题（就是链接本身）应该被网页真标题替换掉
    ok('自动标题被网页真标题替换', fetched.body.title !== 'https://example.com/',
      `实际="${fetched.body.title}"`);
  }

  // ==================== 用户自己起的标题不许动 ====================
  console.log('\n【用户标题优先】');
  const own = await mkbm('https://example.com/', '我自己起的名字');
  const ownFetched = await req('POST', `/api/bookmarks/${own.id}/fetch`);
  if (ownFetched.body?.fetch_error) {
    console.log(`  ⚠️  抓取失败，跳过：${ownFetched.body.fetch_error}`);
  } else {
    ok('用户自己起的标题不被网页标题覆盖',
      ownFetched.body.title === '我自己起的名字',
      `实际="${ownFetched.body.title}"`);
  }

  // ==================== 改链接后内容作废 ====================
  console.log('\n【改链接后内容作废】');
  // 用两个都真实存在的地址：example.com 和 iana.org
  const swapped = await mkbm('https://example.com/', '改链接测试');
  const got = await req('POST', `/api/bookmarks/${swapped.id}/fetch`);
  if (got.body?.fetch_error) {
    console.log(`  ⚠️  抓取失败，跳过：${got.body.fetch_error}`);
  } else {
    // 换成一个真实存在的新链接（原来用的 example.org/b 是 404，
    // 那测的是「对方返回 404」而不是「改链接清空内容」）
    const changed = await req('PUT', `/api/bookmarks/${swapped.id}`, {
      url: 'https://www.iana.org/help/example-domains', title: '改链接测试',
    });
    ok('改链接成功', changed.body.url === 'https://www.iana.org/help/example-domains',
      `实际="${changed.body.url}"`);
    ok('改链接后正文被清空', !changed.body.content,
      `content=${JSON.stringify(String(changed.body.content).slice(0, 60))}`);
    ok('改链接后摘要被清空', !changed.body.summary);
    ok('改链接后 fetched_at 被清空', !changed.body.fetched_at);
  }

  // ==================== 手动清空 ====================
  console.log('\n【DELETE .../fetch 清空内容】');
  const clearme = await mkbm('https://example.com/', '清空测试');
  const cf = await req('POST', `/api/bookmarks/${clearme.id}/fetch`);
  if (cf.body?.fetch_error) {
    console.log(`  ⚠️  抓取失败，跳过：${cf.body.fetch_error}`);
  } else {
    ok('清空前有内容', !!cf.body.content);
    const cleared = await req('DELETE', `/api/bookmarks/${clearme.id}/fetch`);
    ok('DELETE fetch 返回 200', cleared.status === 200, `实际 ${cleared.status}`);
    ok('内容已清空', !cleared.body.content);
    ok('摘要已清空', !cleared.body.summary);
    ok('fetched_at 已清空', !cleared.body.fetched_at);
    ok('链接没被改动', cleared.body.url === 'https://example.com/',
      `实际="${cleared.body.url}"`);
  }

  // ==================== 不存在的收藏 ====================
  console.log('\n【404】');
  const nf = await req('POST', '/api/bookmarks/999999/fetch');
  ok('抓不存在的收藏返回 404', nf.status === 404, `实际 ${nf.status}`);
  const nfd = await req('DELETE', '/api/bookmarks/999999/fetch');
  ok('清空不存在的收藏返回 404', nfd.status === 404, `实际 ${nfd.status}`);

  // ==================== 搜索能搜到正文 ====================
  console.log('\n【搜正文】');
  // 用 fetched（target 抓取的结果），它的 content 里有 example.com 的正文
  if (fetched.body?.content) {
    const firstWord = fetched.body.content.split(/\s+/).find((w) => w.length >= 3);
    if (firstWord) {
      const s = await req('GET', `/api/bookmarks?search=${encodeURIComponent(firstWord)}`);
      ok('能按正文内容搜到', s.body.length > 0, `关键词「${firstWord}」搜到 ${s.body.length} 条`);
    }
  } else {
    console.log('  ⚠️  没抓到正文，跳过');
  }

  console.log(`\n${'─'.repeat(46)}`);
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('测试脚本自己崩了：', e);
  process.exit(2);
});