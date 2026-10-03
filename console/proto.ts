// protobuf 编解码：消息格式的唯一真实来源是 `proto/display.proto`，
// 固件侧用 prost，console 侧用 protobuf.js 在运行时 parse 同一份 `.proto`。
import * as protobuf from 'protobufjs';
import protoText from '../proto/display.proto?raw';

const protoRoot = protobuf.parse(protoText).root;
const ControlMessage = protoRoot.lookupType('kangel.display.Control');
const StatusMessage = protoRoot.lookupType('kangel.display.Status');
const CommandEnum = protoRoot.lookupEnum('kangel.display.Command');

export const Command = {
  NEXT: CommandEnum.values.NEXT,
  AUTO_ON: CommandEnum.values.AUTO_ON,
  AUTO_OFF: CommandEnum.values.AUTO_OFF,
  TOGGLE: CommandEnum.values.TOGGLE,
  GOTO: CommandEnum.values.GOTO,
  DEBUG_ENTER: CommandEnum.values.DEBUG_ENTER,
  DEBUG_EXIT: CommandEnum.values.DEBUG_EXIT,
  SET_BRIGHTNESS: CommandEnum.values.SET_BRIGHTNESS,
  FRAME_CHUNK: CommandEnum.values.FRAME_CHUNK,
  FRAME_SEQ: CommandEnum.values.FRAME_SEQ,
  LOTTERY_START: CommandEnum.values.LOTTERY_START,
  LOTTERY_STOP: CommandEnum.values.LOTTERY_STOP,
};

export const COMMAND_NAMES = Object.fromEntries(
  Object.entries(CommandEnum.valuesById).map(([id, name]) => [Number(id), name]),
) as Record<number, string>;

export interface ControlOpts {
  index?: number
  brightness?: number
  offset?: number
  pixels?: Uint8Array
  last?: boolean
  panel?: number
  speed?: number
  lotteryMask?: Uint8Array
}

export function encodeControl(command: number, opts: ControlOpts = {}): Uint8Array {
  const { index = 0, brightness = 0, offset = 0, pixels = new Uint8Array(0), last = false, panel = 0, speed = 0, lotteryMask = new Uint8Array(0) } = opts;
  const error = ControlMessage.verify({ command, index, brightness, offset, pixels, last, panel, speed, lotteryMask });
  if (error)
    throw new Error(`非法控制命令：${error}`);
  return ControlMessage.encode({ command, index, brightness, offset, pixels, last, panel, speed, lotteryMask }).finish();
}

export interface DeviceStatus {
  index: number
  auto: boolean
  debug: boolean
  brightness: number
  frameSeq: number
  panelIndex: number[]
  lottery: boolean[]
}

export function parseStatus(bytes: Uint8Array): DeviceStatus {
  const msg = StatusMessage.decode(bytes) as unknown as Record<string, unknown>;
  const index = (msg.index as number) ?? 0;
  // 老固件没有 panelIndex/lottery 字段：三块板都按 index 高亮、无滚动标志
  const raw = msg.panelIndex as number[] | undefined;
  const panelIndex = Array.isArray(raw) && raw.length > 0
    ? [raw[0] ?? index, raw[1] ?? index, raw[2] ?? index]
    : [index, index, index];
  const rawLot = msg.lottery as boolean[] | undefined;
  const lottery = Array.isArray(rawLot) && rawLot.length > 0
    ? [!!rawLot[0], !!rawLot[1], !!rawLot[2]]
    : [false, false, false];
  return {
    index,
    auto: (msg.auto as boolean) ?? false,
    debug: (msg.debug as boolean) ?? false,
    brightness: (msg.brightness as number) ?? 0,
    frameSeq: (msg.frameSeq as number) ?? 0,
    panelIndex,
    lottery,
  };
}
