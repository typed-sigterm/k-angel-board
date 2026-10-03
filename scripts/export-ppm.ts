import { Buffer } from 'node:buffer';
import * as fs from 'node:fs/promises';
import { glob } from 'node:fs/promises';
import * as path from 'node:path';
import { createCanvas } from '@napi-rs/canvas';
import { initializeCanvas, readPsd } from 'ag-psd';
import sharp from 'sharp';

initializeCanvas(createCanvas as any);

async function convertPsdToPpm(inputFolder: string, outputFolder: string) {
  const files = glob(`${inputFolder}/**/*.psd`);

  for await (const file of files) {
    const buffer = await fs.readFile(file);
    const psd = readPsd(buffer, { skipLayerImageData: true });
    if (!psd.canvas)
      throw new Error('Cannot get canvas');

    const relPath = path.relative(inputFolder, file).replace(/\.psd$/, '.ppm');
    const outputPath = path.join(outputFolder, relPath);

    await fs.mkdir(path.dirname(outputPath), { recursive: true });

    const ctx = psd.canvas.getContext('2d');
    if (!ctx)
      throw new Error('Cannot get 2D context');
    const imgData = ctx.getImageData(0, 0, psd.width, psd.height);

    // 1. 去除 Alpha 通道，输出纯 RGB 原始字节 (raw)
    const { data, info } = await sharp(Buffer.from(imgData.data.buffer), {
      raw: { width: psd.width, height: psd.height, channels: 4 },
    })
      .removeAlpha()
      .toFormat('raw')
      .toBuffer({ resolveWithObject: true });

    // 2. 构造 PPM (P6 二进制格式) 的标头 Header
    const header = Buffer.from(`P6\n${info.width} ${info.height}\n255\n`, 'ascii');

    // 3. 拼接标头与像素数据并写入文件
    const ppmData = Buffer.concat([header, data]);
    await fs.writeFile(outputPath, ppmData);
  }
}

convertPsdToPpm(
  path.resolve(import.meta.dirname, '../assets/design'),
  path.resolve(import.meta.dirname, '../assets/images'),
);
