// 生成应用图标（一次性的构建工具，不参与运行时）
//
// 为什么图标要「写脚本生成」而不是手工做一张图丢进去：
// 要同时输出 PNG（Windows/Linux 用）、ICO（Windows 用），
// 还得覆盖 16 到 256 好几个尺寸，保证任务栏、开始菜单、Alt-Tab 里都清晰。
// 手工作一遍做不到这么整齐，而且下次想调颜色还要重来。
//
// 配色是从 public/index.html 的 --accent 和 .brand-mark 抄的：
//   linear-gradient(135deg, #6d8bff, #4f6ef7)，圆角方块
// 这样桌面图标和界面里的品牌块是同一套视觉，不是两套东西。
//
// 用法：node desktop/tools/make-icons.js
// 产物：desktop/src-tauri/icons/ 下的各个尺寸

const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

const OUT = path.join(__dirname, '..', 'src-tauri', 'icons');

// ---- 配色（对应 index.html 的 --accent / .brand-mark）----
const C_FROM = [0x6d, 0x8b, 0xff]; // 渐变起点
const C_TO = [0x4f, 0x6e, 0xf7]; // 渐变终点（--accent）
const SS = 4; // 超采样倍数：每像素算 4x4 再平均，边缘才不会有锯齿

// ============================================================================
// PNG 编码
// ============================================================================

let crcTable = null;
function crc32(buf) {
  if (!crcTable) {
    crcTable = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c;
    }
  }
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

function encodePng(width, height, rgba) {
  const stride = width * 4;
  // 每行前面加一个 0（filter 类型 = None）。做压缩时这一整块一起压，
  // 比逐行压更简单，图标这么小，体积差别可以忽略。
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0;
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // 位深 8
  ihdr[9] = 6; // 颜色类型 6 = RGBA
  // 10/11/12 是压缩/滤波/隔行方式，0 = 默认

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ============================================================================
// 画图标本体
// ============================================================================

// 点到圆角矩形的有符号距离。
// SDF（signed distance field）的用处：< 0 在形状内，> 0 在形状外，
// 而且边缘附近的值是「还差多少距离」，正好可以拿来算半透明的过渡带，
// 于是不用开超采样也能得到平滑边缘。
function sdRoundRect(px, py, halfW, halfH, r) {
  const qx = Math.abs(px) - halfW + r;
  const qy = Math.abs(py) - halfH + r;
  const ax = Math.max(qx, 0);
  const ay = Math.max(qy, 0);
  return Math.sqrt(ax * ax + ay * ay) + Math.min(Math.max(qx, qy), 0) - r;
}

// 抗锯齿：把距离换算成覆盖率。
// 用 clamp 而不是 if，是为了让边缘落在形状外的像素也能被部分着色。
function coverage(dist) {
  return Math.min(Math.max(0.5 - dist, 0), 1);
}

function drawIcon(size) {
  const rgba = Buffer.alloc(size * size * 4);
  // 圆角半径取尺寸的 22%：跟界面上 8px/26px ≈ 31% 接近，
  // 但小图标上圆角太大会显得「软」，22% 是任务栏尺寸下看着比较精神的值。
  const radius = size * 0.22;

  // 三条横线 = 笔记的直观隐喻。比画个书本好认，缩到 16px 也还看得出是「有内容的东西」。
  // y 坐标是相对**图标顶部**的比例，所以 0.5 正好在正中；
  // 下面用的时候要减去 0.5 转成相对中心的偏移 —— 这个转换忘了就会整组线往下掉。
  const lines = [
    { y: 0.38, halfW: 0.235, thick: 0.042 },
    { y: 0.50, halfW: 0.235, thick: 0.042 },
    { y: 0.62, halfW: 0.145, thick: 0.042 }, // 最后一条短一点，像没写完
  ];
  const linesCentered = lines.map((l) => ({ ...l, dy: l.y - 0.5 }));

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      // 先按超采样把每个子像素的颜色平均，得到这一格的 RGBA
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const fx = x + (sx + 0.5) / SS;
          const fy = y + (sy + 0.5) / SS;
          // 以图标中心为原点，方便算对称
          const px = fx - size / 2;
          const py = fy - size / 2;

          const d = sdRoundRect(px, py, size / 2, size / 2, radius);
          const cov = coverage(d);
          if (cov <= 0) continue;

          // 135deg 渐变 = 从左上到右下。
          // CSS 里 135deg 的方向向量在屏幕坐标系下就是 (1,1)/√2，
          // 所以用 (fx + fy) / (2*size) 归一化即可，和 CSS 渲染出来的一致。
          const t = Math.min(Math.max((fx + fy) / (2 * size), 0), 1);
          let cr = C_FROM[0] + (C_TO[0] - C_FROM[0]) * t;
          let cg = C_FROM[1] + (C_TO[1] - C_FROM[1]) * t;
          let cb = C_FROM[2] + (C_TO[2] - C_FROM[2]) * t;
          const dy = py / size;

          // 三条白线：各自也是一个圆角矩形，叠在渐变底上
          for (const ln of linesCentered) {
            // px/size 和 dy 都是「相对中心的比例」，减去线的中心偏移
            // 就得到「相对这条线中心的偏移」，正好喂给 sdRoundRect
            const dl = sdRoundRect(px / size, dy - ln.dy, ln.halfW, ln.thick / 2, ln.thick / 2);
            const lc = coverage(dl * size); // SDF 的单位是「图标尺寸的比例」，换回像素
            if (lc > 0) {
              const k = lc * cov; // 乘上底色的覆盖率：线不能超出圆角方块
              cr = cr * (1 - k) + 255 * k;
              cg = cg * (1 - k) + 255 * k;
              cb = cb * (1 - k) + 255 * k;
            }
          }

          r += cr * cov;
          g += cg * cov;
          b += cb * cov;
          a += cov;
        }
      }

      const n = SS * SS;
      const i = (y * size + x) * 4;
      // RGB 要除以 alpha 再写 —— PNG 存的是非预乘颜色，
      // 直接把累加值丢进去会让半透明边缘发黑（透明像素的 RGB 是 0，会把颜色拉暗）。
      const alpha = a / n;
      rgba[i] = alpha > 0 ? Math.round(r / a) : 0;
      rgba[i + 1] = alpha > 0 ? Math.round(g / a) : 0;
      rgba[i + 2] = alpha > 0 ? Math.round(b / a) : 0;
      rgba[i + 3] = Math.round(alpha * 255);
    }
  }
  return rgba;
}

