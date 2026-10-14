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
const NAME = `个人工作台_${VERSION}_x64-setup.exe`;
const NOTES = process.argv[3] || '';

const sigPath = path.join(NSIS_DIR, `${NAME}.sig`);
if (!fs.existsSync(sigPath)) {
  console.error(`找不到签名文件：${sigPath}\n先跑 cargo tauri signer sign`);
  process.exit(1);
}
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
fs.copyFileSync(path.join(NSIS_DIR, NAME), path.join(OUT_DIR, NAME));

console.log('版本  ', manifest.version);
console.log('地址  ', manifest.platforms['windows-x86_64'].url);
console.log('清单  ', path.join(OUT_DIR, 'latest.json'));
console.log('安装包', path.join(OUT_DIR, NAME));