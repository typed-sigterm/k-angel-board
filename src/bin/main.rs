#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Timer;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Pull},
    interrupt::software::SoftwareInterruptControl,
    rmt::{PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_radio::ble::controller::BleConnector;
use prost::Message;
use trouble_host::prelude::*;

extern crate alloc;

#[path = "../images.rs"]
mod images;

use esp_backtrace as _;
use esp_println::println;
esp_bootloader_esp_idf::esp_app_desc!();

// ============ 显示配置 ============
/// 16x16 像素阵列（单块板）
const WIDTH: usize = 16;
const HEIGHT: usize = 16;
const NUM_LEDS: usize = WIDTH * HEIGHT;
/// 灯板数量：三块相同的 16x16 LED 板，分别接 GPIO12 / GPIO33 / GPIO32
const NUM_PANELS: usize = 3;
/// 灯带是否按蛇形（Z 字形）布线；逐行直连则设为 false
const SERPENTINE: bool = false;
/// 全局亮度 0-255。256 颗灯珠全白满载电流很大，需配合独立 5V 电源；
/// 可经 BLE `SET_BRIGHTNESS` 运行时调整（全局生效，图片与调试帧通用），
/// 上限钳制见 `MAX_SAFE_BRIGHTNESS`。
const DEFAULT_BRIGHTNESS: u8 = 32;
/// 全局亮度上限（限流保护，避免烧线 / 触发电源保护）
const MAX_SAFE_BRIGHTNESS: u8 = 255;
/// 按钮去抖时间
const DEBOUNCE_MS: u64 = 30;
/// 自动切换图片的间隔
const SWITCH_INTERVAL_MS: u64 = 500;

// ============ WS2812 RMT 时序 ============
// RMT 基准时钟 80MHz、clk_divider = 1 => 1 tick = 12.5ns
const T0H: u16 = 32; // 0.40us
const T0L: u16 = 68; // 0.85us
const T1H: u16 = 64; // 0.80us
const T1L: u16 = 36; // 0.45us
/// 复位码：低电平 >= 50us
const RESET_TICKS: u16 = 4000;
/// 整帧波形条数：每 bit 一条 PulseCode + 1 条复位码 + 1 条 end marker
const FRAME_CODES: usize = NUM_LEDS * 24 + 2;

// ============ BLE 控制协议（protobuf，见 proto/display.proto） ============
/// BLE 消息的唯一真实来源是 `proto/display.proto`：
/// 固件侧用 prost 编解码，console 侧用 protobuf.js 编解码。
/// 控制特征（0x9E01）写入载荷是 `Control` 编码后的字节，
/// 状态特征（0x9E02）读取 / 通知载荷是 `Status` 编码后的字节。
mod proto {
    include!(concat!(env!("OUT_DIR"), "/kangel.display.rs"));
}

/// 单条 protobuf 消息的容量上限。`Control` 含抽奖位图时约 27 字节
/// （12 字节位图 + protobuf 开销；96 张图以内单次 ATT 写装得下，
///  超过则 console 拒绝发送），`Status` 编码后约 15~22 字节
/// （含三块板索引 + 抽奖标志）；128 字节留足余量。
const MSG_CAP: usize = 128;
/// 抽奖位图上限字节数：96 张图（12 字节）。单次 ATT 写入约 20 字节上限内，
/// 位图 + 命令开销能装进一次写入；超过 96 张图的 console 直接拒绝发送。
const LOTTERY_MASK_CAP: usize = 12;
/// 抽奖滚动速度范围（行/秒）：1 行/秒最慢，120 行/秒最快
/// （16 行 = 滚过一张图；120 行/秒 = 7.5 张图/秒，接近 WS2812 逐行刷新上限）。
const LOTTERY_SPEED_MIN: u32 = 1;
const LOTTERY_SPEED_MAX: u32 = 120;
/// 调试帧：16x16 RGB888 原始字节（console 以 6 字节分片上传，固件拼装后直显）
const DEBUG_FRAME_BYTES: usize = WIDTH * HEIGHT * 3;
/// 单个 FRAME_CHUNK 分片载荷上限（6 字节 + 开销 <= 13 字节，远小于 ATT 载荷上限，
/// 避免某些 BLE 栈把 18~19 字节写入截断到 17 字节导致颜色错位 / 噪点）
const CHUNK_CAP: usize = 6;

/// 显示状态快照：显示任务发布，BLE 任务据此向已连接客户端发通知
#[derive(Clone, Copy)]
struct DisplayState {
    /// 三块灯板各自的图片索引
    panel_index: [u8; NUM_PANELS],
    /// 三块灯板各自是否在抽奖滚动中
    lottery: [bool; NUM_PANELS],
    auto: bool,
    debug: bool,
    brightness: u8,
    frame_seq: u32,
}

/// 抽奖滚动状态（每块板独立）：奖池图片在 16x16 点阵内逐行向上滚动
struct LotteryState {
    /// 是否滚动中
    active: bool,
    /// 滚动速度（行/秒）
    speed: u32,
    /// 奖池：参与滚动的图片索引（去重、保序）
    pool: heapless::Vec<u8, 96>,
    /// 奖池下标：下一行要补的图片在 pool 中的位置
    pool_pos: usize,
    /// 毫秒累积：按速度换算该滚几行（10ms 滴答累加，避免除法抖动）
    acc_ms: u32,
    /// 当前屏 16 行：每行是 (图片索引, 该图行号)
    rows: [(u8, u8); HEIGHT],
}

impl LotteryState {
    const fn new() -> Self {
        Self {
            active: false,
            speed: 16,
            pool: heapless::Vec::new(),
            pool_pos: 0,
            acc_ms: 0,
            rows: [(0, 0); HEIGHT],
        }
    }

    /// 启动滚动：首屏直接显示奖池第 1 张整图（顶行即该图第 0 行），
    /// 后续滴答逐行上滚、底部按奖池顺序补行。
    fn start(&mut self, pool: heapless::Vec<u8, 96>, speed: u32) {
        let first = pool[0];
        self.active = true;
        self.speed = speed;
        self.pool = pool;
        self.pool_pos = 1 % self.pool.len();
        self.acc_ms = 0;
        for (y, slot) in self.rows.iter_mut().enumerate() {
            *slot = (first, y as u8);
        }
    }

    /// 推进 elapsed_ms 毫秒，返回是否有行进位（需重刷）。
    /// 换算：rows = speed * ms / 1000，余数留存下次。
    fn tick(&mut self, elapsed_ms: u64) -> bool {
        self.acc_ms += elapsed_ms as u32;
        let rows = (self.speed * self.acc_ms / 1000) as usize;
        if rows == 0 {
            return false;
        }
        self.acc_ms -= rows as u32 * 1000 / self.speed;
        for _ in 0..rows {
            self.scroll_one();
        }
        true
    }

    /// 上滚一行：顶行丢弃，其余上移，底部按奖池顺序补下一行
    /// （补到某图第 0 行时 pool_pos 才进到下一张，保证整图依次滚过）。
    fn scroll_one(&mut self) {
        self.rows.copy_within(1.., 0);
        let (img, row) = self.rows[HEIGHT - 1];
        if row + 1 >= HEIGHT as u8 {
            let next = self.pool[self.pool_pos];
            self.pool_pos = (self.pool_pos + 1) % self.pool.len();
            self.rows[HEIGHT - 1] = (next, 0);
        } else {
            self.rows[HEIGHT - 1] = (img, row + 1);
        }
    }

    /// 当前屏顶行所属图片：停止时回填 panel_index 用
    fn top_image(&self) -> u8 {
        self.rows[0].0
    }
}

/// 命令队列：按钮任务和 BLE 任务写入解码后的 `Control`，显示任务消费
/// （容量 16：帧分片上传时 BLE 写与 WS2812 刷新速度接近，留足缓冲避免丢片）
static CMD_CHANNEL: Channel<CriticalSectionRawMutex, proto::Control, 16> = Channel::new();
/// 状态队列：显示任务发布状态快照，BLE 任务据此向已连接客户端发通知
static INDEX_CHANNEL: Channel<CriticalSectionRawMutex, DisplayState, 8> = Channel::new();

/// 广播中携带的 16-bit 服务 UUID（0x9E00，小端字节序）
const SERVICE_UUID_LE: [u8; 2] = [0x00, 0x9e];

/// GATT 服务：一个控制特征（写入命令）+ 一个状态特征（读取/通知）
#[gatt_server]
struct DisplayServer {
    control_service: ControlService,
}

#[gatt_service(uuid = "9e00")]
struct ControlService {
    #[characteristic(uuid = "9e01", read, write)]
    control: heapless::Vec<u8, MSG_CAP>,
    #[characteristic(uuid = "9e02", read, notify)]
    status: heapless::Vec<u8, MSG_CAP>,
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // BLE 驱动所需的堆。开启 BLE 后 RWDATA 只有 128KB（BT 控制器保留前 64KB），
    // 若堆放 .bss 会把主栈挤到只剩 ~2.7KB（实测栈溢出 panic）。
    // 因此把 80KB 堆放进 dram2_seg（esp-hal 预留但默认不用的 ~96KB DRAM）
    #[repr(C, align(16))]
    struct Dram2Heap(core::mem::MaybeUninit<[u8; 80 * 1024]>);
    #[unsafe(link_section = ".dram2_uninit")]
    static mut HEAP: Dram2Heap = Dram2Heap(core::mem::MaybeUninit::uninit());
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            core::ptr::addr_of_mut!(HEAP).cast::<u8>(),
            80 * 1024,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }

    // esp-rtos 调度器：需要一个定时器 + 一个软件中断
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupt = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

    // WS2812 数据通道（每块板一个 RMT 通道）。
    // 三块板按地址顺序用完 ch0/ch1/ch2：负载小、时序不占满 RMT RAM，
    // 使 ch3 留给 radio/BLE 做并行 TX，避免 BLE 通知被阻塞。
    // 空闲时持续输出低电平：
    // 否则发完一帧后数据线悬空，杜邦线断开再重接时灯带容易锁存噪声且不刷新
    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let tx_config = TxChannelConfig::default()
        .with_clk_divider(1)
        .with_idle_output_level(Level::Low)
        .with_idle_output(true);
    let ws2812 = {
        let pixels = unsafe { &mut *core::ptr::addr_of_mut!(PIXELS) };
        let codes = unsafe { &mut *core::ptr::addr_of_mut!(CODES) };
        // 帧缓冲和波形缓冲都放在静态存储里，避免占用宝贵的任务栈
        let (p0, rest) = pixels.split_at_mut(NUM_LEDS * 3);
        let (p1, rest) = rest.split_at_mut(NUM_LEDS * 3);
        let (p2, _) = rest.split_at_mut(NUM_LEDS * 3);
        let p0: &mut [u8; NUM_LEDS * 3] = p0.try_into().unwrap();
        let p1: &mut [u8; NUM_LEDS * 3] = p1.try_into().unwrap();
        let p2: &mut [u8; NUM_LEDS * 3] = p2.try_into().unwrap();
        let ch0 = rmt
            .channel0
            .configure_tx(&tx_config)
            .unwrap()
            .with_pin(peripherals.GPIO12);
        let ch1 = rmt
            .channel1
            .configure_tx(&tx_config)
            .unwrap()
            .with_pin(peripherals.GPIO33);
        let ch2 = rmt
            .channel2
            .configure_tx(&tx_config)
            .unwrap()
            .with_pin(peripherals.GPIO32);
        // 三块板共用一条波形缓冲：逐板编码 + 逐板发送，峰值内存只多一块帧缓冲
        Ws2812Panels::new([ch0, ch1, ch2], [p0, p1, p2], codes)
    };

    // 切换按钮（外接）：一端接 GPIO0、另一端接 GND，内部上拉，按下为低电平
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    spawner.spawn(display_task(ws2812).unwrap());
    spawner.spawn(button_task(button).unwrap());
    // BT 外设移入任务内创建 BleConnector（其内部含裸指针、非 Send）
    spawner.spawn(ble_task(peripherals.BT).unwrap());

    println!(
        "k-angel display board: {} 张图片已加载，BLE 广播名 k-angel-board",
        images::IMAGES.len()
    );
}

