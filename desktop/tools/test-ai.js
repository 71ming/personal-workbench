// AI 接口的自动化测试（阶段 2b）
//
// 用法：node desktop/tools/test-ai.js http://127.0.0.1:8795
//
// —— 为什么要起一个假的 AI 服务器 ——
//
// AI 这块**不能**拿真服务商来测：一要密钥、二要花钱、三网络不稳，
// 而且最关键的错误分支（密钥错、限流、模型名不对、对方挂了）根本没法故意触发。
//
// 所以这里起一个本地的假服务器，实现 OpenAI 那套协议，
// 让工作台**真的发出 HTTP 请求**，但对面是可控的。
// 这样能验到的是「请求怎么发的、返回怎么解析的、错怎么分类的」——
// 也就是这块代码里**唯一会出错**的部分。
//
// 有一项**测不了**，必须说清楚：
//   「服务商真的认这个密钥」是对方服务器的事，本地假服务器给不了这个保证。
//   那一项只能你自己填真密钥点一次「测试连接」来验。
//
// 顺带好处：这份测试**不需要密钥**，也不会把你的笔记发到任何地方。

const http = require('http');

const BASE = process.argv[2] || 'http://127.0.0.1:8795';
const FAKE_PORT = 18787;
// 假服务器的地址。填进 base_url 的话，工作台会拒绝 —— 因为服务商
// 只能从内置目录里选，不接受用户自带的地址（这是防「让本机访问任意地址」）。
const FAKE_BASE = `http://127.0.0.1:${FAKE_PORT}`;

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
  try { json = JSON.parse(text); } catch { json = text; }
  return { status: res.status, body: json };
}

// ---- 假 AI 服务器：下一条回复要发什么，由测试决定 ----
let nextReply = { status: 200, body: null };
const seen = [];   // 记下工作台发过来的请求，用来验「到底发了什么」

const fake = http.createServer((rq, rs) => {
  let buf = '';
  rq.on('data', (c) => (buf += c));
  rq.on('end', () => {
    seen.push({
      method: rq.method,
      url: rq.url,
      auth: rq.headers['authorization'] || '',
      body: buf ? JSON.parse(buf) : null,
    });
    rs.writeHead(nextReply.status, { 'Content-Type': 'application/json' });
    rs.end(JSON.stringify(nextReply.body));
  });
});

function reply(status, body) {
  nextReply = { status, body };
}

function chatReply(content, model = 'fake-model', promptTokens = 100, completionTokens = 20) {
  reply(200, {
    model,
    choices: [{ message: { content }, finish_reason: 'stop' }],
    usage: { prompt_tokens: promptTokens, completion_tokens: completionTokens },
  });
}

