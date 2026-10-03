// PPM 图片加载与解析：与固件 `ppm_pixel_offset` 同逻辑，
// 嵌入顺序（按文件名字节序）与 `build.rs` 一致，索引一一对应。

// 导入全部 ppm（按文件名字节序排序，与固件 build.rs 嵌入顺序一致）
export function loadPpmFiles(): ArrayBuffer[] {
  return Object.entries(
    import.meta.glob('../assets/images/*.ppm', { query: '?arraybuffer', import: 'default', eager: true }),
  )
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([, buf]) => buf as ArrayBuffer);
}

export interface PpmHeader {
  width: number
  height: number
  offset: number
}

// P6 PPM 头部解析：返回像素数据起始偏移
export function parsePpmHeader(data: Uint8Array): PpmHeader | null {
  if (data.length < 2 || data[0] !== 0x50 || data[1] !== 0x36)
    return null;
  const isWs = (c: number) => c === 0x20 || c === 0x09 || c === 0x0A || c === 0x0D;
  let pos = 2;
  const readNum = (): number => {
    while (pos < data.length) {
      const c = data[pos];
      if (c === 0x23) { // '#' 注释行
        while (pos < data.length && data[pos] !== 0x0A)
          pos++;
      }
      else if (isWs(c)) {
        pos++;
      }
      else {
        break;
      }
    }
    const start = pos;
    while (pos < data.length && data[pos] >= 0x30 && data[pos] <= 0x39)
      pos++;
    if (start === pos)
      return -1;
    let n = 0;
    for (let i = start; i < pos; i++)
      n = n * 10 + data[i] - 0x30;
    return n;
  };
  const width = readNum();
  const height = readNum();
  const maxval = readNum();
  if (width <= 0 || height <= 0 || maxval !== 255)
    return null;
  // maxval 之后恰好一个空白字符即为像素数据
  if (pos + 1 + width * height * 3 > data.length)
    return null;
  return { width, height, offset: pos + 1 };
}

// 把 PPM 解码后画到 canvas 上（像素级 1:1，CSS 负责放大）；失败返回 false
export function drawPpmToCanvas(canvas: HTMLCanvasElement, buf: ArrayBuffer): boolean {
  const data = new Uint8Array(buf);
  const header = parsePpmHeader(data);
  if (!header)
    return false;
  const { width, height, offset } = header;
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d')!;
  const img = ctx.createImageData(width, height);
  for (let p = 0; p < width * height; p++) {
    img.data[p * 4 + 0] = data[offset + p * 3 + 0];
    img.data[p * 4 + 1] = data[offset + p * 3 + 1];
    img.data[p * 4 + 2] = data[offset + p * 3 + 2];
    img.data[p * 4 + 3] = 255;
  }
  ctx.putImageData(img, 0, 0);
  return true;
}
