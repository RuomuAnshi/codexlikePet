import type { PetManifest } from "./atlas";
import type { PetAnimationPack } from "./animations";

export interface PetSettings {
  scale: number;
  opacity: number;
  speed: number;
  wanderEnabled: boolean;
  clickThrough: boolean;
  lockPosition: boolean;
  quietMode: boolean;
  showInFullscreen: boolean;
  paused: boolean;
  circadianEnabled: boolean;
  sleepStartMinutes: number;
  wakeMinutes: number;
  socialEnabled: boolean;
  windowInteractionEnabled: boolean;
}

export interface PetSettingsEvent {
  petId: string;
  settings: PetSettings;
}

export interface PetDialogue {
  version: number;
  doubleClick: string[];
  click: string[];
  rightClick: string[];
  walk: string[];
  drag: string[];
  idle: string[];
  morning: string[];
  evening: string[];
  sleep: string[];
  wake: string[];
  petting: string[];
  feed: string[];
  play: string[];
  pickup: string[];
  putDown: string[];
  lowBattery: string[];
  breakReminder: string[];
  reunion: string[];
  milestone: string[];
}

export interface PetInstanceInfo {
  id: string;
  petId: string;
  visible: boolean;
  isMain: boolean;
  position?: { x: number; y: number } | null;
}

export interface InstalledPetInfo {
  id: string;
  displayName: string;
  description: string;
  spriteVersionNumber: number;
  spritesheetPath: string;
  source: "bundled" | "imported";
  enabled: boolean;
  previewDataUrl: string | null;
  path: string | null;
  settings: PetSettings;
}

export interface RuntimeConfig {
  instanceId: string;
  petId: string;
  source: "bundled" | "imported";
  path: string | null;
  manifest: PetManifest | null;
  spritesheetDataUrl: string | null;
  settings: PetSettings;
  dialogue: PetDialogue;
  character?: CharacterCard;
  animationPack?: PetAnimationPack | null;
}

export interface CharacterCard {
  name: string;
  description: string;
  personality: string;
  scenario: string;
  firstMes: string;
  mesExample: string;
  systemPrompt: string;
  postHistoryInstructions: string;
}

export type ProviderKind = "openai-responses" | "anthropic-messages" | "openai-compatible";

export interface ModelEndpointConfig {
  provider: ProviderKind;
  baseUrl: string;
  model: string;
  credentialRef: string | null;
  maxOutputTokens: number;
}

export interface AiSettings {
  enabled: boolean;
  chatModel: ModelEndpointConfig | null;
  visionModel: ModelEndpointConfig | null;
  memoryEnabled: boolean;
  maxRecentMessages: number;
  heartbeatEnabled: boolean;
  heartbeatMinMinutes: number;
  heartbeatMaxMinutes: number;
  heartbeatVisionChance: number;
  desktopVisionEnabled: boolean;
  petConversationEnabled: boolean;
}

export interface PetLifeState {
  mood: string;
  energy: number;
  attention: number;
  bond: number;
  activity: string;
  sleepReason: string;
  lastInteractionAt: number;
  lastSpokeAt: number;
  knownSince: number;
  interactionCount: number;
  chatCount: number;
  petInteractionCount: number;
  nextActionAt: number;
  moodValue: number;
  relationshipLevel: number;
  peakBond: number;
  lastAdvancedAt: number;
  energyProgressMs: number;
  attentionProgressMs: number;
  lastBondDecayAt: number;
  sleepingSince: number;
  sleepOverrideUntil: number;
  lastGreetingDate: string;
  lastFedAt: number;
  lastPlayedAt: number;
  lastPettedAt: number;
  unlockedMilestones: string[];
}

export interface PetPairRelationship {
  pairId: string;
  affinity: number;
  peakAffinity: number;
  level: number;
  knownSince: number;
  interactionCount: number;
  lastInteractionAt: number;
  lastAdvancedAt: number;
  unlockedMilestones: string[];
}

export interface EnvironmentSettings {
  foregroundTrackingEnabled: boolean;
  breakReminderEnabled: boolean;
  breakReminderMinutes: number;
  meetingQuietEnabled: boolean;
  lowBatteryEnabled: boolean;
  lowBatteryThreshold: number;
  notificationEventsEnabled: boolean;
  codingApps: string[];
  meetingApps: string[];
}

export interface PetBehavior {
  say: string;
  action: "idle" | "waving" | "jumping" | "failed" | "waiting" | "running" | "review" | "walk" | "sleep";
  mode?: "idle" | "working" | "waiting" | "sleeping" | null;
  gesture?: string | null;
  mood: string;
  duration: number;
  nextActionAfter: number;
  look?: string | null;
}

export interface MemoryFact {
  id: string;
  content: string;
  kind: string;
  scope: string;
  importance: number;
  confidence: number;
  createdAt: number;
  updatedAt: number;
  status: string;
  expiresAt?: number | null;
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  timestamp: number;
  source: string;
  visionSummary?: string | null;
  speakerPetId?: string | null;
  speakerName?: string | null;
  behavior?: PetBehavior | null;
}
