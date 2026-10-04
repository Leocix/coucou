// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, type ChatContext } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";
import {
  CLAUDE_MODELS, PROVIDER_COLOR, geminiModels, modelLabel, providerOf, type ModelOption,
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
  const screenBtn = h(
    "button",
    { class: "screen-btn", title: "Ask about my screen" },
    svg(ICONS.monitor, 13),
  );
  const bar = h("div", { class: "chat-bar" }, input, screenBtn, send);

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
  const modelRow = h("div", { class: "model-row" }, modelBtn, modelMenu);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, modelRow, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let renderedCount = -1;
  /** Armed by the screen button: the next message carries a screen capture. */
  let screenArmed = false;

  function setScreenArmed(on: boolean) {
    screenArmed = on;
    screenBtn.classList.toggle("armed", on);
    State.notify();
    onHeightChange(); // the "Screen" chip comes and goes
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null = screenArmed
      ? { kind: "screen" }
      : State.chatHistory.length === 1 && file
        ? { kind: "file", name: file.name, path: file.path }
        : null;
    // One capture per click: the button disarms once the message is on its way.
    if (screenArmed) setScreenArmed(false);

    try {
      const reply = await Bridge.chatSend(query, context);
      State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  let menuOpen = false;

  function modelChip(m: ModelOption): HTMLElement {
    const chip = h(
      "button",
      { class: `model-chip${m.id === State.settings.model ? " current" : ""}` },
      h("i", { class: "model-dot", style: `background:${PROVIDER_COLOR[providerOf(m.id)]}` }),
      h("span", { text: m.label }),
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
      for (const m of CLAUDE_MODELS) modelMenu.append(modelChip(m));
      // Refreshed on every open: the Google key may have just been saved in Settings.
      const gemini = await geminiModels(true);
      if (!menuOpen) return;
      for (const m of gemini) modelMenu.append(modelChip(m));
    }
    State.notify();
  }

  modelBtn.addEventListener("click", () => void setMenuOpen(!menuOpen));

  send.addEventListener("click", () => void submit());
  screenBtn.addEventListener("click", () => {
    if (sending) return;
    setScreenArmed(!screenArmed);
    Sound.play(screenArmed ? "attach" : "close");
    input.focus();
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
      const wantChip = screenArmed ? "Screen" : (file?.name ?? "");
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const thinking = State.stateOverride === "thinking";
      const count = State.chatHistory.length + (thinking ? 0.5 : 0);
      if (count !== renderedCount) {
        renderedCount = count;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = screenArmed
        ? "Ask about your screen…"
        : State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      input.disabled = sending;
      const model = State.settings.model;
      if (modelName.textContent !== modelLabel(model)) modelName.textContent = modelLabel(model);
      modelDot.style.background = PROVIDER_COLOR[providerOf(model)];
      (screenBtn as HTMLButtonElement).disabled = sending;
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
