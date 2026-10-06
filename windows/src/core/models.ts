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
  return CLAUDE_MODELS.find((m) => m.id === id)?.label ?? geminiLabel(id);
}

/** "gemini-3.1-flash-lite-preview" → "Gemini 3.1 Flash Lite (preview)". */
export function geminiLabel(id: string): string {
  if (!isGemini(id)) return id;
  const tags: string[] = [];
  const words = id.split("-").filter((w) => {
    if (w === "preview" || w === "latest" || w === "exp") {
      tags.push(w);
      return false;
    }
    return w.length > 0;
  });
  const name = words.map((w) => (/^\d/.test(w) ? w : w[0].toUpperCase() + w.slice(1))).join(" ");
  return tags.length ? `${name} (${tags.join(", ")})` : name;
}

const TIERS = ["flash", "pro", "flash-lite"] as const;
const TIER_ID = /^gemini-(\d+(?:\.\d+)?)-(pro|flash|flash-lite)(-preview)?$/;

/**
 * Splits the Gemini list in two: the newest Flash, Pro and Flash Lite, which
 * is what most people want, and everything else (aliases, dated previews,
 * special-purpose models) behind "More".
 */
export function splitGemini(list: ModelOption[]): { main: ModelOption[]; more: ModelOption[] } {
  const newer = (a: string, b: string) => a.localeCompare(b, undefined, { numeric: true }) > 0;
  const best = new Map<string, { m: ModelOption; version: string; preview: boolean }>();
  for (const m of list) {
    const match = TIER_ID.exec(m.id);
    if (!match) continue;
    const version = match[1];
    const preview = Boolean(match[3]);
    const current = best.get(match[2]);
    // Newest version wins; at the same version, the stable one.
    if (
      !current ||
      newer(version, current.version) ||
      (version === current.version && current.preview && !preview)
    ) {
      best.set(match[2], { m, version, preview });
    }
  }
  const main = TIERS.flatMap((t) => (best.has(t) ? [best.get(t)!.m] : []));
  const more = list
    .filter((m) => !main.includes(m))
    // Numbered models first, newest on top; then the aliases and special ones.
    .sort((a, b) => {
      const va = /^gemini-\d/.test(a.id);
      const vb = /^gemini-\d/.test(b.id);
      if (va !== vb) return va ? -1 : 1;
      const order = a.id.localeCompare(b.id, undefined, { numeric: true });
      return va ? -order : order;
    });
  return { main, more };
}

/** The Gemini model dictation uses: the newest Flash the key can reach. */
export async function dictationModel(): Promise<string> {
  const { main } = splitGemini(await geminiModels());
  return main.find((m) => TIER_ID.exec(m.id)?.[2] === "flash")?.id ?? main[0]?.id ?? "gemini-2.5-flash";
}

let geminiCache: ModelOption[] | null = null;

/** Gemini models the saved Google key can use; `refresh` after the key changes. */
export async function geminiModels(refresh = false): Promise<ModelOption[]> {
  if (geminiCache && !refresh) return geminiCache;
  const list = await Bridge.geminiModels();
  geminiCache = list && list.length > 0 ? list : GEMINI_FALLBACK;
  return geminiCache;
}
