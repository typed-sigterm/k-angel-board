import vue from '@vitejs/plugin-vue';
import portless from 'unplugin-portless/vite';
import { defineConfig } from 'vite';
import arraybuffer from 'vite-plugin-arraybuffer';

export default defineConfig({
  plugins: [
    vue(),
    portless(),
    arraybuffer(),
  ],
});