/// BLE 外设任务：广播、接受连接、处理 GATT 写入，并把状态变化通知给客户端
#[embassy_executor::task]
async fn ble_task(bt: esp_hal::peripherals::BT<'static>) {
    let connector = BleConnector::new(bt, Default::default()).unwrap();
    let controller: ExternalController<_, 20> = ExternalController::new(connector);
    let mut resources: HostResources<DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources);
    let Host {
        mut peripheral,
        mut runner,
        ..
    } = stack.build();

    let server = DisplayServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "k-angel-board",
        appearance: &appearance::computer::GENERIC_COMPUTER,
    }))
    .unwrap();

    // 广播数据：标志位 + 服务 UUID + 设备名（供扫描器展示）
    let mut adv_data = [0u8; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids16(&[SERVICE_UUID_LE]),
            AdStructure::CompleteLocalName(b"k-angel-board"),
        ],
        &mut adv_data,
    )
    .unwrap();

    embassy_futures::join::join(runner.run(), async {
        loop {
            // 广播并等待中心设备连接
            let advertiser = match peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..adv_len],
                        scan_data: &[],
                    },
                )
                .await
            {
                Ok(advertiser) => advertiser,
                Err(e) => {
                    println!("广播失败: {e:?}");
                    continue;
                }
            };
            let conn = match advertiser.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    println!("接受连接失败: {e:?}");
                    continue;
                }
            };
            println!("BLE 已连接");
            let gatt = match conn.with_attribute_server(&server.server) {
                Ok(gatt) => gatt,
                Err(e) => {
                    println!("GATT 注册失败: {e:?}");
                    continue;
                }
            };
            if gatt_loop(&server, &gatt).await.is_err() {
                println!("BLE 已断开");
            }
        }
    })
    .await;
}

