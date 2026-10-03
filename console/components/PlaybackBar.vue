<script setup lang="ts">
defineProps<{
  connected: boolean
  autoMode: boolean
  busy: boolean
}>();

const emit = defineEmits<{
  auto: []
  next: [panel: number]
}>();
</script>

<template>
  <p>
    <button :disabled="!connected || busy" @click="emit('auto')">
      启动/停止自动切换
    </button>
    <button :disabled="!connected || busy" @click="emit('next', 0)">
      三块一起下一张
    </button>
    <button v-for="p in 3" :key="p" :disabled="!connected || busy" @click="emit('next', p)">
      第 {{ p }} 块下一张
    </button>
    <span v-if="connected">
      {{ autoMode ? '自动切换开' : '自动切换关' }}
    </span>
  </p>
</template>
