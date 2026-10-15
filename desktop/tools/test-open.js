// 「打开链接」接口的自动化测试
//
// 用法：node desktop/tools/test-open.js http://127.0.0.1:8795
//       node desktop/tools/test-open.js http://127.0.0.1:8787   ← Node 版
//
// —— 这个套件能测什么、不能测什么 ——
//
// 能测（接口契约那部分，两个后端都得满足）：
//   · 路径在，两个后端都在（界面共用，缺一边就报错）
//   · 只放行 http/https，file: / javascript: / data: 一律挡掉  ← **最要紧的**
//   · 缺参数、非 GET 是 400，不是 500
//   · 挡掉的时候说的是**中文人话**，不是英文错误原文
//   · 「这条链接不合法」和「服务器坏了」是两回事（400 vs 500）
//
// **不能测**（只能靠真在应用里点一下）：
//   · 系统浏览器到底有没有真的弹出来
//   · 弹出来的是不是你的默认浏览器
//   · **在 Tauri 窗口里点「打开链接」会不会在应用内弹网页窗口**
//     —— 那正是这个接口要解决的问题本身，只能在真窗口里验。
//
// 所以这里有一条**故意只测「不弹窗」那一半**：
// 用一个绝不会有人真去打开的地址（example.com 的保留测试域名）
// 调一次接口，确认接口接受它。这不会打扰你 —— 它只是告诉系统「用默认浏览器打开
// example.com」，真弹出来的话关掉就行。
//
// 反过来，非法地址必须确认**不会**有任何东西被打开 —— 那才是真正的安全断言。

const BASE = process.argv[2] || 'http://127.0.0.1:8795';
const isNode = BASE.includes('8787');

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

async function req(method, p) {
  const res = await fetch(`${BASE}${p}`, { method });
  let body = null;
  try { body = await res.json(); } catch { /* 非 JSON 也是一种结果 */ }
  return { status: res.status, body };
}

(async () => {
  console.log(`测试目标：${BASE}\n`);

  console.log('【危险地址必须被挡掉】');
  // 这些是**真正要盯住**的：后端一旦放行，界面上存一个收藏就能干坏事。
  const BAD = [
    ['file:///C:/Windows/System32/cmd.exe', '本地 exe'],
    ['file:///etc/passwd', '本地 passwd'],
    ['javascript:alert(1)', 'javascript:'],
    ['data:text/html,<h1>hi</h1>', 'data:'],
    ['ms-msdt:/id PCWDiagnostic', 'ms-msdt:'],
    ['ftp://example.com/a', 'ftp:'],
  ];
  for (const [u, label] of BAD) {
    const r = await req('GET', '/api/open?url=' + encodeURIComponent(u));
    ok(
      `${label} 被挡住`,
      r.status === 400,
      `返回 ${r.status}（应该 400）`
    );
    ok(
      `${label} 说的是中文人话`,
      // 注意**不能**断言「一句英文都没有」：提示里带上协议名是应该的
      // （「不支持 javascript: 开头的地址」比「不支持这个地址」有用得多）。
      // 这里要盯的是「有没有中文、是不是在讲人话」，不是「有没有英文单词」。
      typeof r.body?.message === 'string'
        && /[\u4e00-\u9fa5]/.test(r.body.message),
      `message = ${JSON.stringify(r.body?.message)}`
    );
  }

  console.log('\n【根本不是网址的东西】');
  for (const [u, label] of [
    ['', '空串'],
    ['随便写点什么', '一句中文'],
    ['example.com', '少了协议'],
    ['http://', '没有域名'],
  ]) {
    const r = await req('GET', '/api/open?url=' + encodeURIComponent(u));
    // 空串那条后端可能当成「缺参数」，返回的也是 400，断言同样成立
    ok(`${label} 被挡住`, r.status === 400, `返回 ${r.status}`);
  }

  console.log('\n【参数给全了但方法不对】');
  const r1 = await req('POST', '/api/open?url=' + encodeURIComponent('https://example.com/'));
  ok('POST 返回 400 而不是 500', r1.status === 400, `返回 ${r1.status}`);

  console.log('\n【正常链接被接受】');
  // 用 example.com —— IANA 保留给文档用的域名，不指向任何真实站点。
  // 如果你的机器真弹出了浏览器，**关掉就行**，这是预期的。
  const r2 = await req('GET', '/api/open?url=' + encodeURIComponent('https://example.com/'));
  ok('https 被接受', r2.status === 200, `返回 ${r2.status}`);
  ok('返回 ok:true', r2.body?.ok === true, JSON.stringify(r2.body));

  const r3 = await req('GET', '/api/open?url=' + encodeURIComponent('http://example.com/'));
  ok('http 也被接受', r3.status === 200, `返回 ${r3.status}`);

  console.log('\n【首尾空格不能挡掉正常链接】');
  // 用户从别处复制网址过来几乎总带着空格。
  // 直接报错等于让用户自己猜哪里多了个字符。
  const r4 = await req('GET', '/api/open?url=' + encodeURIComponent('  https://example.com/a  '));
  ok('带空格照样能开', r4.status === 200, `返回 ${r4.status}`);

  console.log('\n【带查询参数的地址】');
  // 这一条最容易出事：URL 里的 & 在 shell 眼里是命令连接符。
  // 后端必须把它当成普通字符原样传给浏览器，而不是交给 cmd.exe。
  const tricky = 'https://example.com/search?q=a&b=c&d=hello world';
  const r5 = await req('GET', '/api/open?url=' + encodeURIComponent(tricky));
  ok('含 & 和空格的地址能被接受', r5.status === 200, `返回 ${r5.status}`);

  console.log('\n【路径尾斜杠和多段斜杠】');
  const r6 = await req('GET', '/api/open/?url=' + encodeURIComponent('https://example.com/'));
  ok('尾斜杠也能认', r6.status === 200, `返回 ${r6.status}`);

  console.log(`\n通过 ${pass}，失败 ${fail}`);
  console.log('\n注意：**系统浏览器有没有真的弹出来，这个脚本验不了**。');
  console.log('需要在桌面版窗口里手动点一次收藏的「打开链接 ↗」才算数。');
  if (isNode) console.log('（Node 版本来就跑在浏览器里，那边 target="_blank" 本来也正常。）');

  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => { console.error(e); process.exit(2); });