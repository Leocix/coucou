// Chat models offered in the island and in Settings. The model id decides the
// provider: anything starting with "gemini" goes to Google, the rest to Claude
// (see chat_send in lib.rs).

import { Bridge } from "./bridge";

export interface ModelOption {
  id: string;
  label: string;
}

export const CLAUDE_MODELS: ModelOption[] = [
  { id: "claude-opus-5", label: "Claude Opus 5" },
  { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
  { id: "claude-haiku-4-5", label: "Claude Haiku 4.5" },
];

/** Used until the Google key can list its own models. */
export const GEMINI_FALLBACK: ModelOption[] = [
  { id: "gemini-2.5-flash", label: "gemini-2.5-flash" },
  { id: "gemini-2.5-pro", label: "gemini-2.5-pro" },
];

/** Same accents as ChatProvider.accentHex on macOS. */
export const PROVIDER_COLOR = { claude: "#E07950", gemini: "#4285F4" } as const;

export function isGemini(id: string): boolean {
  return id.startsWith("gemini");
}

export function providerOf(id: string): "claude" | "gemini" {
  return isGemini(id) ? "gemini" : "claude";
}

export function modelLabel(id: string): string {
  return CLAUDE_MODELS.find((m) => m.id === id)?.label ?? id;
}

let geminiCache: ModelOption[] | null = null;

/** Gemini models the saved Google key can use; `refresh` after the key changes. */
export async function geminiModels(refresh = false): Promise<ModelOption[]> {
  if (geminiCache && !refresh) return geminiCache;
  const list = await Bridge.geminiModels();
  geminiCache = list && list.length > 0 ? list : GEMINI_FALLBACK;
  return geminiCache;
}
