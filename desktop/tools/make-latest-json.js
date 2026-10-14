// 生成 GitHub Releases 用的 latest.json（更新清单）—— 每次发版都要跑一次。
//
// 用法：
//   1. cargo tauri build --bundles nsis          （设好 TAURI_SIGNING_PRIVATE_KEY）
//   2. cargo tauri signer sign -f <私钥路径> --app-version <版本> -- "<安装包绝对路径>"
//   3. node desktop/tools/make-latest-json.js <版本号>
//   4. gh release create v<版本号> desktop\release\latest.json "<安装包>" --title ... --notes ...
//
// 为什么手写而不是用 tauri-action：那个是 GitHub Actions 里跑的，
// 而这个项目没上 CI，本地发版这几行就够。
//
// 三个字段：
//   version   界面上显示，也用来跟本机版本比大小
//   signature 客户端验签用的，内容就是 .sig 文件的全文（去掉换行）
//   url       下载地址 —— **必须是这个 tag 下的附件地址**，
//             不能用 /releases/latest/download/…：
//             那条指向「最新那个 release」，回头发新版时
//             还没更新的老版本会突然去下新版的包（而且验签会失败）
const fs = require('node:fs');
const path = require('node:path');

const NSIS_DIR = path.join(__dirname, '..', 'src-tauri', 'target', 'release', 'bundle', 'nsis');
const OUT_DIR = path.join(__dirname, '..', 'release');
const REPO = 'https://github.com/71ming/personal-workbench';

const VERSION = process.argv[2];
if (!VERSION) {
  console.error('用法：node desktop/tools/make-latest-json.js <版本号>');
  process.exit(1);
}
if (!/^\d+\.\d+\.\d+$/.test(VERSION)) {
  console.error(`版本号不像样：${VERSION}（要形如 0.6.0）`);
  process.exit(1);
}

const TAG = `v${VERSION}`;

// 附件名必须是**纯 ASCII**。
//
// 实测踩到的大坑：原来这里写的是「个人工作台_0.6.2_x64-setup.exe」，
// 但这个文件名是作为命令行参数传给 gh 的，PowerShell 5.1 传中文参数会掉编码 ——
// GitHub 上实际存下来的名字变成了「_0.6.2_x64-setup.exe」（中文前缀整个没了），
// 而清单里的 url 指向的还是中文名，于是点「下载并安装」必然 404。
//
// 这个坑特别阴险的地方在于：**「检查更新」照样显示一切正常**
// （清单能取到、版本号对得上），非要点到真的下载安装才会炸。
//
// 文件名走 ASCII 之后，这条路上就再也没有中文要编码了，
// 从根上断掉这类问题。软件中文名放在 release 标题里，一样能看见。
const NAME = `PersonalWorkbench_${VERSION}_x64-setup.exe`;
const NOTES = process.argv[3] || '';

// 签名文件是**跟着 NSIS 那个原始包名**走的（签名的时候包名还是中文的），
// 所以这里按后缀找，不按 NAME 拼 —— 否则每换一次附件命名规则就得重签一次。
const sigName = fs
  .readdirSync(NSIS_DIR)
  .find(n => n.endsWith(`_${VERSION}_x64-setup.exe.sig`));
if (!sigName) {
  console.error(`在 ${NSIS_DIR} 里找不到 _${VERSION}_x64-setup.exe.sig\n先跑 cargo tauri signer sign`);
  process.exit(1);
}
const sigPath = path.join(NSIS_DIR, sigName);
const sig = fs.readFileSync(sigPath, 'utf8').trim();
// 签名文件的开头固定是 base64 编码的 "untrusted comment: ..."。
// 对不上就说明拿错了文件（比如误拿了公钥）—— 那会让所有用户的更新全部失败。
if (!sig.startsWith('dW50cnVzdGVk')) {
  throw new Error('签名文件内容不对，开头不是 untrusted comment');
}

const manifest = {
  version: VERSION,
  notes: NOTES,
  pub_date: new Date().toISOString(),
  platforms: {
    // target 三元组 <系统>-<处理器>，必须和插件算出来的一致，
    // 对不上就是 TargetsNotFound，界面会显示「没有适配你这台电脑的版本」。
    'windows-x86_64': {
      signature: sig,
      url: `${REPO}/releases/download/${TAG}/${encodeURIComponent(NAME)}`,
    },
  },
};

fs.mkdirSync(OUT_DIR, { recursive: true });
fs.writeFileSync(path.join(OUT_DIR, 'latest.json'), JSON.stringify(manifest, null, 2), 'utf8');
// NSIS 打出来的原始文件带中文名，这里复制成 ASCII 名。
// 顺带记一条：**复制而不是改名** —— 原始包留在 target\release\bundle\nsis 里，
// 改掉它会让下次打包时 makensis 的输出路径对不上，多余的麻烦。
const src = fs.readdirSync(NSIS_DIR).find(n => n.endsWith(`_${VERSION}_x64-setup.exe`));
if (!src) {
  console.error(`在 ${NSIS_DIR} 里找不到 _${VERSION}_x64-setup.exe`);
  process.exit(1);
}
fs.copyFileSync(path.join(NSIS_DIR, src), path.join(OUT_DIR, NAME));

console.log('版本  ', manifest.version);
console.log('来源包', src);
console.log('地址  ', manifest.platforms['windows-x86_64'].url);
console.log('清单  ', path.join(OUT_DIR, 'latest.json'));
console.log('安装包', path.join(OUT_DIR, NAME));

// 收尾自查：清单里写的文件名，必须和实际复制出来的文件名一模一样。
// 这两条对不上就是「点下载必 404」，而「检查更新」那一步完全看不出来 ——
// 所以这行自查是必须的，不是锦上添花。
if (!fs.existsSync(path.join(OUT_DIR, NAME))) {
  console.error('复制后的安装包不存在');
  process.exit(1);
}
if (!decodeURIComponent(manifest.platforms['windows-x86_64'].url).endsWith('/' + NAME)) {
  console.error('清单里的文件名和实际文件名对不上');
  process.exit(1);
}
console.log('自查  清单地址与实际文件名一致 ✔');