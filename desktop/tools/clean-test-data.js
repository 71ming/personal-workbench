// 清掉测试期间往数据库里塞的临时收藏。
//
// 测试脚本每次都建新收藏，跑几轮下来能堆到一百多条。
// 这些是垃圾数据，留在界面上会干扰人工验证。
//
// 只删「指向 example.com / example.org / iana.org 这类测试地址」的，
// 不碰别的东西 —— 万一你自己存过真收藏，不能被这个脚本误删。
//
// 用法：node desktop/tools/clean-test-data.js http://127.0.0.1:8795

const BASE = process.argv[2] || 'http://127.0.0.1:8795';

// 这些域名只可能来自测试脚本，没有真人会收藏 example.com
const TEST_HOSTS = ['example.com', 'example.org', 'www.iana.org'];

// 内网地址同理：test-fetch.js 会建一堆「攻击测试」收藏来验拦截，
// 它们也全是垃圾。但**只按标题认**，不按地址 ——
// 万一你自己真收藏了一个内网地址（比如公司 Wiki），不能被误删。
const ATTACK_TITLE = /^攻击测试$/;

async function req(method, path, body) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  try { return { status: res.status, body: JSON.parse(text) }; }
  catch { return { status: res.status, body: text }; }
}

(async () => {
  const all = await req('GET', '/api/bookmarks');
  if (all.status !== 200 || !Array.isArray(all.body)) {
    console.error('拿不到收藏列表，退出');
    process.exit(1);
  }

  const targets = all.body.filter((b) => {
    if (ATTACK_TITLE.test((b.title || '').trim())) return true;
    try {
      const h = new URL(b.url).hostname.toLowerCase();
      return TEST_HOSTS.includes(h);
    } catch {
      return false;
    }
  });

  if (targets.length === 0) {
    console.log('没有测试数据，库里是干净的');
    process.exit(0);
  }

  console.log(`发现 ${targets.length} 条测试收藏，开始清理…`);
  let okCount = 0;
  for (const b of targets) {
    const r = await req('DELETE', `/api/bookmarks/${b.id}`);
    if (r.status === 200) okCount++;
  }

  // 再软删掉之前遗留的同名测试任务（它们的链接指向 example.com）
  const tasks = await req('GET', '/api/tasks');
  if (Array.isArray(tasks.body)) {
    for (const t of tasks.body) {
      if (/未完成逾期|未完成将来|未完成无日期|已完成无日期|幂等测试|待删除$/.test(t.title)) {
        await req('DELETE', `/api/tasks/${t.id}`);
        okCount++;
      }
    }
  }

  console.log(`已清理 ${okCount} 条`);
  const after = await req('GET', '/api/bookmarks');
  console.log(`现在库里还剩 ${after.body.length} 条收藏`);
})().catch((e) => {
  console.error(e);
  process.exit(2);
});