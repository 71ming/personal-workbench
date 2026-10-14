// 临时脚本：生成 GitHub Releases 用的 latest.json（更新清单）。
//
// 为什么要手写：cargo tauri build 只负责出安装包，
// latest.json 是 tauri-action（CI 用的）才会生成的，这里没上 CI。
//
// 三个字段的作用：
//   version   界面上显示、也用来跟本机版本比大小
//   signature 客户端验签用的，内容就是 .sig 文件的全文（去掉换行）
//   url       下载地址 —— 必须是**这个 tag 下的附件地址**，
//             不能用 /releases/latest/download/…：
//             那条指向「最新那个 release」，回头发新版时老版本会指向新版的包
const fs = require('fs');

const NSIS_DIR = 'E:/个人工作台/desktop/src-tauri/target/release/bundle/nsis';
const OUT_DIR = 'E:/个人工作台/desktop/release';
const VERSION = '0.6.0';
const TAG = `v${VERSION}`;
const REPO = 'https://github.com/71ming/personal-workbench';
const NAME = `个人工作台_${VERSION}_x64-setup.exe`;
const NOTES = '首次启动引导 + 自动更新（GitHub Releases，签名验签）。数据不受影响。';

const sig = fs.readFileSync(`${NSIS_DIR}/${NAME}.sig`, 'utf8').trim();
if (!sig.startsWith('dW50cnVzdGVk')) throw new Error('签名文件内容不对，开头不是 untrusted comment');

const manifest = {
  version: VERSION,
  notes: NOTES,
  pub_date: new Date().toISOString(),
  platforms: {
    // target 三元组：<系统>-<处理器>。这个字符串必须跟插件算出来的一致，
    // 对不上就是 TargetsNotFound —— 界面会显示「没有适配你这台电脑的版本」。
    'windows-x86_64': {
      signature: sig,
      url: `${REPO}/releases/download/${TAG}/${encodeURIComponent(NAME)}`,
    },
  },
};

fs.mkdirSync(OUT_DIR, { recursive: true });
fs.writeFileSync(`${OUT_DIR}/latest.json`, JSON.stringify(manifest, null, 2), 'utf8');
fs.copyFileSync(`${NSIS_DIR}/${NAME}`, `${OUT_DIR}/${NAME}`);

console.log('版本', manifest.version);
console.log('地址', manifest.platforms['windows-x86_64'].url);
console.log('签名长度', sig.length);
console.log('清单已写到', `${OUT_DIR}/latest.json`);