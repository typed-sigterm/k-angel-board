# KAngel Display Board

> ✟小天使请安✟

用 ESP32wroom32 驱动 LED 点阵屏显示[超绝最可爱天使酱](https://zh.moegirl.org.cn/%E9%9B%A8(%E4%B8%BB%E6%92%AD%E5%A5%B3%E5%AD%A9%E9%87%8D%E5%BA%A6%E4%BE%9D%E8%B5%96))！

为 Gadfly128 的 cosplay 设计。

> [!NOTE]
> 本项目采用 MIT License，但是 `assets/**` `public/favicon.ico` 是 [Why so serious, Inc.](https://whysoserious.jp) 的素材，使用时请注意。

## BLE 协议

`proto/display.proto` 是唯一的协议定义（Single Source of Truth）：

- 固件侧用 [prost](https://github.com/tokio-rs/prost) 编解码（`src/bin/main.rs` 的 `proto` 模块由 `build.rs` 经 prost-build 生成，`protoc` 由 `protoc-bin-vendored` 自带，无需系统安装）；
- console 侧用 [protobuf.js](https://github.com/protobufjs/protobuf.js) 编解码（运行时 `parse` 同一份 `.proto`）。

服务 UUID `0x9E00`：控制特征 `0x9E01`（读/写，载荷为 `Control` 编码字节），状态特征 `0x9E02`（读/通知，载荷为 `Status` 编码字节）。单条消息上限 64 字节（`MSG_CAP`），ATT 短写合法，无需补齐。

三块灯板：三块相同的 16x16 LED 板，分别接 GPIO12 / GPIO33 / GPIO32（各占一个 RMT 通道 ch0/ch1/ch2，ch3 留给 radio/BLE），图片索引各自独立。`Control.panel` 指定目标灯板：0 = 三块一起（默认，兼容旧 console），1/2/3 = 仅对应单板；图片类命令（`NEXT` / `GOTO`）中 0 表三块一起，抽奖命令（`LOTTERY_*`）的 panel 必须为 1/2/3（每块板独立奖池），其它设置（亮度 / 自动开关 / 调试 / 帧回传）全局生效。`Status.panel_index` 回传三块板各自的索引（`index` 字段回填第 1 块板，保持旧 console 可用），`Status.lottery` 回传三块板各自的滚动标志。

抽奖滚动：console 在每张预览图下勾选 板1/板2/板3 组成各板奖池，`LOTTERY_START(panel=1/2/3, speed=1-120行/秒, lottery_mask=奖池位图)` 启动单板垂直滚动（奖池图片逐行向上滚，16 行=滚过一张图；位图 bit i = 第 i 张图，96 张图内单次写入装得下），`LOTTERY_STOP(panel=1/2/3 停单板，panel=0 停三块板)` 立即停在当前画面（行对齐，无减速）。滚动中该板暂停自动轮播；图片类命令（`NEXT/GOTO/AUTO_*`）先停目标板滚动再执行；调试模式下拒绝启动抽奖。速度全局统一（滑杆 1-120 行/秒，每次 START 即时生效）。

调试模式：`DEBUG_ENTER` 进入（暂停轮播、三块板同显调试帧缓冲）、`DEBUG_EXIT` 退出（回到各自图片显示）；回传流程为 `FRAME_SEQ(index=帧序号)` → 128 片 `FRAME_CHUNK(offset/pixels/last，6 字节/片)` → 固件攒齐整帧（768 字节）后三块板一次性同刷并经 `Status.frame_seq` 回显确认，console 收不到确认则整帧重传（至多 3 次）；`SET_BRIGHTNESS` 设置全局亮度（0-255，图片与调试帧通用，三块板一起，固件钳制到 `MAX_SAFE_BRIGHTNESS = 64` 限流保护）。`Status` 回传 `index/auto/debug/brightness/frame_seq/panel_index` 六字段。图片类命令（`NEXT/AUTO_*/TOGGLE/GOTO`）在调试模式下先退出调试再执行。

扩展约定：新增消息一律追加新字段 / 新枚举值，绝不复用或重编号已有字段与枚举值。

> [!CAUTION]
> 当前线格式与历史版本（控制特征单字节 `0x01/0x02/0x03/0xFF`、状态特征 `[index, auto]`）不兼容，固件与 console 必须配套更新。
