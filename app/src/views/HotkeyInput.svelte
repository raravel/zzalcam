<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { t } from "../lib/i18n.svelte";

  let { value = $bindable(), conflict = false }: { value: string; conflict?: boolean } = $props();
  let listening = $state(false);

  // KeyboardEvent.code → the key names the hotkey parser accepts.
  function keyName(code: string): string | null {
    if (/^Digit\d$/.test(code)) return code.slice(5);
    if (/^Key[A-Z]$/.test(code)) return code.slice(3);
    if (/^F\d{1,2}$/.test(code)) return code;
    if (["Backquote", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon",
      "Quote", "Comma", "Period", "Slash", "Space", "Insert", "Home", "End", "PageUp", "PageDown",
      "Pause", "ScrollLock"].includes(code)) return code;
    return null;
  }

  async function begin() {
    listening = true;
    // The app's own hotkeys would fire while the user presses them here.
    await invoke("suspend_hotkeys", { suspend: true });
  }

  async function end() {
    listening = false;
    await invoke("suspend_hotkeys", { suspend: false });
  }

  function onkeydown(e: KeyboardEvent) {
    if (!listening) return;
    e.preventDefault();
    if (e.code === "Escape") return end();
    if (e.code === "Backspace" || e.code === "Delete") {
      value = "";
      return end();
    }
    const key = keyName(e.code);
    if (!key) return; // a modifier on its own: keep waiting
    const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
    // Bare keys would swallow normal typing system-wide; only F-keys and Pause/ScrollLock may stand alone.
    if (mods.length === 0 && !/^F\d|^Pause$|^ScrollLock$/.test(key)) return;
    value = [...mods, key].join("+");
    end();
  }
</script>

<button class="hotkey" class:listening class:conflict onclick={begin} onblur={() => listening && end()} {onkeydown}>
  {#if listening}
    {t("settings.hotkey.press")}
  {:else if value}
    {#each value.split("+") as part, i}
      {#if i > 0}<span class="plus">+</span>{/if}<kbd>{part}</kbd>
    {/each}
  {:else}
    —
  {/if}
</button>

<style>
  .hotkey {
    min-width: 170px;
    height: 32px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 3px;
  }
  .listening {
    border-color: var(--accent);
    color: var(--accent);
  }
  .conflict {
    border-color: var(--danger);
  }
  kbd {
    font: inherit;
    font-size: 12px;
    padding: 1px 6px;
    border-radius: 4px;
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .plus {
    color: var(--muted);
    font-size: 11px;
  }
</style>