// ============================================================================
// ICO 封装
// ============================================================================
//
// .ico 是个容器格式，本身不含图像，就是一个目录 + 若干张图的二进制。
// 好消息：Vista 之后的 Windows 允许 ICO 内部直接放 PNG，
// 所以不用再把图转成 BMP（带 alpha 的 BMP 又多又乱），把 PNG 拼进去就行。
//
// 结构：
//   ICONDIR      6 字节：保留位(2)=0、类型(2)=1、图标数量(2)
//   ICONDIRENTRY 每张图 16 字节：宽(1) 高(1) 调色板(1) 保留(1)
//                      平面数(2)=1 位深(2)=32 字节数(4) 偏移(4)
//   图像数据     依次排列的 PNG
//
// 宽和高各占 1 字节，256 存不下（最大 255），所以规定：写 0 表示 256。

function buildIco(entries) {
  const count = entries.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(count, 4);

  const dir = Buffer.alloc(16 * count);
  let offset = 6 + 16 * count;
  entries.forEach((e, i) => {
    const base = i * 16;
    dir[base] = e.size >= 256 ? 0 : e.size; // 256 要写成 0
    dir[base + 1] = e.size >= 256 ? 0 : e.size;
    dir[base + 2] = 0; // 调色板数：真彩色写 0
    dir[base + 3] = 0; // 保留
    dir.writeUInt16LE(1, base + 4); // 平面数
    dir.writeUInt16LE(32, base + 6); // 每像素位数
    dir.writeUInt32LE(e.png.length, base + 8);
    dir.writeUInt32LE(offset, base + 12);
    offset += e.png.length;
  });

  return Buffer.concat([header, dir, ...entries.map((e) => e.png)]);
}

// ============================================================================
// 输出
// ============================================================================

fs.mkdirSync(OUT, { recursive: true });

const sizes = [16, 32, 48, 64, 128, 256];
const pngs = {};
for (const s of sizes) {
  const rgba = drawIcon(s);
  pngs[s] = encodePng(s, s, rgba);
}

// Tauri 配置文件里引用的那几个名字必须一字不差
fs.writeFileSync(path.join(OUT, '32x32.png'), pngs[32]);
fs.writeFileSync(path.join(OUT, '128x128.png'), pngs[128]);
fs.writeFileSync(path.join(OUT, '128x128@2x.png'), pngs[256]);
fs.writeFileSync(path.join(OUT, 'icon.png'), pngs[512] || pngs[256]);
for (const s of sizes) {
  fs.writeFileSync(path.join(OUT, `icon-${s}x${s}.png`), pngs[s]);
}
fs.writeFileSync(path.join(OUT, 'icon.ico'), buildIco(sizes.map((s) => ({ size: s, png: pngs[s] }))));

console.log('图标已生成：');
for (const f of fs.readdirSync(OUT).sort()) {
  const st = fs.statSync(path.join(OUT, f));
  console.log(`  ${f.padEnd(20)} ${String(st.size).padStart(7)} 字节`);
}
