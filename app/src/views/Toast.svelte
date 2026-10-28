<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { t } from "../lib/i18n.svelte";
  import type { Toast } from "../lib/types";

  const AUTO_HIDE_MS = 6000;
  let toast = $state<Toast | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;

  const hide = () => {
    clearTimeout(timer);
    getCurrentWindow().hide();
  };

  onMount(() => {
    document.body.classList.add("hud");
    const un = listen<Toast>("toast", (e) => {
      toast = e.payload;
      clearTimeout(timer);
      // "Encoding…" stays until the result replaces it; recovery waits for a decision.
      if (toast.kind !== "encoding" && toast.kind !== "recovery") timer = setTimeout(hide, AUTO_HIDE_MS);
    });
    // Toasts emitted before this listener existed (e.g. recovery at startup) are re-sent now.
    un.then(() => invoke("toast_ready"));
    return () => un.then((f) => f());
  });

  function reveal() {
    if (toast?.kind === "saved" && toast.path) {
      invoke("reveal", { path: toast.path });
      hide();
    }
  }

  function recover(e: MouseEvent, action: "open" | "save" | "delete") {
    e.stopPropagation();
    invoke("recover", { action });
    hide();
  }

  function undo(e: MouseEvent) {
    e.stopPropagation();
    invoke("undo_cancel");
    hide();
  }
</script>

{#if toast}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="toast {toast.kind}" onclick={reveal}>
    <div class="icon" aria-hidden="true">
      {#if toast.kind === "encoding"}
        <span class="spinner"></span>
      {:else if toast.kind === "saved"}
        <svg viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7" /></svg>
      {:else}
        <svg viewBox="0 0 16 16"><path d="M8 3.5v5.5M8 11.5v.5" /></svg>
      {/if}
    </div>
    <div class="text">
      <div class="title">{toast.title}</div>
      {#if toast.detail}<div class="detail">{toast.detail}</div>{/if}
    </div>
    {#if toast.kind === "saved"}
      <button onclick={(e) => { e.stopPropagation(); reveal(); }}>{t("toast.open_folder")}</button>
    {:else if toast.kind === "cancelled"}
      <button onclick={undo}>{t("toast.undo")}</button>
    {:else if toast.kind === "recovery"}
      <button onclick={(e) => recover(e, "open")}>{t("toast.open_editor")}</button>
      <button onclick={(e) => recover(e, "save")}>{t("toast.save")}</button>
      <button onclick={(e) => recover(e, "delete")}>{t("toast.delete")}</button>
    {/if}
    {#if toast.kind !== "encoding"}
      <button class="close" aria-label="close" onclick={(e) => { e.stopPropagation(); hide(); }}>×</button>
    {/if}
  </div>
{/if}

<style>
  .toast {
    height: 100vh;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 10px 0 16px;
    border-radius: 12px;
    background: var(--hud);
    color: var(--hud-text);
  }
  .saved {
    cursor: pointer;
  }
  .icon {
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgba(255, 255, 255, 0.1);
  }
  .saved .icon {
    background: #2f9e5a;
  }
  .failed .icon {
    background: var(--danger);
  }
  .cancelled .icon,
  .warning .icon,
  .recovery .icon {
    background: var(--warning);
  }
  svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: #fff;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .spinner {
    width: 14px;
    height: 14px;
    border: 2px solid rgba(255, 255, 255, 0.3);
    border-top-color: #fff;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  .text {
    flex: 1;
    min-width: 0;
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail {
    color: #aab2c0;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  button {
    flex: none;
    border: 0;
    border-radius: 6px;
    padding: 6px 10px;
    background: rgba(255, 255, 255, 0.12);
    font-size: 12px;
  }
  button:hover {
    background: rgba(255, 255, 255, 0.2);
  }
  button.close {
    background: transparent;
    font-size: 18px;
    padding: 2px 8px;
    color: #aab2c0;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
