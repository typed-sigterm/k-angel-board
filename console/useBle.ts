// Web Bluetooth 连接与命令收发：特征 UUID 与固件 `src/bin/main.rs` 保持一致。
// 状态通知经 `parseStatus` 解码后写入 ref，由调用方（App.vue）消费。
import { ref, shallowRef } from 'vue';
import { COMMAND_NAMES, Command, type ControlOpts, encodeControl, parseStatus } from './proto';
import { CHUNK_LEN, CONTROL_UUID, DEBUG_BYTES, DEVICE_NAME, SERVICE_UUID, STATUS_UUID } from './constants';

export interface LogFn {
  (msg: string): void
}

export function useBle(log: LogFn) {
  const connected = ref(false);
  const statusText = ref('未连接');
  const device = shallowRef<BluetoothDevice | null>(null);
  const controlChar = shallowRef<BluetoothRemoteGATTCharacteristic | null>(null);
  const statusChar = shallowRef<BluetoothRemoteGATTCharacteristic | null>(null);

  // 设备状态镜像（App.vue 用 computed 派生各面板显示）
  const autoMode = ref(false);
  const debugMode = ref(false);
  const brightness = ref(8);
  const panelIndex = ref<number[]>([0, 0, 0]);
  const lottery = ref<boolean[]>([false, false, false]);
  const imageCount = ref(0);

  // GATT 写串行化：Chrome 同一时间只允许一个 in-flight 写操作，
  // 并发 writeValueWithResponse 会抛 `NetworkError: GATT operation already in progress`。
  // 所有写（单条命令 / 整帧回传）都经这条 promise 链排队；busy 置灰 UI。
  const busy = ref(false);
  let writeQueue: Promise<void> = Promise.resolve();
  let pendingWrites = 0;

  function enqueueWrite<T>(fn: () => Promise<T>): Promise<T> {
    pendingWrites++;
    busy.value = true;
    const run = writeQueue.then(fn);
    // 链条本身永不断：失败只影响本次调用方，排队继续；
    // 队列排空才清 busy，避免闪烁
    writeQueue = run.then(
      () => { afterWrite(); },
      () => { afterWrite(); },
    );
    return run;
  }

  function afterWrite() {
    pendingWrites--;
    if (pendingWrites <= 0) {
      pendingWrites = 0;
      busy.value = false;
    }
  }

  // 调试帧确认等待：sendFrame 发完整帧后等固件 frame_seq 回显
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
      const s = parseStatus(bytes);
      autoMode.value = s.auto;
      debugMode.value = s.debug;
      brightness.value = s.brightness;
      panelIndex.value = s.panelIndex;
      lottery.value = s.lottery;
      if (s.frameSeq !== 0)
        resolveFrameAck(s.frameSeq);
      const n = imageCount.value;
      const tag = (p: number) => `${s.panelIndex[p] + 1}/${n}${s.lottery[p] ? '滚动' : ''}`;
      statusText.value = `已连接 | 板1：${tag(0)} | 板2：${tag(1)} | 板3：${tag(2)} | 自动：${s.auto ? '开' : '关'} | 调试：${s.debug ? '开' : '关'} | 亮度：${s.brightness}`;
    }
    catch (e) {
      log(`状态解析失败：${e}`);
    }
  }

  function statusBytesOf(view: DataView): Uint8Array {
    return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  }

  function onStatusChanged(e: Event) {
    const char = e.target as BluetoothRemoteGATTCharacteristic;
    if (!char.value)
      return;
    applyStatus(statusBytesOf(char.value));
  }

  function onDisconnected() {
    device.value = null;
    controlChar.value = null;
    statusChar.value = null;
    connected.value = false;
    statusText.value = '未连接';
    log('设备已断开');
  }

  async function connect() {
    if (!navigator.bluetooth) {
      statusText.value = '此浏览器不支持 Web Bluetooth（请用 Chrome/Edge）';
      return;
    }
    try {
      statusText.value = '正在请求设备…';
      const d = await navigator.bluetooth.requestDevice({
        filters: [{ name: DEVICE_NAME }],
        optionalServices: [SERVICE_UUID],
      });
      d.addEventListener('gattserverdisconnected', onDisconnected);
      device.value = d;
      statusText.value = '正在连接…';
      const server = await d.gatt!.connect();
      const service = await server.getPrimaryService(SERVICE_UUID);
      controlChar.value = await service.getCharacteristic(CONTROL_UUID);
      statusChar.value = await service.getCharacteristic(STATUS_UUID);
      await statusChar.value.startNotifications();
      statusChar.value.addEventListener('characteristicvaluechanged', onStatusChanged);
      try {
        // 固件在每次通知时都会同步更新特征存储值，直接读到的就是最新状态
        applyStatus(statusBytesOf(await statusChar.value.readValue()));
      }
      catch {
        log('读取初始状态失败，等待通知');
      }
      connected.value = true;
      statusText.value = '已连接';
      log(`已连接 ${d.name}`);
    }
    catch (e) {
      statusText.value = `连接失败：${e}`;
      log(String(e));
    }
  }

  function disconnect() {
    if (device.value?.gatt?.connected)
      device.value.gatt.disconnect();
    else
      void connect();
  }

  function describeCommand(cmd: number, opts: ControlOpts): string {
    if (cmd === Command.GOTO)
      return `(板${opts.panel ?? 0}, ${(opts.index ?? 0) + 1})`;
    if (cmd === Command.NEXT)
      return `(板${opts.panel ?? 0})`;
    if (cmd === Command.LOTTERY_START)
      return `(板${opts.panel}, ${opts.speed}行/秒)`;
    if (cmd === Command.LOTTERY_STOP)
      return `(板${opts.panel ?? 0})`;
    if (cmd === Command.SET_BRIGHTNESS)
      return `(${opts.brightness})`;
    return '';
  }

  async function sendCommand(cmd: number, opts: ControlOpts = {}) {
    if (!controlChar.value)
      return;
    const char = controlChar.value;
    try {
      // 经写队列串行化：回传 129 片长写期间点的命令会排队而非抢占抛错
      await enqueueWrite(async () => {
        // 拷贝为独占 ArrayBuffer 视图，满足 Web Bluetooth 的 BufferSource 类型
        await char.writeValueWithResponse(new Uint8Array(encodeControl(cmd, opts)));
      });
      log(`已发送命令：${COMMAND_NAMES[cmd] ?? cmd}${describeCommand(cmd, opts)}`);
    }
    catch (e) {
      log(`发送失败：${e}`);
    }
  }

  async function sendFrame(frame: Uint8Array) {
    if (!controlChar.value)
      return;
    const char = controlChar.value;
    // 768 字节 / 6 字节 = 128 片，逐片 writeWithResponse（<=13 字节，远小于 ATT 载荷上限）。
    // 固件攒齐整帧才刷屏并回 frame_seq 确认；收不到确认则整帧重传（至多 3 次）。
    // 整帧（含重传等待）包在一次排队里：期间其它命令排队等，不插片、不抢占。
    const seq = (frameSeqCounter = (frameSeqCounter + 1) >>> 0);
    await enqueueWrite(async () => {
      for (let attempt = 1; attempt <= 3; attempt++) {
        try {
          await char.writeValueWithResponse(
            new Uint8Array(encodeControl(Command.FRAME_SEQ, { index: seq })),
          );
          for (let offset = 0; offset < DEBUG_BYTES; offset += CHUNK_LEN) {
            const last = offset + CHUNK_LEN >= DEBUG_BYTES;
            const pixels = frame.slice(offset, offset + CHUNK_LEN);
            await char.writeValueWithResponse(
              new Uint8Array(encodeControl(Command.FRAME_CHUNK, { offset, pixels, last })),
            );
          }
        }
        catch (e) {
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
    }).catch(() => {
      // 队列串行化的写失败已在内部打过日志，这里吞掉避免未处理 rejection
    });
  }

  return {
    // 连接态
    connected,
    statusText,
    // 写队列忙：UI 置灰全部命令按钮（回传 129 片期间点别的会排队而非抢占）
    busy,
    // 设备状态镜像
    autoMode,
    debugMode,
    brightness,
    panelIndex,
    lottery,
    imageCount,
    // 动作
    connect,
    disconnect,
    sendCommand,
    sendFrame,
  };
}
