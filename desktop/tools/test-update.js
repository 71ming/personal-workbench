// 自动更新接口的自动化测试（阶段 3）
//
// 用法：node desktop/tools/test-update.js http://127.0.0.1:8795
//
// —— 这个套件能测什么、不能测什么 ——
//
// 能测（接口契约那部分，两个后端都得满足）：
//   · 两个路径在，两个后端都在（界面共用，缺一边就报错）
//   · 失败一律 200 + ok:false，绝不是 4xx/5xx
//   · kind 是前端认得的那几个值之一
//   · current 字段永远在（界面要拿它显示版本号）
//   · message 是中文、不是英文错误原文
//   · Node 版老老实实说「不支持」，不假装自己能更新
//   · 不认识的子路径是 404
//
// **不能测**（只能靠真的发一版来验）：
//   · 「装了新版本之后真的能升级上去」—— 那要真发布一次、真下载、真安装。
//     这一项我在发布 0.6.0/0.6.1 时人工验过，脚本验不了。
//
// 特别注意：**签名不对时必须报 signature，不能报 network**。
// 那不是「体验问题」，是安全提示被稀释了 ——
// 用户会以为重试一下就能好，其实永远不可能成功。

const BASE = process.argv[2] || 'http://127.0.0.1:8795';
const isNode = BASE.includes('8787');

let pass = 0;
let fail = 0;
const KINDS = ['network', 'manifest', 'signature', 'install_failed', 'not_available', 'unsupported'];

function ok(name, cond, detail) {
  if (cond) {
    pass++;
    console.log(`  ✅ ${name}`);
  } else {
    fail++;
    console.log(`  ❌ ${name}${detail !== undefined ? `  → ${detail}` : ''}`);
  }
}

async function req(method, p) {
  const res = await fetch(`${BASE}${p}`, { method });
  let body = null;
  try { body = await res.json(); } catch { /* 非 JSON 也算一种结果，记下来 */ }
  return { status: res.status, body };
}

function looksLikeChinese(s) {
  return typeof s === 'string' && /[\u4e00-\u9fa5]/.test(s) && !/[A-Za-z]{4,}\s*error/i.test(s);
}

(async () => {
  console.log(`\n自动更新接口测试 · ${BASE}\n${'─'.repeat(50)}`);

  // ---------------- 路径存在 ----------------
  console.log('\n【两个路径都在】');
  const check = await req('GET', '/api/update/check');
  ok('GET /api/update/check 有响应', check.status > 0, `status=${check.status}`);
  ok('check 返回的是 JSON', check.body && typeof check.body === 'object');

  const install = await req('POST', '/api/update/install');
  ok('POST /api/update/install 有响应', install.status > 0, `status=${install.status}`);
  ok('install 返回的是 JSON', install.body && typeof install.body === 'object');

  // ---------------- 失败约定 ----------------
  console.log('\n【失败一律 200 + ok:false】');
  // 这条是硬约定：前端 api() 遇非 2xx 会抛异常，
  // 用户就只能看到「接口出错」，精心写的人话提示根本显示不出来。
  for (const [name, r] of [['check', check], ['install', install]]) {
    ok(`${name} 不是 4xx/5xx`, r.status === 200, `实际 ${r.status}`);
    // 成功的话 ok 必须是 true；失败必须是 false。两个后端都不允许「不 ok 也不 false」。
    ok(`${name} 的 ok 是布尔值`, typeof r.body?.ok === 'boolean', `实际 ${JSON.stringify(r.body?.ok)}`);
  }

  // ---------------- 字段完整性 ----------------
  console.log('\n【字段完整性】');
  for (const [name, r] of [['check', check], ['install', install]]) {
    ok(`${name} 带 current`, r.body && 'current' in r.body, JSON.stringify(r.body));
    if (r.body && r.body.ok === false) {
      ok(`${name} 失败时带 kind`, typeof r.body.kind === 'string', JSON.stringify(r.body));
      ok(`${name} 的 kind 前端认得`, KINDS.includes(r.body.kind), `实际 ${r.body.kind}`);
      ok(`${name} 的 message 是中文`, looksLikeChinese(r.body.message), `实际「${r.body.message}」`);
      ok(`${name} 没有把英文错误名糊给用户`, !/Error|error:/i.test(r.body.message || ''), r.body.message);
    }
  }

  // ---------------- Node 版特有约定 ----------------
  console.log('\n【Node 版行为】');
  if (isNode) {
    ok('check 说清楚不支持', check.body?.kind === 'unsupported', JSON.stringify(check.body));
    ok('install 也说清楚不支持', install.body?.kind === 'unsupported', JSON.stringify(install.body));
    ok('给了能照做的下一步', /下载|安装包|官网/.test(check.body?.message || ''), check.body?.message);
    ok('没有编一个版本号出来', check.body?.current === 'Node 版', `实际 ${check.body?.current}`);
  } else {
    ok('桌面版不报 unsupported（它就是干这个的）',
      check.body?.kind !== 'unsupported', JSON.stringify(check.body));
    ok('桌面版带得出真实版本号（形如 0.x.y）',
      /^\d+\.\d+\.\d+$/.test(check.body?.current || ''), `实际 ${check.body?.current}`);
  }

  // ---------------- 成功路径长什么样 ----------------
  // 这一项只在「真发布了新版本」时才成立。
  // 没发布时 check 应该是 ok:false（取不到清单），
  // 而不是 ok:true + available:false —— 因为清单压根取不到，
  // 说「已经是最新」是**撒谎**：用户会以为检查过了，其实什么都没验到。
  console.log('\n【成功路径】');
  if (check.body?.ok === true) {
    ok('available 是布尔值', typeof check.body.available === 'boolean', JSON.stringify(check.body));
    if (check.body.available) {
      ok('有更新时带 version', typeof check.body.version === 'string' && check.body.version.length > 0);
      ok('版本号比当前的新', compareVer(check.body.version, check.body.current) > 0,
        `${check.body.version} vs ${check.body.current}`);
      ok('notes 字段在（哪怕是空字符串）', 'notes' in check.body);
    } else {
      ok('没有更新时不该带 version', !('version' in check.body), JSON.stringify(check.body));
    }
  } else {
    // unsupported 也算数 —— Node 版压根没有更新通道，
    // 「取不到清单」对它来说是必然结果，不是故障。
    // 只放行 manifest / network / unsupported 这三种，
    // 其余（比如报成 install_failed）都说明分类写歪了。
    ok('取不到清单时报 manifest/network/unsupported，而不是谎称「已是最新」',
      ['manifest', 'network', 'unsupported'].includes(check.body?.kind),
      `实际 ${JSON.stringify(check.body)}`);
  }

  // ---------------- 不认识的子路径 ----------------
  console.log('\n【不认识的子路径】');
  const nope = await req('GET', '/api/update/nope');
  ok('未知子路径返回 404', nope.status === 404, `实际 ${nope.status}`);

  console.log(`\n${'─'.repeat(50)}`);
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('测试脚本自己崩了：', e);
  process.exit(2);
});

// 比较两个版本号（只处理 x.y.z，够用了）
function compareVer(a, b) {
  const pa = String(a).split('.').map(Number);
  const pb = String(b).split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) - (pb[i] || 0);
  }
  return 0;
}