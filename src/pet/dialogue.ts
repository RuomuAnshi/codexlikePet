import type { PetDialogue } from "./config";

export const DEFAULT_DIALOGUE: PetDialogue = {
  version: 1,
  doubleClick: ["嗯？找我吗？", "今天也一起玩吧。"],
  click: ["怎么啦？", "我在这里哦。"],
  rightClick: ["轻一点嘛。"],
  walk: ["我去附近转转。", "散步时间到了！"],
  drag: ["要带我去哪里呀？", "我来啦！"],
  idle: ["这里待着也很舒服。", "要不要陪我说说话？"],
  morning: ["早上好呀，今天也要一起度过。"],
  evening: ["晚上啦，今天辛苦了。"],
  sleep: ["我先睡一会儿，晚安。"],
  wake: ["唔……醒来了。早上好！"],
  petting: ["呼噜呼噜……再摸一会儿嘛。"],
  feed: ["谢谢投喂！好好吃。"],
  play: ["来玩一会儿吧！"],
  pickup: ["诶、诶？我被拎起来啦！"],
  putDown: ["呼……落地了。"],
  lowBattery: ["电量好低了，要不要休息一下？"],
  breakReminder: ["已经忙很久啦，起来活动一下吧。"],
  reunion: ["你回来啦……我有一点想你。"],
  milestone: ["我们又一起走过一段时间了。"],
};

function normalizeLines(value: unknown, fallback: string[]): string[] {
  if (!Array.isArray(value)) return fallback;
  return value
    .filter((line): line is string => typeof line === "string")
    .map((line) => line.trim())
    .filter(Boolean)
    .slice(0, 32);
}

function normalizeDialogue(value: unknown): PetDialogue {
  if (!value || typeof value !== "object") return DEFAULT_DIALOGUE;
  const source = value as {
    version?: unknown;
    doubleClick?: unknown;
    click?: unknown;
    rightClick?: unknown;
    walk?: unknown;
    drag?: unknown;
    idle?: unknown;
    morning?: unknown;
    evening?: unknown;
    sleep?: unknown;
    wake?: unknown;
    petting?: unknown;
    feed?: unknown;
    play?: unknown;
    pickup?: unknown;
    putDown?: unknown;
    lowBattery?: unknown;
    breakReminder?: unknown;
    reunion?: unknown;
    milestone?: unknown;
  };
  const doubleClick = normalizeLines(source.doubleClick, DEFAULT_DIALOGUE.doubleClick);
  return {
    version: source.version === 1 ? 1 : DEFAULT_DIALOGUE.version,
    doubleClick: doubleClick.length ? doubleClick : DEFAULT_DIALOGUE.doubleClick,
    click: normalizeLines(source.click, DEFAULT_DIALOGUE.click),
    rightClick: normalizeLines(source.rightClick, DEFAULT_DIALOGUE.rightClick),
    walk: normalizeLines(source.walk, DEFAULT_DIALOGUE.walk),
    drag: normalizeLines(source.drag, DEFAULT_DIALOGUE.drag),
    idle: normalizeLines(source.idle, DEFAULT_DIALOGUE.idle),
    morning: normalizeLines(source.morning, DEFAULT_DIALOGUE.morning),
    evening: normalizeLines(source.evening, DEFAULT_DIALOGUE.evening),
    sleep: normalizeLines(source.sleep, DEFAULT_DIALOGUE.sleep),
    wake: normalizeLines(source.wake, DEFAULT_DIALOGUE.wake),
    petting: normalizeLines(source.petting, DEFAULT_DIALOGUE.petting),
    feed: normalizeLines(source.feed, DEFAULT_DIALOGUE.feed),
    play: normalizeLines(source.play, DEFAULT_DIALOGUE.play),
    pickup: normalizeLines(source.pickup, DEFAULT_DIALOGUE.pickup),
    putDown: normalizeLines(source.putDown, DEFAULT_DIALOGUE.putDown),
    lowBattery: normalizeLines(source.lowBattery, DEFAULT_DIALOGUE.lowBattery),
    breakReminder: normalizeLines(source.breakReminder, DEFAULT_DIALOGUE.breakReminder),
    reunion: normalizeLines(source.reunion, DEFAULT_DIALOGUE.reunion),
    milestone: normalizeLines(source.milestone, DEFAULT_DIALOGUE.milestone),
  };
}

export async function loadDialogue(baseUrl: string): Promise<PetDialogue> {
  try {
    const response = await fetch(`${baseUrl}/character.json`);
    if (!response.ok) return DEFAULT_DIALOGUE;
    return normalizeDialogue(await response.json());
  } catch {
    return DEFAULT_DIALOGUE;
  }
}