/// 单个连接的 GATT 事件循环：处理控制写入，同时把最新状态通知给客户端
async fn gatt_loop<P: PacketPool>(
    server: &DisplayServer<'_>,
    conn: &GattConnection<'_, '_, P>,
) -> Result<(), Error> {
    let control_handle = server.control_service.control.handle;
    loop {
        // 竞争等待：GATT 事件 或 本地状态更新
        match select(conn.next(), INDEX_CHANNEL.receive()).await {
            Either::First(GattConnectionEvent::Disconnected { reason }) => {
                println!("连接断开: {reason:?}");
                return Err(Error::Disconnected);
            }
            Either::First(GattConnectionEvent::Gatt { event }) => match event {
                GattEvent::Write(write) if write.handle() == control_handle => {
                    match proto::Control::decode(write.data()) {
                        Ok(msg) => {
                            println!("BLE 命令：{}({})", command_name(msg.command), msg.command);
                            CMD_CHANNEL.try_send(msg).ok();
                        }
                        Err(_) => {
                            println!("非法控制消息：{} 字节，忽略", write.data().len());
                        }
                    }
                    write.accept()?.send().await;
                }
                _ => {
                    event.accept()?.send().await;
                }
            },
            Either::First(_) => {}
            Either::Second(state) => {
                // 通知状态：`Status` 编码后字节（同时更新特征存储值，read 能读到最新状态）。
                // index 回填第 1 块板的索引，保持旧 console 可用；
                // panel_index 携带三块板各自的索引，lottery 携带滚动标志，
                // 新 console 用它们分别高亮 / 显示滚动中。
                let status = proto::Status {
                    index: state.panel_index[0] as u32,
                    auto: state.auto,
                    debug: state.debug,
                    brightness: state.brightness as u32,
                    frame_seq: state.frame_seq,
                    panel_index: state.panel_index.iter().map(|&i| i as u32).collect(),
                    lottery: state.lottery.iter().copied().collect(),
                };
                match heapless::Vec::<u8, MSG_CAP>::from_slice(&status.encode_to_vec()) {
                    Ok(payload) => {
                        server.control_service.status.notify(conn, &payload).await.ok();
                    }
                    Err(_) => println!("状态编码溢出（>{MSG_CAP} 字节），跳过本次通知"),
                }
            }
        }
    }
}

