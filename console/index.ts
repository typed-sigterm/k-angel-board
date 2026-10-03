import * as protobuf from 'protobufjs';
import protoText from '../proto/display.proto?raw';
import './style.css';

// BLE 基本常量（UUID 与固件 src/bin/main.rs 保持一致）。
// 消息格式的唯一真实来源是 `proto/display.proto`：
// 固件侧用 prost 编解码，console 侧用 protobuf.js 编解码。
const DEVICE_NAME = 'k-angel-board';
const SERVICE_UUID = 0x9E00;
const CONTROL_UUID = 0x9E01;
const STATUS_UUID = 0x9E02;

// ---------- protobuf ----------
const protoRoot = protobuf.parse(protoText).root;
const ControlMessage = protoRoot.lookupType('kangel.display.Control');
const StatusMessage = protoRoot.lookupType('kangel.display.Status');
const CommandEnum = protoRoot.lookupEnum('kangel.display.Command');
const Command = {
  NEXT: CommandEnum.values.NEXT,
  AUTO_ON: CommandEnum.values.AUTO_ON,
  AUTO_OFF: CommandEnum.values.AUTO_OFF,
  TOGGLE: CommandEnum.values.TOGGLE,
  GOTO: CommandEnum.values.GOTO,
  DEBUG_ENTER: CommandEnum.values.DEBUG_ENTER,
  DEBUG_EXIT: CommandEnum.values.DEBUG_EXIT,
  SET_BRIGHTNESS: CommandEnum.values.SET_BRIGHTNESS,
  FRAME_CHUNK: CommandEnum.values.FRAME_CHUNK,
  FRAME_SEQ: CommandEnum.values.FRAME_SEQ,
  LOTTERY_START: CommandEnum.values.LOTTERY_START,
  LOTTERY_STOP: CommandEnum.values.LOTTERY_STOP,
};
const COMMAND_NAMES = Object.fromEntries(
  Object.entries(CommandEnum.valuesById).map(([id, name]) => [Number(id), name]),
) as Record<number, string>;

// 调试帧：16x16 RGB888；分片 6 字节 + protobuf 开销 <= 13 字节，
// 远小于 ATT 默认 20 字节载荷（18~19 字节写入在部分 BLE 栈会被截断，
// 导致颜色错位 / 噪点），无需 MTU 协商
const DEBUG_W = 16;
const DEBUG_H = 16;
const DEBUG_BYTES = DEBUG_W * DEBUG_H * 3;
const CHUNK_LEN = 6;
const MAX_SAFE_BRIGHTNESS = 255;

interface ControlOpts {
  index?: number
  brightness?: number
  offset?: number
  pixels?: Uint8Array
  last?: boolean
  panel?: number
  speed?: number
  lotteryMask?: Uint8Array
}

function encodeControl(command: number, opts: ControlOpts = {}): Uint8Array {
  const { index = 0, brightness = 0, offset = 0, pixels = new Uint8Array(0), last = false, panel = 0, speed = 0, lotteryMask = new Uint8Array(0) } = opts;
  const error = ControlMessage.verify({ command, index, brightness, offset, pixels, last, panel, speed, lotteryMask });
  if (error)
    throw new Error(`非法控制命令：${error}`);
  return ControlMessage.encode({ command, index, brightness, offset, pixels, last, panel, speed, lotteryMask }).finish();
}

interface DeviceStatus {
  index: number
  auto: boolean
  debug: boolean
  brightness: number
  frameSeq: number
  panelIndex: number[]
  lottery: boolean[]
}

function parseStatus(bytes: Uint8Array): DeviceStatus {
  const msg = StatusMessage.decode(bytes) as unknown as Record<string, unknown>;
  const index = (msg.index as number) ?? 0;
  // 老固件没有 panelIndex/lottery 字段：三块板都按 index 高亮、无滚动标志
  const raw = msg.panelIndex as number[] | undefined;
  const panelIndex = Array.isArray(raw) && raw.length > 0
    ? [raw[0] ?? index, raw[1] ?? index, raw[2] ?? index]
    : [index, index, index];
  const rawLot = msg.lottery as boolean[] | undefined;
  const lottery = Array.isArray(rawLot) && rawLot.length > 0
    ? [!!rawLot[0], !!rawLot[1], !!rawLot[2]]
    : [false, false, false];
  return {
    index,
    auto: (msg.auto as boolean) ?? false,
    debug: (msg.debug as boolean) ?? false,
    brightness: (msg.brightness as number) ?? 0,
    frameSeq: (msg.frameSeq as number) ?? 0,
    panelIndex,
    lottery,
  };
}

