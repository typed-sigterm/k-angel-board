// 前端常量：BLE UUID 与固件 `src/bin/main.rs` 保持一致，
// 调试帧分片约束见 `proto/display.proto` 注释。
export const DEVICE_NAME = 'k-angel-board';
export const SERVICE_UUID = 0x9E00;
export const CONTROL_UUID = 0x9E01;
export const STATUS_UUID = 0x9E02;

// 调试帧：16x16 RGB888；分片 6 字节 + protobuf 开销 <= 13 字节，
// 远小于 ATT 默认 20 字节载荷（18~19 字节写入在部分 BLE 栈会被截断，
// 导致颜色错位 / 噪点），无需 MTU 协商
export const DEBUG_W = 16;
export const DEBUG_H = 16;
export const DEBUG_BYTES = DEBUG_W * DEBUG_H * 3;
export const CHUNK_LEN = 6;
export const MAX_SAFE_BRIGHTNESS = 255;

// 抽奖位图上限 96 张图（12 字节），与固件 `LOTTERY_MASK_CAP` 一致
export const LOTTERY_MAX_IMAGES = 96;
export const LOTTERY_MASK_BYTES = 12;

// 灯板数量，与固件 `NUM_PANELS` 一致
export const NUM_PANELS = 3;