/// 命令编号的可读名称（日志用）
fn command_name(command: i32) -> &'static str {
    match proto::Command::try_from(command).ok() {
        Some(proto::Command::Next) => "NEXT",
        Some(proto::Command::AutoOn) => "AUTO_ON",
        Some(proto::Command::AutoOff) => "AUTO_OFF",
        Some(proto::Command::Toggle) => "TOGGLE",
        Some(proto::Command::Goto) => "GOTO",
        Some(proto::Command::DebugEnter) => "DEBUG_ENTER",
        Some(proto::Command::DebugExit) => "DEBUG_EXIT",
        Some(proto::Command::SetBrightness) => "SET_BRIGHTNESS",
        Some(proto::Command::FrameChunk) => "FRAME_CHUNK",
        Some(proto::Command::FrameSeq) => "FRAME_SEQ",
        Some(proto::Command::LotteryStart) => "LOTTERY_START",
        Some(proto::Command::LotteryStop) => "LOTTERY_STOP",
        _ => "未知",
    }
}

/// 显示任务：唯一的画面控制者。消费命令队列，自动模式下每 500ms 切换一张；
/// 调试模式下暂停轮播，直接显示 console 分片上传的 16x16 RGB888 帧。
/// 三块灯板：图片索引各自独立（NEXT/GOTO 可带 panel 单独控制），
/// 亮度 / 自动开关 / 调试帧全局通用。
/// 抽奖滚动：每块板独立开关 + 独立奖池 + 统一速度，奖池图片逐行向上滚动；
/// 滚动中该板暂停自动轮播，图片类命令（NEXT/GOTO/AUTO）先停该板滚动再执行，
/// 收到 LOTTERY_STOP 立即停在当前画面（行对齐，无减速过程）。
#[embassy_executor::task]
async fn display_task(mut ws2812: Ws2812Panels) {
    use proto::Command;

    let mut panel_index = [0usize; NUM_PANELS];
    let mut lottery = [false; NUM_PANELS];
    let mut auto = true;
    let mut debug = false;
    let mut brightness = DEFAULT_BRIGHTNESS;
    let mut frame_seq: u32 = 0;
    let mut pending_seq: Option<u32> = None;
    let mut pending_count: usize = 0;
    let mut lottery_state = [
        LotteryState::new(),
        LotteryState::new(),
        LotteryState::new(),
    ];
    /// 抽奖滴答粒度：每 10ms 按速度换算该滚几行（1 行/秒 = 100 秒滚 100 行，
    /// 最小可辨；120 行/秒 = 每滴答 1.2 行，累积换算无抖动）
    const LOTTERY_TICK_MS: u64 = 10;
    let frame = unsafe { &mut *core::ptr::addr_of_mut!(DEBUG_FRAME) };
    frame.fill(0);

    let publish = |panel_index: &[usize; NUM_PANELS],
                   lottery: &[bool; NUM_PANELS],
                   auto: bool,
                   debug: bool,
                   brightness: u8,
                   frame_seq: u32| {
        INDEX_CHANNEL
            .try_send(DisplayState {
                panel_index: [
                    panel_index[0] as u8,
                    panel_index[1] as u8,
                    panel_index[2] as u8,
                ],
                lottery: *lottery,
                auto,
                debug,
                brightness,
                frame_seq,
            })
            .ok();
    };
    draw_all(&mut ws2812, &panel_index, brightness);
    publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);

    loop {
        // 取下一条命令：三档等待——
        // 1. 有板在抽奖滚动：每 10ms 滴答推进滚动（多块板同一滴答一起推）；
        // 2. 非调试 + 自动模式：500ms 无命令视为"切换"（仅非滚动板参与）；
        // 3. 其余：阻塞等命令。
        enum Wake {
            Cmd(proto::Control),
            AutoTick,
            LotteryTick,
        }
        let wake = if lottery.iter().any(|&l| l) {
            match select(
                CMD_CHANNEL.receive(),
                Timer::after_millis(LOTTERY_TICK_MS),
            )
            .await
            {
                Either::First(cmd) => Wake::Cmd(cmd),
                Either::Second(()) => Wake::LotteryTick,
            }
        } else if auto && !debug {
            match select(
                CMD_CHANNEL.receive(),
                Timer::after_millis(SWITCH_INTERVAL_MS),
            )
            .await
            {
                Either::First(cmd) => Wake::Cmd(cmd),
                Either::Second(()) => Wake::AutoTick,
            }
        } else {
            Wake::Cmd(CMD_CHANNEL.receive().await)
        };

        // 抽奖滴答：每块滚动中的板按速度推进，有进位的板重刷
        if let Wake::LotteryTick = wake {
            let mut dirty = [false; NUM_PANELS];
            for (p, on) in lottery.iter().enumerate() {
                if *on && lottery_state[p].tick(LOTTERY_TICK_MS) {
                    dirty[p] = true;
                }
            }
            if dirty.iter().any(|&d| d) {
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
            }
            continue;
        }

        let cmd = match wake {
            Wake::Cmd(cmd) => cmd,
            // 自动轮播：退出调试的板不参与（调试全局暂停轮播，与之前一致）；
            // 抽奖滚动中的板不参与（滚动优先）。
            Wake::AutoTick => {
                if !debug {
                    let mut changed = false;
                    for (p, on) in lottery.iter().enumerate() {
                        if !on {
                            panel_index[p] = (panel_index[p] + 1) % images::IMAGES.len();
                            changed = true;
                        }
                    }
                    if changed {
                        draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                        println!(
                            "切换 -> [{} / {} / {}]",
                            images::IMAGE_NAMES[panel_index[0]],
                            images::IMAGE_NAMES[panel_index[1]],
                            images::IMAGE_NAMES[panel_index[2]]
                        );
                    }
                }
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                continue;
            }
            Wake::LotteryTick => continue,
        };

        // 图片类命令（NEXT/AUTO_*/TOGGLE/GOTO）：调试模式下先退出调试再执行；
        // 目标板在抽奖滚动中则先停滚动（停在当前画面），再执行。
        let exit_debug_for_gallery = |debug: &mut bool| -> bool {
            if *debug {
                *debug = false;
                true
            } else {
                false
            }
        };
        let stop_lottery_for_gallery = |lottery: &mut [bool; NUM_PANELS],
                                        lottery_state: &mut [LotteryState; NUM_PANELS],
                                        panel_index: &mut [usize; NUM_PANELS],
                                        targets: &[bool; NUM_PANELS]| {
            for (p, on) in targets.iter().enumerate() {
                if *on && lottery[p] {
                    lottery[p] = false;
                    lottery_state[p].active = false;
                    panel_index[p] = lottery_state[p].top_image() as usize;
                    println!("抽奖滚动：板{} 停止，停在 [{}]", p + 1, panel_index[p]);
                }
            }
        };

        // panel 目标解析：0（或缺省）= 三块板一起，1/2/3 = 仅对应单板；
        // 越界值忽略本条命令。
        let targets = |panel: u32| -> Option<[bool; NUM_PANELS]> {
            match panel {
                0 => Some([true; NUM_PANELS]),
                1 | 2 | 3 => {
                    let mut t = [false; NUM_PANELS];
                    t[(panel - 1) as usize] = true;
                    Some(t)
                }
                _ => None,
            }
        };

        match Command::try_from(cmd.command).ok() {
            Some(Command::Next) => {
                let Some(targets) = targets(cmd.panel) else {
                    println!("非法灯板编号：panel={}（0-3），忽略", cmd.panel);
                    continue;
                };
                if exit_debug_for_gallery(&mut debug) {
                    println!("退出调试模式，回到图片显示");
                }
                stop_lottery_for_gallery(
                    &mut lottery,
                    &mut lottery_state,
                    &mut panel_index,
                    &targets,
                );
                for (p, on) in targets.iter().enumerate() {
                    if *on {
                        panel_index[p] = (panel_index[p] + 1) % images::IMAGES.len();
                    }
                }
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                println!(
                    "切换 -> [{} / {} / {}]",
                    images::IMAGE_NAMES[panel_index[0]],
                    images::IMAGE_NAMES[panel_index[1]],
                    images::IMAGE_NAMES[panel_index[2]]
                );
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::AutoOn) => {
                if exit_debug_for_gallery(&mut debug) {
                    println!("退出调试模式，回到图片显示");
                }
                stop_lottery_for_gallery(
                    &mut lottery,
                    &mut lottery_state,
                    &mut panel_index,
                    &[true; NUM_PANELS],
                );
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                if !auto {
                    auto = true;
                    println!("自动切换：开启");
                }
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::AutoOff) => {
                if exit_debug_for_gallery(&mut debug) {
                    println!("退出调试模式，回到图片显示");
                }
                stop_lottery_for_gallery(
                    &mut lottery,
                    &mut lottery_state,
                    &mut panel_index,
                    &[true; NUM_PANELS],
                );
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                if auto {
                    auto = false;
                    println!("自动切换：停止");
                }
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::Toggle) => {
                if exit_debug_for_gallery(&mut debug) {
                    println!("退出调试模式，回到图片显示");
                }
                stop_lottery_for_gallery(
                    &mut lottery,
                    &mut lottery_state,
                    &mut panel_index,
                    &[true; NUM_PANELS],
                );
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                auto = !auto;
                println!("自动切换：{}", if auto { "开启" } else { "停止" });
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::Goto) => {
                // 点击 console 预览图：指定灯板跳到指定索引（越界则忽略）
                let Some(targets) = targets(cmd.panel) else {
                    println!("非法灯板编号：panel={}（0-3），忽略", cmd.panel);
                    continue;
                };
                let target = cmd.index as usize;
                if target < images::IMAGES.len() {
                    if exit_debug_for_gallery(&mut debug) {
                        println!("退出调试模式，回到图片显示");
                    }
                    stop_lottery_for_gallery(
                        &mut lottery,
                        &mut lottery_state,
                        &mut panel_index,
                        &targets,
                    );
                    for (p, on) in targets.iter().enumerate() {
                        if *on {
                            panel_index[p] = target;
                        }
                    }
                    draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                    println!(
                        "跳转 -> panel={} [{}] {}",
                        cmd.panel, target, images::IMAGE_NAMES[target]
                    );
                    publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                } else {
                    println!("非法跳转索引：{}（共 {} 张），忽略", target, images::IMAGES.len());
                }
            }
            Some(Command::DebugEnter) => {
                if !debug {
                    debug = true;
                    draw_debug_all(&mut ws2812, frame, brightness);
                    println!("调试模式：进入（暂停轮播，三块板同显调试帧）");
                    publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                }
            }
            Some(Command::DebugExit) => {
                if debug {
                    debug = false;
                    pending_seq = None;
                    pending_count = 0;
                    draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                    println!(
                        "调试模式：退出，回到 [{}/{}/{}]",
                        panel_index[0], panel_index[1], panel_index[2]
                    );
                    publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                }
            }
            Some(Command::SetBrightness) => {
                // 全局亮度：图片与调试帧通用，三块板一起，实时重刷当前画面
                let want = cmd.brightness.min(255);
                let clamped = want.min(MAX_SAFE_BRIGHTNESS as u32) as u8;
                if clamped as u32 != want {
                    println!("亮度 {want} 超过安全上限，钳制为 {clamped}");
                }
                if clamped != brightness {
                    brightness = clamped;
                    if debug {
                        draw_debug_all(&mut ws2812, frame, brightness);
                    } else {
                        draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                    }
                    println!("亮度 -> {brightness}");
                }
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::FrameSeq) => {
                // 新帧开始：清零暂存帧并计数，后续 FRAME_CHUNK 攒齐后才刷屏
                pending_seq = Some(cmd.index);
                pending_count = 0;
                frame.fill(0);
            }
            Some(Command::FrameChunk) => {
                let offset = cmd.offset as usize;
                let data = cmd.pixels.as_slice();
                if data.len() > CHUNK_CAP {
                    println!("分片过大：{} 字节（上限 {CHUNK_CAP}），忽略", data.len());
                } else if offset + data.len() > DEBUG_FRAME_BYTES {
                    println!(
                        "分片越界：offset={offset} len={}（帧共 {DEBUG_FRAME_BYTES} 字节），忽略",
                        data.len()
                    );
                } else {
                    // 无 FRAME_SEQ 的旧版 console 兼容：首片自动建帧
                    if pending_seq.is_none() {
                        pending_seq = Some(frame_seq.wrapping_add(1));
                        pending_count = 0;
                        frame.fill(0);
                    }
                    frame[offset..offset + data.len()].copy_from_slice(data);
                    pending_count += data.len();
                    // 攒齐整帧才刷屏：避免半帧上屏（颜色错位 / 噪点 / 撕裂），
                    // 且逐片刷屏的 WS2812 刷新会拖慢 BLE 接收造成丢片
                    if cmd.last {
                        let seq = pending_seq.unwrap_or(frame_seq.wrapping_add(1));
                        if pending_count >= DEBUG_FRAME_BYTES
                            && offset + data.len() == DEBUG_FRAME_BYTES
                        {
                            let entered = !debug;
                            debug = true;
                            frame_seq = seq;
                            draw_debug_all(&mut ws2812, frame, brightness);
                            if entered {
                                println!("调试模式：进入（调试帧 #{seq} 收齐，三块板同显）");
                            } else {
                                println!("调试帧 #{seq} 收齐（{pending_count} 字节），已显示");
                            }
                            publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                        } else {
                            println!(
                                "调试帧 #{seq} 不完整（{pending_count}/{DEBUG_FRAME_BYTES} 字节），丢弃等重传"
                            );
                            publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
                        }
                        pending_seq = None;
                        pending_count = 0;
                    }
                }
            }
            Some(Command::LotteryStart) => {
                // 抽奖开始：panel 必须为 1/2/3（每块板独立奖池），调试模式下拒绝启动
                if !(1..=3).contains(&cmd.panel) {
                    println!("抽奖启动：panel={} 非法（须为 1/2/3），忽略", cmd.panel);
                    continue;
                }
                if debug {
                    println!("抽奖启动：调试模式中，先退出调试再抽奖");
                    continue;
                }
                let p = (cmd.panel - 1) as usize;
                let speed = cmd.speed.clamp(LOTTERY_SPEED_MIN, LOTTERY_SPEED_MAX);
                if cmd.speed != speed {
                    println!("抽奖速度 {} 超限，钳制为 {speed} 行/秒", cmd.speed);
                }
                let mask = cmd.lottery_mask.as_slice();
                if mask.len() > LOTTERY_MASK_CAP {
                    println!("抽奖位图过大：{} 字节（上限 {LOTTERY_MASK_CAP}），忽略", mask.len());
                    continue;
                }
                // 位图 -> 奖池（去重、保序、跳过越界位）
                let mut pool: heapless::Vec<u8, 96> = heapless::Vec::new();
                let total = images::IMAGES.len();
                for (byte_i, &byte) in mask.iter().enumerate() {
                    for bit in 0..8 {
                        if byte & (1 << bit) != 0 {
                            let img = byte_i * 8 + bit;
                            if img < total && img <= u8::MAX as usize && !pool.contains(&(img as u8))
                            {
                                pool.push(img as u8).ok();
                            }
                        }
                    }
                }
                if pool.is_empty() {
                    println!("抽奖启动：奖池为空（位图全零或全越界），忽略");
                    continue;
                }
                lottery_state[p].start(pool, speed);
                lottery[p] = true;
                // 抽奖期间该板暂停自动轮播：panel_index 保持进滚动前的图，
                // 停止时回填顶行图片
                draw_mixed(&mut ws2812, &panel_index, &lottery, &lottery_state, brightness);
                println!(
                    "抽奖启动：板{p} {speed} 行/秒，奖池 {} 张",
                    lottery_state[p].pool.len()
                );
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            Some(Command::LotteryStop) => {
                // 抽奖停止：panel=0 停三块板，1/2/3 停单板；立即停在当前画面
                let Some(targets) = (match cmd.panel {
                    0 => Some([true; NUM_PANELS]),
                    1 | 2 | 3 => {
                        let mut t = [false; NUM_PANELS];
                        t[(cmd.panel - 1) as usize] = true;
                        Some(t)
                    }
                    _ => None,
                }) else {
                    println!("非法灯板编号：panel={}（0-3），忽略", cmd.panel);
                    continue;
                };
                let mut any = false;
                for (p, on) in targets.iter().enumerate() {
                    if *on && lottery[p] {
                        lottery[p] = false;
                        lottery_state[p].active = false;
                        panel_index[p] = lottery_state[p].top_image() as usize;
                        println!("抽奖停止：板{} 停在 [{}]", p + 1, panel_index[p]);
                        any = true;
                    }
                }
                if any {
                    draw_all(&mut ws2812, &panel_index, brightness);
                }
                publish(&panel_index, &lottery, auto, debug, brightness, frame_seq);
            }
            _ => println!("未知命令：command={}", cmd.command),
        }
    }
}