function statusBytesOf(view: DataView): Uint8Array {
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

// 导入全部 ppm（按文件名字节序排序，与固件 build.rs 嵌入顺序一致）
const ppmFiles = Object.entries(
  import.meta.glob('../assets/images/*.ppm', { query: '?arraybuffer', import: 'default', eager: true }),
)
  .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
  .map(([, buf]) => buf as ArrayBuffer);

// P6 PPM 头部解析（与固件 ppm_pixel_offset 同逻辑）：返回像素数据起始偏移
function parsePpmHeader(data: Uint8Array): { width: number, height: number, offset: number } | null {
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
      } else if (isWs(c)) {
        pos++;
      } else {
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

// ---------- UI 构建 ----------
const app = document.querySelector<HTMLDivElement>('#app')!;
app.innerHTML = `
  <h1>
    ${DEVICE_NAME} 灯板控制台
  </h1>
  <p>
    状态：<span id="status-text">未连接</span>
    <button id="btn-connect">连接设备</button>
  </p>
  <div id="panel">
    <p>
      <button id="btn-auto" disabled>启动/停止自动切换</button>
      <button id="btn-next-all" disabled>三块一起下一张</button>
      <button id="btn-next-1" disabled>第 1 块下一张</button>
      <button id="btn-next-2" disabled>第 2 块下一张</button>
      <button id="btn-next-3" disabled>第 3 块下一张</button>
    </p>
    <p>
      <label for="brightness">全局亮度（0-${MAX_SAFE_BRIGHTNESS}，图片与调试帧通用）：</label>
      <input id="brightness" type="range" min="0" max="${MAX_SAFE_BRIGHTNESS}" value="8" disabled />
      <span id="brightness-val">8</span>
      <button id="btn-brightness" disabled>设置亮度</button>
    </p>
    <p>目标灯板：
      <label><input type="radio" name="panel" value="0" checked />三块一起</label>
      <label><input type="radio" name="panel" value="1" />第 1 块</label>
      <label><input type="radio" name="panel" value="2" />第 2 块</label>
      <label><input type="radio" name="panel" value="3" />第 3 块</label>
    </p>
    <p>点击图片 = 所选灯板跳到该图片（选「三块一起」则三块同显所点图片）</p>
    <div id="grid"></div>
    <fieldset id="lottery-panel">
      <legend>抽奖滚动（每块板独立选图，统一速度，垂直滚动）</legend>
      <p>
        <label for="lottery-speed">滚动速度（1-120 行/秒，16 行=滚过一张图）：</label>
        <input id="lottery-speed" type="range" min="1" max="120" value="16" disabled />
        <span id="lottery-speed-val">16</span>
      </p>
      <p>
        <button id="btn-lottery-1" disabled>第 1 块开始/停止</button>
        <button id="btn-lottery-2" disabled>第 2 块开始/停止</button>
        <button id="btn-lottery-3" disabled>第 3 块开始/停止</button>
        <button id="btn-lottery-stop-all" disabled>三块全停</button>
        <span id="lottery-state">未开始</span>
      </p>
      <p class="hint">每张预览图下的 板1/板2/板3 勾选 = 该板奖池；点「开始」滚动，点「停止」立即停在当前画面。滚动中该板暂停自动轮播，点下一张/点图/开关自动会先停该板滚动。</p>
    </fieldset>
    <fieldset id="debug-panel">
      <legend>调试模式（点阵图回传）</legend>
      <p>
        <button id="btn-debug" disabled>进入调试模式</button>
        <span id="debug-state">未进入调试</span>
      </p>
      <p>
        <canvas id="debug-canvas" width="${DEBUG_W}" height="${DEBUG_H}"></canvas>
      </p>
      <p>
        <button id="btn-clear" disabled>清空调色板</button>
        <button id="btn-fill-red" disabled>全红</button>
        <button id="btn-fill-green" disabled>全绿</button>
        <button id="btn-fill-blue" disabled>全白</button>
        <button id="btn-fill-pure-blue" disabled>全蓝</button>
        <button id="btn-send-frame" disabled>回传显示</button>
      </p>
      <p class="hint">点击调色板格子切换颜色，再点「回传显示」把 16x16 点阵图分片发到灯板直显（自动重传直到灯板确认）。</p>
    </fieldset>
    <pre id="log"></pre>
  </div>
`;

const btnConnect = document.querySelector<HTMLButtonElement>('#btn-connect')!;
const btnAuto = document.querySelector<HTMLButtonElement>('#btn-auto')!;
const btnNextAll = document.querySelector<HTMLButtonElement>('#btn-next-all')!;
const btnNext1 = document.querySelector<HTMLButtonElement>('#btn-next-1')!;
const btnNext2 = document.querySelector<HTMLButtonElement>('#btn-next-2')!;
const btnNext3 = document.querySelector<HTMLButtonElement>('#btn-next-3')!;
const btnLottery1 = document.querySelector<HTMLButtonElement>('#btn-lottery-1')!;
const btnLottery2 = document.querySelector<HTMLButtonElement>('#btn-lottery-2')!;
const btnLottery3 = document.querySelector<HTMLButtonElement>('#btn-lottery-3')!;
const btnLotteryStopAll = document.querySelector<HTMLButtonElement>('#btn-lottery-stop-all')!;
const lotterySpeed = document.querySelector<HTMLInputElement>('#lottery-speed')!;
const lotterySpeedVal = document.querySelector<HTMLSpanElement>('#lottery-speed-val')!;
const lotteryState = document.querySelector<HTMLSpanElement>('#lottery-state')!;
const btnDebug = document.querySelector<HTMLButtonElement>('#btn-debug')!;
const btnBrightness = document.querySelector<HTMLButtonElement>('#btn-brightness')!;
const btnSendFrame = document.querySelector<HTMLButtonElement>('#btn-send-frame')!;
const btnClear = document.querySelector<HTMLButtonElement>('#btn-clear')!;
const btnFillRed = document.querySelector<HTMLButtonElement>('#btn-fill-red')!;
const btnFillGreen = document.querySelector<HTMLButtonElement>('#btn-fill-green')!;
const btnFillBlue = document.querySelector<HTMLButtonElement>('#btn-fill-blue')!;
const btnFillPureBlue = document.querySelector<HTMLButtonElement>('#btn-fill-pure-blue')!;
const brightnessInput = document.querySelector<HTMLInputElement>('#brightness')!;
const brightnessVal = document.querySelector<HTMLSpanElement>('#brightness-val')!;
const debugState = document.querySelector<HTMLSpanElement>('#debug-state')!;
const debugCanvas = document.querySelector<HTMLCanvasElement>('#debug-canvas')!;
const statusText = document.querySelector<HTMLSpanElement>('#status-text')!;
const grid = document.querySelector<HTMLDivElement>('#grid')!;
const logEl = document.querySelector<HTMLPreElement>('#log')!;

function log(msg: string) {
  logEl.textContent += `${msg}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}

function setStatus(text: string) {
  statusText.textContent = text;
}

// 图片预览：16x16 canvas + 每板奖池勾选，CSS 放大渲染。
// 三块灯板各一张预览图不好排：用粉色边框同时标出三块板各自的当前图片
// （三块同图时只看到一张被框住，三块不同图时看到三张被框住）。
// 滚动中的板：边框变虚线跑马灯样式（CSS 类 lottery），区别于静止高亮。
const canvases: HTMLCanvasElement[] = [];
const lotteryChecks: HTMLInputElement[][] = [];
function highlight(panelIndex: number[], lottery: boolean[]) {
  canvases.forEach((c, i) => {
    c.classList.toggle('current', panelIndex.includes(i));
  });
  for (let p = 0; p < 3; p++) {
    canvases.forEach((c, i) => {
      c.classList.toggle(`lottery-p${p + 1}`, lottery[p] && panelIndex[p] === i);
    });
  }
  const rolling = lottery.map((on, p) => on ? `板${p + 1}` : '').filter(Boolean).join(' ');
  lotteryState.textContent = rolling ? `滚动中：${rolling}` : '未开始';
  btnLottery1.textContent = lottery[0] ? '第 1 块停止' : '第 1 块开始';
  btnLottery2.textContent = lottery[1] ? '第 2 块停止' : '第 2 块开始';
  btnLottery3.textContent = lottery[2] ? '第 3 块停止' : '第 3 块开始';
}

function selectedPanel(): number {
  const checked = document.querySelector<HTMLInputElement>('input[name="panel"]:checked');
  return checked ? Number(checked.value) : 0;
}

ppmFiles.forEach((buf, index) => {
  const data = new Uint8Array(buf);
  const header = parsePpmHeader(data);
  const wrap = document.createElement('div');
  wrap.className = 'thumb';
  const canvas = document.createElement('canvas');
  if (!header) {
    canvas.width = 16;
    canvas.height = 16;
    canvas.title = 'PPM 解析失败';
  } else {
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
    canvas.title = `点击跳转到第 ${index + 1} 张`;
  }
  canvas.addEventListener('click', () => sendCommand(Command.GOTO, { index, panel: selectedPanel() }));
  wrap.appendChild(canvas);
  // 奖池勾选：每张图下 板1/板2/板3 三个框，默认全不选
  const checks: HTMLInputElement[] = [];
  const pick = document.createElement('div');
  pick.className = 'pick';
  for (let p = 1; p <= 3; p++) {
    const label = document.createElement('label');
    const box = document.createElement('input');
    box.type = 'checkbox';
    box.checked = false;
    box.title = `第 ${p} 块板奖池${index + 1}`;
    label.append(box, `板${p}`);
    pick.appendChild(label);
    checks.push(box);
  }
  wrap.appendChild(pick);
  grid.appendChild(wrap);
  canvases.push(canvas);
  lotteryChecks.push(checks);
});

// 奖池位图：选中图序号 -> bit 置位（bit i = 第 i 张图），96 张图内单次写入装得下
function lotteryMaskOf(panel: number): Uint8Array | null {
  if (ppmFiles.length > 96) {
    log(`图片共 ${ppmFiles.length} 张，超过单次写入上限 96 张，抽奖拒绝发送`);
    return null;
  }
  const mask = new Uint8Array(Math.ceil(ppmFiles.length / 8));
  let count = 0;
  lotteryChecks.forEach((checks, i) => {
    if (checks[panel - 1]?.checked) {
      mask[i >> 3] |= 1 << (i & 7);
      count++;
    }
  });
  if (count === 0) {
    log(`第 ${panel} 块板奖池为空：请至少勾选一张图`);
    return null;
  }
  return mask;
}

async function toggleLottery(panel: number) {
  if (lotteryRunning[panel - 1]) {
    // 本地状态认为滚动中：发停止（固件回状态后以固件为准刷新按钮）
    await sendCommand(Command.LOTTERY_STOP, { panel });
  } else {
    const mask = lotteryMaskOf(panel);
    if (!mask)
      return;
    await sendCommand(Command.LOTTERY_START, { panel, speed: Number(lotterySpeed.value), lotteryMask: mask });
  }
}

// ---------- 调试点阵编辑器 ----------
const debugFrame = new Uint8Array(DEBUG_BYTES);
let brush: [number, number, number] = [255, 0, 0];

function renderDebugCanvas() {
  const ctx = debugCanvas.getContext('2d')!;
  const img = ctx.createImageData(DEBUG_W, DEBUG_H);
  for (let p = 0; p < DEBUG_W * DEBUG_H; p++) {
    img.data[p * 4 + 0] = debugFrame[p * 3 + 0];
    img.data[p * 4 + 1] = debugFrame[p * 3 + 1];
    img.data[p * 4 + 2] = debugFrame[p * 3 + 2];
    img.data[p * 4 + 3] = 255;
  }
  ctx.putImageData(img, 0, 0);
}

function fillDebug(r: number, g: number, b: number) {
  for (let p = 0; p < DEBUG_W * DEBUG_H; p++) {
    debugFrame[p * 3 + 0] = r;
    debugFrame[p * 3 + 1] = g;
    debugFrame[p * 3 + 2] = b;
  }
  renderDebugCanvas();
}

function togglePixel(px: number, py: number) {
  const off = (py * DEBUG_W + px) * 3;
  const isOn = debugFrame[off] !== 0 || debugFrame[off + 1] !== 0 || debugFrame[off + 2] !== 0;
  const [r, g, b] = isOn ? [0, 0, 0] : brush;
  debugFrame[off] = r;
  debugFrame[off + 1] = g;
  debugFrame[off + 2] = b;
  renderDebugCanvas();
}

debugCanvas.addEventListener('click', (e) => {
  const rect = debugCanvas.getBoundingClientRect();
  const px = Math.min(DEBUG_W - 1, Math.max(0, Math.floor((e.clientX - rect.left) / rect.width * DEBUG_W)));
  const py = Math.min(DEBUG_H - 1, Math.max(0, Math.floor((e.clientY - rect.top) / rect.height * DEBUG_H)));
  togglePixel(px, py);
});

renderDebugCanvas();

// ---------- Web Bluetooth ----------
let device: BluetoothDevice | null = null;
let controlChar: BluetoothRemoteGATTCharacteristic | null = null;
let statusChar: BluetoothRemoteGATTCharacteristic | null = null;
let autoMode = false;
let debugMode = false;
// 本地滚动标志：按钮文案即时翻转用；以固件 Status.lottery 回显为准刷新
let lotteryRunning = [false, false, false];

async function sendCommand(cmd: number, opts: ControlOpts = {}) {
  if (!controlChar)
    return;
  try {
    // 拷贝为独占 ArrayBuffer 视图，满足 Web Bluetooth 的 BufferSource 类型
    await controlChar.writeValueWithResponse(new Uint8Array(encodeControl(cmd, opts)));
    const extra = cmd === Command.GOTO
      ? `(板${opts.panel ?? 0}, ${(opts.index ?? 0) + 1})`
      : cmd === Command.NEXT
        ? `(板${opts.panel ?? 0})`
        : cmd === Command.LOTTERY_START
          ? `(板${opts.panel}, ${opts.speed}行/秒)`
          : cmd === Command.LOTTERY_STOP
            ? `(板${opts.panel ?? 0})`
            : cmd === Command.SET_BRIGHTNESS
        ? `(${opts.brightness})`
        : '';
    log(`已发送命令：${COMMAND_NAMES[cmd] ?? cmd}${extra}`);
  } catch (e) {
    log(`发送失败：${e}`);
  }
}

async function sendFrame() {
  if (!controlChar)
    return;
  // 768 字节 / 6 字节 = 128 片，逐片 writeWithResponse（<=13 字节，远小于 ATT 载荷上限）。
  // 固件攒齐整帧才刷屏并回 frame_seq 确认；收不到确认则整帧重传（至多 3 次）。
  const seq = (frameSeqCounter = (frameSeqCounter + 1) >>> 0);
  for (let attempt = 1; attempt <= 3; attempt++) {
    try {
      await controlChar.writeValueWithResponse(
        new Uint8Array(encodeControl(Command.FRAME_SEQ, { index: seq })),
      );
      for (let offset = 0; offset < DEBUG_BYTES; offset += CHUNK_LEN) {
        const last = offset + CHUNK_LEN >= DEBUG_BYTES;
        const pixels = debugFrame.slice(offset, offset + CHUNK_LEN);
        await controlChar.writeValueWithResponse(
          new Uint8Array(encodeControl(Command.FRAME_CHUNK, { offset, pixels, last })),
        );
      }
    } catch (e) {
      log(`回传失败（第 ${attempt} 次，发送中出错）：${e}`);
      continue;
    }
    // 等固件攒齐整帧后的状态确认（frame_seq 回显）
    const ok = await waitForFrameAck(seq, 3000);
    if (ok) {
      log(`调试帧 #${seq} 已回传确认（${DEBUG_BYTES} 字节 / ${Math.ceil(DEBUG_BYTES / CHUNK_LEN)} 片）`);
      return;
    }
    log(`调试帧 #${seq} 未收到确认（第 ${attempt} 次），重传整帧…`);
  }
  log(`调试帧 #${seq} 重传 3 次仍无确认，请检查连接后重试`);
}

