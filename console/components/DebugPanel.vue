<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { DEBUG_BYTES, DEBUG_H, DEBUG_W } from '../constants';

export type Brush = [number, number, number];

const props = defineProps<{
  connected: boolean
  debugMode: boolean
  busy: boolean
}>();

const emit = defineEmits<{
  enter: []
  exit: []
  send: [frame: Uint8Array]
}>();

const frame = new Uint8Array(DEBUG_BYTES);
const brush = ref<Brush>([255, 0, 0]);
const canvasRef = ref<HTMLCanvasElement | null>(null);

function render() {
  const ctx = canvasRef.value?.getContext('2d');
  if (!ctx)
    return;
  const img = ctx.createImageData(DEBUG_W, DEBUG_H);
  for (let p = 0; p < DEBUG_W * DEBUG_H; p++) {
    img.data[p * 4 + 0] = frame[p * 3 + 0];
    img.data[p * 4 + 1] = frame[p * 3 + 1];
    img.data[p * 4 + 2] = frame[p * 3 + 2];
    img.data[p * 4 + 3] = 255;
  }
  ctx.putImageData(img, 0, 0);
}

function fill(r: number, g: number, b: number) {
  for (let p = 0; p < DEBUG_W * DEBUG_H; p++) {
    frame[p * 3 + 0] = r;
    frame[p * 3 + 1] = g;
    frame[p * 3 + 2] = b;
  }
  render();
}

function paint(r: number, g: number, b: number) {
  brush.value = [r, g, b];
  fill(r, g, b);
}

function togglePixel(px: number, py: number) {
  const off = (py * DEBUG_W + px) * 3;
  const isOn = frame[off] !== 0 || frame[off + 1] !== 0 || frame[off + 2] !== 0;
  const [r, g, b] = isOn ? [0, 0, 0] : brush.value;
  frame[off] = r;
  frame[off + 1] = g;
  frame[off + 2] = b;
  render();
}

function onCanvasClick(e: MouseEvent) {
  const rect = (e.target as HTMLCanvasElement).getBoundingClientRect();
  const px = Math.min(DEBUG_W - 1, Math.max(0, Math.floor((e.clientX - rect.left) / rect.width * DEBUG_W)));
  const py = Math.min(DEBUG_H - 1, Math.max(0, Math.floor((e.clientY - rect.top) / rect.height * DEBUG_H)));
  togglePixel(px, py);
}

onMounted(render);

defineExpose({ frame });
</script>

<template>
  <fieldset id="debug-panel">
    <legend>调试模式（点阵图回传）</legend>
    <p>
      <button :disabled="!connected || busy" @click="debugMode ? emit('exit') : emit('enter')">
        {{ debugMode ? '退出调试模式' : '进入调试模式' }}
      </button>
      <span>{{ debugMode ? '调试中（轮播已暂停）' : '未进入调试' }}{{ busy ? '（发送中…）' : '' }}</span>
    </p>
    <p>
      <canvas
        ref="canvasRef"
        id="debug-canvas"
        :width="DEBUG_W"
        :height="DEBUG_H"
        @click="onCanvasClick"
      />
    </p>
    <p>
      <button :disabled="!connected || busy" @click="fill(0, 0, 0)">
        清空调色板
      </button>
      <button :disabled="!connected || busy" @click="paint(255, 0, 0)">
        全红
      </button>
      <button :disabled="!connected || busy" @click="paint(0, 255, 0)">
        全绿
      </button>
      <button :disabled="!connected || busy" @click="paint(255, 255, 255)">
        全白
      </button>
      <button :disabled="!connected || busy" @click="paint(0, 0, 255)">
        全蓝
      </button>
      <button :disabled="!connected || busy" @click="emit('send', frame)">
        回传显示
      </button>
    </p>
    <p class="hint">
      点击调色板格子切换颜色，再点「回传显示」把 16x16 点阵图分片发到灯板直显（自动重传直到灯板确认）。
    </p>
  </fieldset>
</template>
