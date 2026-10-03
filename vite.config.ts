import { defineConfig } from 'vite';
import portless from 'unplugin-portless/vite';
import arraybuffer from "vite-plugin-arraybuffer";

export default defineConfig({
  plugins: [
    portless(),
    arraybuffer(),
  ],
});