let frameSeqCounter = 0;
let ackWaiters: Array<{ seq: number, resolve: (ok: boolean) => void }> = [];

function waitForFrameAck(seq: number, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const waiter = { seq, resolve };
    ackWaiters.push(waiter);
    setTimeout(() => {
      const i = ackWaiters.indexOf(waiter);
      if (i >= 0) {
        ackWaiters.splice(i, 1);
        resolve(false);
      }
    }, timeoutMs);
  });
}

function resolveFrameAck(seq: number) {
  for (let i = ackWaiters.length - 1; i >= 0; i--) {
    if (ackWaiters[i].seq === seq) {
      const [w] = ackWaiters.splice(i, 1);
      w.resolve(true);
    }
  }
}

function applyStatus(bytes: Uint8Array) {
  try {
    const { auto, debug, brightness, frameSeq, panelIndex, lottery } = parseStatus(bytes);
    autoMode = auto;
    debugMode = debug;
    lotteryRunning = [...lottery] as [boolean, boolean, boolean];
    if (frameSeq !== 0)
      resolveFrameAck(frameSeq);
    setStatus(`已连接 | 板1：${panelIndex[0] + 1}/${ppmFiles.length}${lottery[0] ? '滚动' : ''} | 板2：${panelIndex[1] + 1}/${ppmFiles.length}${lottery[1] ? '滚动' : ''} | 板3：${panelIndex[2] + 1}/${ppmFiles.length}${lottery[2] ? '滚动' : ''} | 自动：${auto ? '开' : '关'} | 调试：${debug ? '开' : '关'} | 亮度：${brightness}`);
    highlight(panelIndex, lottery);
    debugState.textContent = debug ? '调试中（轮播已暂停）' : '未进入调试';
    btnDebug.textContent = debug ? '退出调试模式' : '进入调试模式';
    if (brightness !== Number(brightnessInput.value)) {
      brightnessInput.value = String(Math.min(MAX_SAFE_BRIGHTNESS, brightness));
      brightnessVal.textContent = String(brightness);
    }
  } catch (e) {
    log(`状态解析失败：${e}`);
  }
}