/// 按钮任务：去抖后等松开，向命令队列发送"下一张"
#[embassy_executor::task]
async fn button_task(button: Input<'static>) {
    let mut pressed = false;
    loop {
        Timer::after_millis(10).await;
        let is_down = button.is_low();
        if is_down && !pressed {
            pressed = true;
            Timer::after_millis(DEBOUNCE_MS).await;
            if button.is_low() {
                // 松开后再切换，避免长按连跳
                while button.is_low() {
                    Timer::after_millis(10).await;
                }
                CMD_CHANNEL
                    .try_send(proto::Control {
                        command: proto::Command::Next as i32,
                        ..Default::default()
                    })
                    .ok();
            }
        } else if !is_down {
            pressed = false;
        }
    }
}

/// 把三块灯板各自的图片刷上去（按给定全局亮度缩放）
fn draw_all(ws2812: &mut Ws2812Panels, panel_index: &[usize; NUM_PANELS], brightness: u8) {
    for panel in 0..NUM_PANELS {
        draw_image(ws2812, panel, panel_index[panel], brightness);
    }
    ws2812.show_all().unwrap();
}

/// 混合刷新：滚动中的板按 rows 拼帧直刷，静止板按图片索引刷
fn draw_mixed(
    ws2812: &mut Ws2812Panels,
    panel_index: &[usize; NUM_PANELS],
    lottery: &[bool; NUM_PANELS],
    lottery_state: &[LotteryState; NUM_PANELS],
    brightness: u8,
) {
    for panel in 0..NUM_PANELS {
        if lottery[panel] {
            draw_lottery(ws2812, panel, &lottery_state[panel], brightness);
        } else {
            draw_image(ws2812, panel, panel_index[panel], brightness);
        }
    }
    ws2812.show_all().unwrap();
}