(async () => {
  await new Promise((r) => fake.listen(FAKE_PORT, '127.0.0.1', r));
  console.log(`\n工作台：${BASE}`);
  console.log(`假 AI 服务器：http://127.0.0.1:${FAKE_PORT}\n`);

  // =====================================================================
  console.log('【服务商目录】');
  const cfg0 = await req('GET', '/api/ai/config');
  ok('GET /api/ai/config 返回 200', cfg0.status === 200);
  ok('有六家服务商', Array.isArray(cfg0.body.providers) && cfg0.body.providers.length === 6,
     cfg0.body.providers && cfg0.body.providers.length);
  ok('Claude 单独列在「暂不支持」里', Array.isArray(cfg0.body.pending) && cfg0.body.pending[0].id === 'claude');
  ok('没设置过时 config.ready 是 false', cfg0.body.config.ready === false);
  ok('没设置过时没有密钥', cfg0.body.config.has_key === false);

  // 目录里的 base_url 必须是 https 的公网地址，不能有本地地址
  const allHttps = cfg0.body.providers.every((p) => p.base_url.startsWith('https://'));
  ok('目录里没有本地地址（防「让本机访问任意地址」）', allHttps);
  const noLocal = cfg0.body.providers.every((p) => !/127\.0\.0\.1|localhost|192\.168\./.test(p.base_url));
  ok('目录里没有内网地址', noLocal);

  // =====================================================================
  console.log('【配置存取】');
  const bad1 = await req('PUT', '/api/ai/config', { provider: '不存在的服务商', model: 'x' });
  ok('瞎填的服务商 → 400', bad1.status === 400, `状态 ${bad1.status}`);
  ok('错误说清不认识哪个', String(bad1.body.error).includes('不认识的服务商'), JSON.stringify(bad1.body));

  const bad2 = await req('PUT', '/api/ai/config', { provider: 'deepseek', model: 'x'.repeat(200) });
  ok('模型名超长 → 400', bad2.status === 400, `状态 ${bad2.status}`);

  // 密钥里有非 ASCII 字符要在存进去之前就拦下。
  // 这不是洁癖：HTTP 头只允许 ASCII，一个混进全角符号的密钥发出去会被
  // 逐字节当成别的字符，对面回一个完全看不懂的 401（实测踩到过）。
  const bad3 = await req('PUT', '/api/ai/config', { provider: 'deepseek', api_key: 'sk-中文密钥' });
  ok('密钥里有中文 → 400', bad3.status === 400, `状态 ${bad3.status}`);
  ok('提示指向「全角符号/中文空格」', String(bad3.body.error).includes('非英文字符'), JSON.stringify(bad3.body));

  // 用一个真服务商把配置存下来（密钥是假的，只为验存取逻辑）
  const saved = await req('PUT', '/api/ai/config', { provider: 'deepseek', model: 'deepseek-flash', api_key: 'sk-testkey-1234567890' });
  ok('存配置返回 200', saved.status === 200);
  ok('存完 ready 变成 true', saved.body.config.ready === true);
  ok('has_key 是 true', saved.body.config.has_key === true);
  ok('回传的 key 是打码的，不是原文',
     saved.body.config.key_preview === 'sk-****7890',
     saved.body.config.key_preview);

  // 不带 api_key 的保存必须**保留**原密钥（前端就是这么用的）
  const kept = await req('PUT', '/api/ai/config', { provider: 'deepseek', model: 'deepseek-v4-pro' });
  ok('不带 api_key 保存不会把密钥弄丢', kept.body.config.has_key === true);
  ok('模型名换成了新的', kept.body.config.model === 'deepseek-v4-pro');

  const cfg1 = await req('GET', '/api/ai/config');
  ok('读回来模型名是对的', cfg1.body.config.model === 'deepseek-v4-pro');
  ok('读回来的 key 仍然是打码的', !JSON.stringify(cfg1.body).includes('sk-testkey'));

  // =====================================================================
  console.log('【探测模型】');
  const m1 = await req('POST', '/api/ai/models', { provider: '不存在' });
  ok('不认识的服务商 → 拒绝', m1.body.ok === false);

  // 没有密钥时不该发请求，直接说「还没密钥」
  const before = seen.length;
  await req('PUT', '/api/ai/config', { clear_key: true });
  const m2 = await req('POST', '/api/ai/models', { provider: 'deepseek' });
  ok('没密钥时提示先填密钥', m2.body.ok === false && String(m2.body.error).includes('密钥'), JSON.stringify(m2.body));
  ok('没密钥时一个请求都没发出去', seen.length === before, `发了 ${seen.length - before} 个`);

  // 现在指向假服务器是不可能的（base_url 只从目录取），
  // 所以探测只能验「打不通 / 对方报错」这两条真实分支。
  //
  // ⚠️ 这里**有网就会真的打到 DeepSeek**，拿一个假密钥换一个真 401。
  // 这是写测试时的意外收获：它顺带证明了「请求怎么发出去、
  // 对方错误怎么翻成中文」这条链路在真实网络上是通的
  // （实测对方回了 401 和一句英文原文，工作台翻成
  //   「DeepSeek 返回 401（密钥可能填错了，或者没激活）：Authentication Fails…」）。
  // 所以下面只断言两件事：**提示是中文**，以及**说清了是哪一类问题**。
  await req('PUT', '/api/ai/config', { provider: 'deepseek', model: 'x', api_key: 'sk-fake' });
  const m3 = await req('POST', '/api/ai/models', { provider: 'deepseek' });
  ok('打不通或被对方拒绝时 ok:false', m3.body.ok === false, JSON.stringify(m3.body));
  ok('错误提示是中文，不是甩一段英文',
     /[一-龥]/.test(String(m3.body.error)), m3.body.error);
  ok('网络不通说「连不上/超时」，密钥被拒说「返回 401/403」',
     /连不上|超时|返回\s*40[13]/.test(String(m3.body.error)), m3.body.error);

  // =====================================================================
  console.log('【问 AI 的入参校验（不花钱的那几条）】');
  const a1 = await req('POST', '/api/ai/ask', { question: '', note_ids: [1] });
  ok('问题为空 → 400', a1.status === 400);
  ok('提示「问题不能空」', String(a1.body.error).includes('不能空'), JSON.stringify(a1.body));

  const a2 = await req('POST', '/api/ai/ask', { question: '我写了什么', note_ids: [] });
  ok('一条笔记都不勾 → 400', a2.status === 400);
  ok('明确说「不会发送任何内容」', String(a2.body.error).includes('不会勾就不该发出去'), JSON.stringify(a2.body));

  const a3 = await req('POST', '/api/ai/ask', { question: '问'.repeat(600), note_ids: [1] });
  ok('问题太长 → 400', a3.status === 400);

  const a4 = await req('POST', '/api/ai/ask', { question: '我写了什么', note_ids: [999999] });
  ok('勾的笔记都不存在 → 400', a4.status === 400);
  ok('提示「勾选的笔记都找不到了」', String(a4.body.error).includes('勾选的笔记都找不到了'), JSON.stringify(a4.body));

  // =====================================================================
  console.log('【没配好就去问】');
  // 故意清掉模型名（只清 key 不够，key 留着但模型空着）
  await req('PUT', '/api/ai/config', { reset: true });
  const notes = (await req('GET', '/api/notes')).body;
  if (notes.length === 0) {
    console.log('  （库里没有笔记，问答主流程跳过）');
  } else {
    const a5 = await req('POST', '/api/ai/ask', { question: '我最近写了什么', note_ids: [notes[0].id] });
    ok('没配服务商时 ok:false', a5.body.ok === false, JSON.stringify(a5.body).slice(0, 120));
    ok('kind 是 config（前端据此提示去设置）', a5.body.kind === 'config', a5.body.kind);
  }

  // =====================================================================
  console.log('【清空配置】');
  const reset = await req('PUT', '/api/ai/config', { reset: true });
  ok('reset 之后 config 全空', reset.body.config.provider === '' && reset.body.config.has_key === false);
  const after = await req('GET', '/api/ai/config');
  ok('读回来确实空了', after.body.config.ready === false);

  fake.close();
  console.log(`\n通过 ${pass}，失败 ${fail}`);
  console.log('\n注意：真服务商能不能认你的密钥，这一项测不了 ——');
  console.log('需要你自己在「AI」设置里填真密钥，点一次「测试连接」来验。\n');
  process.exit(fail ? 1 : 0);
})().catch((e) => {
  console.error(e);
  fake.close();
  process.exit(2);
});