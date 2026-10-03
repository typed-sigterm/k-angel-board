<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { drawPpmToCanvas } from '../ppm';

const props = defineProps<{
  buf: ArrayBuffer
  index: number
  highlighted: boolean
  lotteryClass: string | null
  pool: boolean[]
  disabled: boolean
}>();

const emit = defineEmits<{
  goto: [index: number]
  'update:pool': [panel: number, checked: boolean]
}>();

const canvasRef = ref<HTMLCanvasElement | null>(null);
const broken = ref(false);

onMounted(() => {
  if (canvasRef.value && !drawPpmToCanvas(canvasRef.value, props.buf))
    broken.value = true;
});
</script>

<template>
  <div class="thumb">
    <canvas
      ref="canvasRef"
      width="16"
      height="16"
      :class="[{ current: highlighted, disabled }, lotteryClass]"
      :title="broken ? 'PPM 解析失败' : disabled ? '发送中，请稍候' : `点击跳转到第 ${index + 1} 张`"
      @click="!disabled && emit('goto', index)"
    />
    <div class="pick">
      <label v-for="p in 3" :key="p">
        <input
          type="checkbox"
          :checked="pool[p - 1]"
          :title="`第 ${p} 块板奖池${index + 1}`"
          @change="emit('update:pool', p, ($event.target as HTMLInputElement).checked)"
        />
        板{{ p }}
      </label>
    </div>
  </div>
</template>