/// 把抽奖滚动屏（16 行行表）刷到指定灯板的帧缓冲里（不发送，需调 show_all 生效）。
/// 灯珠线性索引与 (x, y) 的映射经 pixel_coords（含蛇形），此处逐灯珠反查坐标。
fn draw_lottery(ws2812: &mut Ws2812Panels, panel: usize, state: &LotteryState, brightness: u8) {
    for led in 0..NUM_LEDS {
        let (x, y) = pixel_coords(led);
        let (img, row) = state.rows[y];
        let data = images::IMAGES[img as usize];
        let Some(pixel_base) = ppm_pixel_offset(data) else {
            continue;
        };
        let src = pixel_base + (row as usize * WIDTH + x) * 3;
        let r = scale(data[src], brightness);
        let g = scale(data[src + 1], brightness);
        let b = scale(data[src + 2], brightness);
        ws2812.set_pixel(panel, led, g, r, b);
    }
}

/// 解析 PPM 并把指定图片刷到指定灯板的帧缓冲里（不发送，需调 show_all 生效）
fn draw_image(ws2812: &mut Ws2812Panels, panel: usize, index: usize, brightness: u8) {
    let data = images::IMAGES[index];
    let pixel_base = match ppm_pixel_offset(data) {
        Some(offset) => offset,
        None => {
            println!("第 {index} 张图片 PPM 格式错误，跳过");
            return;
        }
    };

    for i in 0..NUM_LEDS {
        let (x, y) = pixel_coords(i);
        let src = pixel_base + (y * WIDTH + x) * 3;
        let r = scale(data[src], brightness);
        let g = scale(data[src + 1], brightness);
        let b = scale(data[src + 2], brightness);
        // WS2812 数据顺序为 G-R-B
        ws2812.set_pixel(panel, i, g, r, b);
    }
}