function setDebugButtons(enabled: boolean) {
  btnDebug.disabled = !enabled;
  btnSendFrame.disabled = !enabled;
  btnClear.disabled = !enabled;
  btnFillRed.disabled = !enabled;
  btnFillGreen.disabled = !enabled;
  btnFillBlue.disabled = !enabled;
  btnFillPureBlue.disabled = !enabled;
  // 亮度是全局设置：连接后即可调，不随调试面板开关
  btnBrightness.disabled = !enabled;
  brightnessInput.disabled = !enabled;
}

function setLotteryButtons(enabled: boolean) {
  btnLottery1.disabled = !enabled;
  btnLottery2.disabled = !enabled;
  btnLottery3.disabled = !enabled;
  btnLotteryStopAll.disabled = !enabled;
  lotterySpeed.disabled = !enabled;
}

function onStatusChanged(e: Event) {
  const char = e.target as BluetoothRemoteGATTCharacteristic;
  if (!char.value)
    return;
  applyStatus(statusBytesOf(char.value));
}

function onDisconnected() {
  device = null;
  controlChar = null;
  statusChar = null;
  btnConnect.textContent = '连接设备';
  btnAuto.disabled = true;
  btnNextAll.disabled = true;
  btnNext1.disabled = true;
  btnNext2.disabled = true;
  btnNext3.disabled = true;
  setLotteryButtons(false);
  setDebugButtons(false);
  setStatus('未连接');
  log('设备已断开');
}

