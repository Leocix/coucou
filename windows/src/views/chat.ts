// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, onEvent, type ChatContext } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";
import { Speech } from "../core/speech";
import {
  CLAUDE_MODELS, PROVIDER_COLOR, dictationModel, geminiLabel, geminiModels, modelLabel, providerOf,
  splitGemini, type ModelOption,
} from "../core/models";

let nextId = 1;

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

type Attachment =
  | { kind: "screen" }
  | { kind: "image"; source: "area" | "clipboard"; label: string }
  | { kind: "text"; text: string; label: string };

function attachmentLabel(a: Attachment): string {
  return a.kind === "screen" ? "Screen" : a.label;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const clipBtn = h(
    "button",
    { class: "screen-btn", title: "Use what I copied" },
    svg(ICONS.clipboard, 13),
  );
  const areaBtn = h(
    "button",
    { class: "screen-btn", title: "Capture an area of the screen" },
    svg(ICONS.crop, 13),
  );
  const micBtn = h(
    "button",
    { class: "screen-btn", title: "Dictate (click again to stop)" },
    svg(ICONS.mic, 13),
  );
  const screenBtn = h(
    "button",
    { class: "screen-btn", title: "Ask about my screen" },
    svg(ICONS.monitor, 13),
  );
  const bar = h("div", { class: "chat-bar" }, input, micBtn, clipBtn, areaBtn, screenBtn, send);

  // Model picker: which model answers, and a one-click switch between Claude
  // and Gemini without opening Settings.
  const modelDot = h("i", { class: "model-dot" });
  const modelName = h("span", {});
  const modelBtn = h(
    "button",
    { class: "model-btn", title: "Change model" },
    modelDot,
    modelName,
    svg(ICONS.chevronRight, 9, { stroke: 2.4 }),
  );
  const modelMenu = h("div", { class: "model-menu" });
  const speakBtn = h("button", { class: "speak-btn" });
  const modelRow = h(
    "div",
    { class: "model-row" },
    h("div", { class: "model-head" }, modelBtn, Speech.available ? speakBtn : null),
    modelMenu,
  );

  function syncSpeak() {
    const on = Speech.enabled;
    clear(speakBtn);
    speakBtn.append(svg(on ? ICONS.speakerOn : ICONS.speakerOff, 12));
    speakBtn.classList.toggle("on", on);
    speakBtn.title = on ? "Reading answers aloud (click to stop)" : "Read answers aloud";
  }
  syncSpeak();
  speakBtn.addEventListener("click", () => {
    Speech.enabled = !Speech.enabled;
    syncSpeak();
    Sound.play("pop");
  });

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, modelRow, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderedCount = -1;
  /** The answer being written, shown piece by piece while it streams in. */
  let streaming: ChatMessage | null = null;
  let heightQueued = false;

  void onEvent<string>("chat-delta", (piece) => {
    if (!sending) return;
    if (!streaming) {
      streaming = { id: nextId++, role: "assistant", content: "" };
      State.chatHistory.push(streaming);
    }
    streaming.content += piece;
    Speech.feed(piece);
    State.notify();
    // The bubble grows line by line: re-measure once per frame, not per piece.
    if (!heightQueued) {
      heightQueued = true;
      requestAnimationFrame(() => {
        heightQueued = false;
        onHeightChange();
      });
    }
  });
  /**
   * What the next message carries, set by the buttons next to the field. Each
   * one covers a single message and is dropped once it is sent.
   */
  let attachment: Attachment | null = null;
  /** Waiting for the snipping overlay. */
  let capturing = false;

  function setAttachment(next: Attachment | null) {
    // An image waiting in Rust that the user dropped must not ride along later.
    if (attachment?.kind === "image" && next?.kind !== "image") void Bridge.clearPendingImage();
    attachment = next;
    screenBtn.classList.toggle("armed", next?.kind === "screen");
    areaBtn.classList.toggle("armed", next?.kind === "image" && next.source === "area");
    clipBtn.classList.toggle(
      "armed",
      next?.kind === "text" || (next?.kind === "image" && next.source === "clipboard"),
    );
    State.notify();
    onHeightChange(); // the chip comes and goes
  }

  function showError(message: string) {
    State.noteMessage = message.replace(/^Error:\s*/, "");
    State.view = "note";
    Sound.play("error");
    State.notify();
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending || recording) return;
    input.value = "";
    Speech.stop(); // a new question interrupts the last answer
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const sent = attachment;
    let context: ChatContext | null =
      State.chatHistory.length === 1 && file ? { kind: "file", name: file.name, path: file.path } : null;
    let prompt = query;
    if (sent?.kind === "screen") context = { kind: "screen" };
    if (sent?.kind === "image") context = { kind: "image" };
    if (sent?.kind === "text") prompt = `Text I copied:\n"""\n${sent.text}\n"""\n\n${query}`;
    // One attachment per click. Rust takes a waiting image itself, so forget
    // it here rather than asking Rust to drop it.
    attachment = null;
    if (sent) setAttachment(null);

    try {
      const reply = await Bridge.chatSend(prompt, context);
      if (streaming) {
        streaming.content = reply.text;
        Speech.finish();
      } else {
        State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
        Speech.feed(reply.text);
        Speech.finish();
      }
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      // A cut-off or declined answer doesn't stay half-written in the chat.
      if (streaming) State.chatHistory.splice(State.chatHistory.indexOf(streaming), 1);
      Speech.stop();
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      streaming = null;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  let menuOpen = false;

  let showAllGemini = false;

  function modelChip(m: ModelOption): HTMLElement {
    const chip = h(
      "button",
      { class: `model-chip${m.id === State.settings.model ? " current" : ""}`, title: m.id },
      h("i", { class: "model-dot", style: `background:${PROVIDER_COLOR[providerOf(m.id)]}` }),
      h("span", { text: providerOf(m.id) === "gemini" ? geminiLabel(m.id) : m.label }),
    );
    chip.addEventListener("click", () => {
      if (m.id !== State.settings.model) {
        State.settings.model = m.id;
        void Bridge.saveSettings(State.settings);
        Sound.play("pop");
      }
      setMenuOpen(false);
      input.focus();
    });
    return chip;
  }

  async function setMenuOpen(open: boolean) {
    menuOpen = open;
    modelRow.classList.toggle("open", open);
    clear(modelMenu);
    if (open) {
      modelMenu.append(modelGroup("Claude", CLAUDE_MODELS));
      // Refreshed on every open: the Google key may have just been saved in Settings.
      const { main, more } = splitGemini(await geminiModels(true));
      if (!menuOpen) return;
      // The model in use always shows, even when it sits under "More".
      const current = more.find((m) => m.id === State.settings.model);
      const shown = current && !showAllGemini ? [...main, current] : main;
      const gemini = modelGroup("Gemini", shown);
      if (more.length > 0) {
        const toggle = h("button", {
          class: "model-chip more",
          text: showAllGemini ? "Fewer" : `More (${more.length})`,
        });
        toggle.addEventListener("click", () => {
          showAllGemini = !showAllGemini;
          void setMenuOpen(true);
        });
        gemini.querySelector(".model-chips")!.append(toggle);
      }
      modelMenu.append(gemini);
      if (showAllGemini && more.length > 0) {
        const all = h("div", { class: "model-chips model-all" });
        for (const m of more) all.append(modelChip(m));
        modelMenu.append(all);
      }
    }
    State.notify();
    onHeightChange();
  }

  function modelGroup(name: string, models: ModelOption[]): HTMLElement {
    const chips = h("div", { class: "model-chips" });
    for (const m of models) chips.append(modelChip(m));
    return h("div", { class: "model-group" }, h("span", { class: "model-group-name", text: name }), chips);
  }

  modelBtn.addEventListener("click", () => void setMenuOpen(!menuOpen));

  // Dictation: first click records, second click stops and writes what was
  // said into the field, ready to edit or send.
  let recording = false;
  let transcribing = false;
  micBtn.addEventListener("click", async () => {
    if (sending || transcribing) return;
    if (!recording) {
      try {
        await Bridge.dictationStart();
        recording = true;
        Speech.stop();
        Sound.play("attach");
      } catch (err) {
        showError(String(err));
      }
      State.notify();
      return;
    }
    recording = false;
    transcribing = true;
    State.notify();
    try {
      const text = await Bridge.dictationStop(await dictationModel());
      input.value = input.value.trim() ? `${input.value.trim()} ${text}` : text;
      Sound.play("pop");
    } catch (err) {
      showError(String(err));
    } finally {
      transcribing = false;
      State.notify();
      input.focus();
    }
  });

  send.addEventListener("click", () => void submit());
  screenBtn.addEventListener("click", () => {
    if (sending) return;
    const on = attachment?.kind !== "screen";
    setAttachment(on ? { kind: "screen" } : null);
    Sound.play(on ? "attach" : "close");
    input.focus();
  });

  clipBtn.addEventListener("click", async () => {
    if (sending) return;
    if (clipBtn.classList.contains("armed")) {
      setAttachment(null);
      Sound.play("close");
      return;
    }
    try {
      const content = await Bridge.clipboardRead();
      if (content.kind === "text") {
        const chars = content.text.length.toLocaleString();
        setAttachment({
          kind: "text",
          text: content.text,
          label: `Copied text · ${chars} chars${content.truncated ? " (cut)" : ""}`,
        });
      } else {
        setAttachment({
          kind: "image",
          source: "clipboard",
          label: `Copied image · ${content.width}×${content.height}`,
        });
      }
      Sound.play("attach");
      input.focus();
    } catch (err) {
      showError(String(err));
    }
  });

  areaBtn.addEventListener("click", async () => {
    if (sending || capturing) return;
    if (areaBtn.classList.contains("armed")) {
      setAttachment(null);
      Sound.play("close");
      return;
    }
    capturing = true;
    areaBtn.classList.add("busy");
    try {
      const content = await Bridge.captureArea();
      if (content.kind === "image") {
        setAttachment({ kind: "image", source: "area", label: `Area · ${content.width}×${content.height}` });
        Sound.play("attach");
      }
    } catch (err) {
      showError(String(err));
    } finally {
      capturing = false;
      areaBtn.classList.remove("busy");
      input.focus();
    }
  });
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = attachment ? attachmentLabel(attachment) : (file?.name ?? "");
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      // The dots wait for the first words; then the answer itself shows progress.
      const thinking = State.stateOverride === "thinking" && !streaming;
      const count = State.chatHistory.length + (thinking ? 0.5 : 0);
      if (count !== renderedCount) {
        renderedCount = count;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      } else {
        // Same messages, but the last answer may have grown or been finalised.
        const last = State.chatHistory[State.chatHistory.length - 1];
        const reply = log.lastElementChild?.querySelector(".reply");
        if (last?.role === "assistant" && reply && reply.textContent !== last.content) {
          reply.textContent = last.content;
          log.scrollTop = log.scrollHeight;
        }
      }

      input.placeholder = recording
        ? "Listening… click the mic when you're done"
        : transcribing
          ? "Writing down what you said…"
          : attachment
            ? "Ask about it…"
            : State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending || transcribing;
      micBtn.classList.toggle("recording", recording);
      micBtn.classList.toggle("busy", transcribing);
      (micBtn as HTMLButtonElement).disabled = sending;
      const model = State.settings.model;
      if (modelName.textContent !== modelLabel(model)) modelName.textContent = modelLabel(model);
      modelDot.style.background = PROVIDER_COLOR[providerOf(model)];
      for (const b of [screenBtn, areaBtn, clipBtn]) (b as HTMLButtonElement).disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