/// 把调试帧缓冲（16x16 RGB888）直刷到三块灯板上（同显，按给定亮度缩放）
fn draw_debug_all(
    ws2812: &mut Ws2812Panels,
    frame: &[u8; DEBUG_FRAME_BYTES],
    brightness: u8,
) {
    for panel in 0..NUM_PANELS {
        for i in 0..NUM_LEDS {
            let (x, y) = pixel_coords(i);
            let src = (y * WIDTH + x) * 3;
            let r = scale(frame[src], brightness);
            let g = scale(frame[src + 1], brightness);
            let b = scale(frame[src + 2], brightness);
            ws2812.set_pixel(panel, i, g, r, b);
        }
    }
    ws2812.show_all().unwrap();
}

/// 线性索引 -> (x, y)，支持蛇形布线
fn pixel_coords(index: usize) -> (usize, usize) {
    let y = index / WIDTH;
    let mut x = index % WIDTH;
    if SERPENTINE && y % 2 == 1 {
        x = WIDTH - 1 - x;
    }
    (x, y)
}

/// 按亮度等比缩放（整数近似 v * brightness / 256）
fn scale(value: u8, brightness: u8) -> u8 {
    (((value as u16) * (brightness as u16 + 1)) >> 8) as u8
}

/// 解析 P6 PPM 头部，返回像素数据起始偏移
fn ppm_pixel_offset(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 2 || &bytes[..2] != b"P6" {
        return None;
    }
    let mut pos = 2usize;
    let read_num = |bytes: &[u8], pos: &mut usize| -> Option<u32> {
        while *pos < bytes.len() {
            let c = bytes[*pos];
            if c == b'#' {
                while *pos < bytes.len() && bytes[*pos] != b'\n' {
                    *pos += 1;
                }
            } else if c.is_ascii_whitespace() {
                *pos += 1;
            } else {
                break;
            }
        }
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if start == *pos {
            return None;
        }
        core::str::from_utf8(&bytes[start..*pos]).ok()?.parse().ok()
    };

    let width = read_num(bytes, &mut pos)?;
    let height = read_num(bytes, &mut pos)?;
    let maxval = read_num(bytes, &mut pos)?;
    if maxval != 255 || width as usize != WIDTH || height as usize != HEIGHT {
        return None;
    }
    // maxval 之后恰好一个空白字符即为像素数据
    if pos >= bytes.len() || bytes.len() < pos + 1 + WIDTH * HEIGHT * 3 {
        return None;
    }
    Some(pos + 1)
}

