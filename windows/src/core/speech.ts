// Reads chat answers aloud with the voices installed in Windows
// (speechSynthesis), a sentence at a time while the answer streams in.

const KEY = "coucou.speakReplies";
const BOUNDARY = /[.!?…:;](?=\s)|\n/g;

let pending = "";

function voiceFor(lang: string): SpeechSynthesisVoice | undefined {
  const voices = speechSynthesis.getVoices();
  const base = lang.split("-")[0].toLowerCase();
  return (
    voices.find((v) => v.lang.toLowerCase() === lang.toLowerCase()) ??
    voices.find((v) => v.lang.toLowerCase().startsWith(base))
  );
}

function say(text: string) {
  const clean = text.trim();
  if (!clean) return;
  const utterance = new SpeechSynthesisUtterance(clean);
  // Mochi answers in the user's language, which is usually the system's.
  const lang = navigator.language || "en-US";
  utterance.lang = lang;
  const voice = voiceFor(lang);
  if (voice) utterance.voice = voice;
  speechSynthesis.speak(utterance);
}

export const Speech = {
  available: typeof window !== "undefined" && "speechSynthesis" in window,

  get enabled(): boolean {
    try {
      return Speech.available && localStorage.getItem(KEY) === "1";
    } catch {
      return false;
    }
  },

  set enabled(on: boolean) {
    try {
      localStorage.setItem(KEY, on ? "1" : "0");
    } catch {
      // Not remembered across launches; still applies now.
    }
    if (!on) Speech.stop();
  },

  /** A new piece of the answer: speaks every sentence that is now complete. */
  feed(piece: string) {
    if (!Speech.enabled) return;
    pending += piece;
    let end = -1;
    for (const match of pending.matchAll(BOUNDARY)) end = match.index + match[0].length;
    if (end > 0) {
      say(pending.slice(0, end));
      pending = pending.slice(end);
    }
  },

  /** The answer is complete: speak what is left. */
  finish() {
    if (Speech.enabled) say(pending);
    pending = "";
  },

  stop() {
    pending = "";
    if (Speech.available) speechSynthesis.cancel();
  },
};