async function connect() {
  if (!navigator.bluetooth) {
    setStatus('此浏览器不支持 Web Bluetooth（请用 Chrome/Edge）');
    return;
  }
  try {
    setStatus('正在请求设备…');
    device = await navigator.bluetooth.requestDevice({
      filters: [{ name: DEVICE_NAME }],
      optionalServices: [SERVICE_UUID],
    });
    device.addEventListener('gattserverdisconnected', onDisconnected);
    setStatus('正在连接…');
    const server = await device.gatt!.connect();
    const service = await server.getPrimaryService(SERVICE_UUID);
    controlChar = await service.getCharacteristic(CONTROL_UUID);
    statusChar = await service.getCharacteristic(STATUS_UUID);
    await statusChar.startNotifications();
    statusChar.addEventListener('characteristicvaluechanged', onStatusChanged);
    try {
      // 固件在每次通知时都会同步更新特征存储值，直接读到的就是最新状态
      applyStatus(statusBytesOf(await statusChar.readValue()));
    } catch {
      log('读取初始状态失败，等待通知');
    }
    btnConnect.textContent = '断开连接';
    btnAuto.disabled = false;
    btnNextAll.disabled = false;
    btnNext1.disabled = false;
    btnNext2.disabled = false;
    btnNext3.disabled = false;
    setLotteryButtons(true);
    setDebugButtons(true);
    setStatus('已连接');
    log(`已连接 ${device.name}`);
  } catch (e) {
    setStatus(`连接失败：${e}`);
    log(String(e));
  }
}