/// 帧缓冲：三块灯板，每块 NUM_LEDS 颗灯珠，每颗 G-R-B 3 字节
static mut PIXELS: [u8; NUM_PANELS * NUM_LEDS * 3] = [0; NUM_PANELS * NUM_LEDS * 3];

/// 调试帧缓冲：16x16 RGB888，由 console 经 FRAME_CHUNK 分片上传、拼装后直显
static mut DEBUG_FRAME: [u8; DEBUG_FRAME_BYTES] = [0; DEBUG_FRAME_BYTES];

/// 整帧 RMT 波形缓冲（约 24KB，必须放静态存储）
static mut CODES: [PulseCode; FRAME_CODES] =
    [PulseCode::new(Level::Low, 0, Level::Low, 0); FRAME_CODES];

/// 三块 WS2812 灯板驱动：每块独占一个 RMT 通道 + 一块帧缓冲，
/// 共用一条波形缓冲（逐板编码 + 逐板发送，省 ~48KB RAM）
struct Ws2812Panels {
    channels: [Option<esp_hal::rmt::Channel<'static, Blocking, Tx>>; NUM_PANELS],
    buffers: [&'static mut [u8; NUM_LEDS * 3]; NUM_PANELS],
    codes: &'static mut [PulseCode; FRAME_CODES],
}

impl Ws2812Panels {
    fn new(
        channels: [esp_hal::rmt::Channel<'static, Blocking, Tx>; NUM_PANELS],
        buffers: [&'static mut [u8; NUM_LEDS * 3]; NUM_PANELS],
        codes: &'static mut [PulseCode; FRAME_CODES],
    ) -> Self {
        let [c0, c1, c2] = channels;
        Self {
            channels: [Some(c0), Some(c1), Some(c2)],
            buffers,
            codes,
        }
    }

    /// 设置第 panel 块板上第 index 颗灯珠的颜色（调用方已按 G-R-B 顺序传入）
    fn set_pixel(&mut self, panel: usize, index: usize, g: u8, r: u8, b: u8) {
        let base = index * 3;
        self.buffers[panel][base] = g;
        self.buffers[panel][base + 1] = r;
        self.buffers[panel][base + 2] = b;
    }

    /// 把三块板的帧缓冲逐板发送出去。
    /// esp-hal 的阻塞 transmit 支持远超 RMT RAM（64 条）的数据，
    /// wait() 内部会在阈值事件时自动向硬件续写，因此每块板一次发送即可。
    /// 板间是串行发送：后板晚约一帧时间（16x16 全刷约 8ms），三块板肉眼无感。
    fn show_all(&mut self) -> Result<(), ()> {
        for panel in 0..NUM_PANELS {
            self.show_one(panel)?;
        }
        Ok(())
    }

    fn show_one(&mut self, panel: usize) -> Result<(), ()> {
        let channel = self.channels[panel].take().ok_or(())?;
        let codes = &mut *self.codes;
        let mut count = 0usize;
        for &byte in self.buffers[panel].iter() {
            write_byte(&mut codes[count..], byte);
            count += 8;
        }
        // 末尾补一条 >=50us 低电平复位码，确保灯珠锁存
        codes[count] = PulseCode::new(Level::Low, RESET_TICKS, Level::Low, RESET_TICKS);
        count += 1;
        codes[count] = PulseCode::end_marker();
        match channel.transmit(&codes[..=count]) {
            Ok(transaction) => match transaction.wait() {
                Ok(ch) => {
                    self.channels[panel] = Some(ch);
                    Ok(())
                }
                Err((_, ch)) => {
                    self.channels[panel] = Some(ch);
                    Err(())
                }
            },
            Err((_, ch)) => {
                self.channels[panel] = Some(ch);
                Err(())
            }
        }
    }
}

/// 把一个字节展开为 8 条 WS2812 波形（MSB 优先）
fn write_byte(out: &mut [PulseCode], byte: u8) {
    for (i, slot) in out.iter_mut().enumerate().take(8) {
        let one = byte & (0x80 >> i) != 0;
        *slot = if one {
            PulseCode::new(Level::High, T1H, Level::Low, T1L)
        } else {
            PulseCode::new(Level::High, T0H, Level::Low, T0L)
        };
    }
}
