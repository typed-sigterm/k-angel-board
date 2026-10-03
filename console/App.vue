<script setup lang="ts">
import { computed, ref } from 'vue';
import { DEVICE_NAME, LOTTERY_MAX_IMAGES, MAX_SAFE_BRIGHTNESS, NUM_PANELS } from './constants';
import { Command } from './proto';
import { loadPpmFiles } from './ppm';
import { useBle } from './useBle';
import GalleryThumb from './components/GalleryThumb.vue';
import PlaybackBar from './components/PlaybackBar.vue';
import LotteryPanel from './components/LotteryPanel.vue';
import DebugPanel from './components/DebugPanel.vue';
import './style.css';

const logs = ref<string[]>([]);
const logEl = ref<HTMLPreElement | null>(null);
function log(msg: string) {
  logs.value.push(msg);
  // DOM 更新后滚到底
  requestAnimationFrame(() => {
    if (logEl.value)
      logEl.value.scrollTop = logEl.value.scrollHeight;
  });
}

const ble = useBle(log);
const ppmFiles = loadPpmFiles();
ble.imageCount.value = ppmFiles.length;

// 目标灯板单选：0 = 三块一起，1/2/3 = 单板（GOTO/NEXT 用）
const targetPanel = ref(0);

// 全局亮度滑杆本地值；拖动只改本地，点「设置亮度」才下发，
// 固件回状态后以固件为准（applyStatus 直接写 ble.brightness，滑杆绑定它）。
// 拖动中若收到状态通知会被覆盖——可接受（与之前 vanilla 行为一致）。
const brightnessLocal = ref(8);

// 抽奖速度滑杆本地值（每次 START 即时生效）
const lotterySpeed = ref(16);

// 奖池勾选：pool[图片][板]，默认全不选
const pool = ref<boolean[][]>(ppmFiles.map(() => Array.from({ length: NUM_PANELS }, () => false)));

const highlightedLotteryClass = computed(() => (index: number) => {
  for (let p = 0; p < NUM_PANELS; p++) {
    if (ble.lottery.value[p] && ble.panelIndex.value[p] === index)
      return `lottery-p${p + 1}`;
  }
  return null;
});

function gotoImage(index: number) {
  void ble.sendCommand(Command.GOTO, { index, panel: targetPanel.value });
}

function setPool(index: number, panel: number, checked: boolean) {
  pool.value[index][panel - 1] = checked;
}

function toggleAuto() {
  void ble.sendCommand(ble.autoMode.value ? Command.AUTO_OFF : Command.AUTO_ON);
}

function nextPanel(panel: number) {
  void ble.sendCommand(Command.NEXT, { panel });
}

function setBrightness() {
  void ble.sendCommand(Command.SET_BRIGHTNESS, { brightness: Number(brightnessLocal.value) });
}

function toggleLottery(panel: number) {
  if (ble.lottery.value[panel - 1]) {
    void ble.sendCommand(Command.LOTTERY_STOP, { panel });
    return;
  }
  if (ppmFiles.length > LOTTERY_MAX_IMAGES) {
    log(`图片共 ${ppmFiles.length} 张，超过单次写入上限 ${LOTTERY_MAX_IMAGES} 张，抽奖拒绝发送`);
    return;
  }
  // 奖池位图：选中图序号 -> bit 置位（bit i = 第 i 张），96 张内单次写入装得下
  const mask = new Uint8Array(Math.ceil(ppmFiles.length / 8));
  let picked = 0;
  pool.value.forEach((checks, i) => {
    if (checks[panel - 1]) {
      mask[i >> 3] |= 1 << (i & 7);
      picked++;
    }
  });
  if (picked === 0) {
    log(`第 ${panel} 块板奖池为空：请至少勾选一张图`);
    return;
  }
  void ble.sendCommand(Command.LOTTERY_START, { panel, speed: lotterySpeed.value, lotteryMask: mask });
}

function stopAllLottery() {
  void ble.sendCommand(Command.LOTTERY_STOP, { panel: 0 });
}

function sendDebugFrame(frame: Uint8Array) {
  void ble.sendFrame(frame);
}
</script>

<template>
  <h1>{{ DEVICE_NAME }} 灯板控制台</h1>
  <p>
    状态：<span>{{ ble.statusText.value }}</span>
    <button @click="ble.connected.value ? ble.disconnect() : ble.connect()">
      {{ ble.connected.value ? '断开连接' : '连接设备' }}
    </button>
  </p>
  <div id="panel">
    <PlaybackBar
      :connected="ble.connected.value"
      :auto-mode="ble.autoMode.value"
      :busy="ble.busy.value"
      @auto="toggleAuto"
      @next="nextPanel"
    />
    <p>
      <label for="brightness">全局亮度（0-{{ MAX_SAFE_BRIGHTNESS }}，图片与调试帧通用）：</label>
      <input
        id="brightness"
        type="range"
        :min="0"
        :max="MAX_SAFE_BRIGHTNESS"
        v-model.number="brightnessLocal"
        :disabled="!ble.connected.value"
      />
      <span>{{ brightnessLocal }}</span>
      <button :disabled="!ble.connected.value || ble.busy.value" @click="setBrightness">
        设置亮度
      </button>
    </p>
    <p>
      目标灯板：
      <label><input v-model="targetPanel" type="radio" :value="0" />三块一起</label>
      <label><input v-model="targetPanel" type="radio" :value="1" />第 1 块</label>
      <label><input v-model="targetPanel" type="radio" :value="2" />第 2 块</label>
      <label><input v-model="targetPanel" type="radio" :value="3" />第 3 块</label>
    </p>
    <p>点击图片 = 所选灯板跳到该图片（选「三块一起」则三块同显所点图片）</p>
    <div id="grid">
      <GalleryThumb
        v-for="(buf, i) in ppmFiles"
        :key="i"
        :buf="buf"
        :index="i"
        :highlighted="ble.panelIndex.value.includes(i)"
        :lottery-class="highlightedLotteryClass(i)"
        :pool="pool[i]"
        :disabled="ble.busy.value"
        @goto="gotoImage"
        @update:pool="(panel, checked) => setPool(i, panel, checked)"
      />
    </div>
    <LotteryPanel
      :connected="ble.connected.value"
      :speed="lotterySpeed"
      :lottery="ble.lottery.value"
      :busy="ble.busy.value"
      @update:speed="lotterySpeed = $event"
      @toggle="toggleLottery"
      @stop-all="stopAllLottery"
    />
    <DebugPanel
      :connected="ble.connected.value"
      :debug-mode="ble.debugMode.value"
      :busy="ble.busy.value"
      @enter="ble.sendCommand(Command.DEBUG_ENTER)"
      @exit="ble.sendCommand(Command.DEBUG_EXIT)"
      @send="sendDebugFrame"
    />
    <pre id="log" ref="logEl">{{ logs.join('\n') }}</pre>
  </div>
</template>
