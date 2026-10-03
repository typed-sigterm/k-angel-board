<script setup lang="ts">
import { computed } from 'vue';
import { NUM_PANELS } from '../constants';

const props = defineProps<{
  connected: boolean
  speed: number
  lottery: boolean[]
  busy: boolean
}>();

const emit = defineEmits<{
  'update:speed': [value: number]
  toggle: [panel: number]
  stopAll: []
}>();

const rollingText = computed(() => {
  const rolling = props.lottery
    .map((on, p) => on ? `板${p + 1}` : '')
    .filter(Boolean)
    .join(' ');
  return rolling ? `滚动中：${rolling}` : '未开始';
});
</script>

<template>
  <fieldset id="lottery-panel">
    <legend>抽奖滚动（每块板独立选图，统一速度，垂直滚动）</legend>
    <p>
      <label for="lottery-speed">滚动速度（1-120 行/秒，16 行=滚过一张图）：</label>
      <input
        id="lottery-speed"
        type="range"
        min="1"
        max="120"
        :value="speed"
        :disabled="!connected"
        @input="emit('update:speed', Number(($event.target as HTMLInputElement).value))"
      />
      <span>{{ speed }}</span>
    </p>
    <p>
      <button
        v-for="p in NUM_PANELS"
        :key="p"
        :disabled="!connected || busy"
        @click="emit('toggle', p)"
      >
        {{ lottery[p - 1] ? `第 ${p} 块停止` : `第 ${p} 块开始` }}
      </button>
      <button :disabled="!connected || busy" @click="emit('stopAll')">
        三块全停
      </button>
      <span>{{ rollingText }}{{ busy ? '（发送中…）' : '' }}</span>
    </p>
    <p class="hint">
      每张预览图下的 板1/板2/板3 勾选 = 该板奖池；点「开始」滚动，点「停止」立即停在当前画面。滚动中该板暂停自动轮播，点下一张/点图/开关自动会先停该板滚动。
    </p>
  </fieldset>
</template>