btnConnect.addEventListener('click', () => {
  if (device?.gatt?.connected) {
    device.gatt.disconnect();
  } else {
    void connect();
  }
});

btnAuto.addEventListener('click', () => {
  void sendCommand(autoMode ? Command.AUTO_OFF : Command.AUTO_ON);
});

btnNextAll.addEventListener('click', () => {
  void sendCommand(Command.NEXT, { panel: 0 });
});

btnNext1.addEventListener('click', () => {
  void sendCommand(Command.NEXT, { panel: 1 });
});

btnNext2.addEventListener('click', () => {
  void sendCommand(Command.NEXT, { panel: 2 });
});

btnNext3.addEventListener('click', () => {
  void sendCommand(Command.NEXT, { panel: 3 });
});

lotterySpeed.addEventListener('input', () => {
  lotterySpeedVal.textContent = lotterySpeed.value;
});

btnLottery1.addEventListener('click', () => {
  void toggleLottery(1);
});

btnLottery2.addEventListener('click', () => {
  void toggleLottery(2);
});

btnLottery3.addEventListener('click', () => {
  void toggleLottery(3);
});

btnLotteryStopAll.addEventListener('click', () => {
  void sendCommand(Command.LOTTERY_STOP, { panel: 0 });
});

btnDebug.addEventListener('click', () => {
  void sendCommand(debugMode ? Command.DEBUG_EXIT : Command.DEBUG_ENTER);
});

brightnessInput.addEventListener('input', () => {
  brightnessVal.textContent = brightnessInput.value;
});

btnBrightness.addEventListener('click', () => {
  void sendCommand(Command.SET_BRIGHTNESS, { brightness: Number(brightnessInput.value) });
});

btnClear.addEventListener('click', () => {
  fillDebug(0, 0, 0);
});

btnFillRed.addEventListener('click', () => {
  brush = [255, 0, 0];
  fillDebug(255, 0, 0);
});

btnFillGreen.addEventListener('click', () => {
  brush = [0, 255, 0];
  fillDebug(0, 255, 0);
});

btnFillBlue.addEventListener('click', () => {
  brush = [255, 255, 255];
  fillDebug(255, 255, 255);
});

btnFillPureBlue.addEventListener('click', () => {
  brush = [0, 0, 255];
  fillDebug(0, 0, 255);
});

btnSendFrame.addEventListener('click', () => {
  void sendFrame();
});
