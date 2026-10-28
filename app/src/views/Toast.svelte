<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { Check, Download, FolderOpen, TriangleAlert, Undo2, X } from "@lucide/svelte";
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
      // Prompts that need a decision stay until answered.
      if (!["encoding", "recovery", "update"].includes(toast.kind)) timer = setTimeout(hide, AUTO_HIDE_MS);
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
  {@const actions = ["saved", "cancelled", "update", "recovery"].includes(toast.kind)}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="toast {toast.kind}" onclick={reveal}>
    <div class="head">
      <div class="icon" aria-hidden="true">
        {#if toast.kind === "encoding"}
          <span class="spinner"></span>
        {:else if toast.kind === "saved"}
          <Check size={16} strokeWidth={2.5} />
        {:else if toast.kind === "failed"}
          <X size={16} strokeWidth={2.5} />
        {:else if toast.kind === "update"}
          <Download size={15} strokeWidth={2.25} />
        {:else}
          <TriangleAlert size={15} strokeWidth={2.25} />
        {/if}
      </div>
      <div class="text">
        <div class="title" title={toast.title}>{toast.title}</div>
        {#if toast.detail}<div class="detail" title={toast.detail}>{toast.detail}</div>{/if}
      </div>
      {#if toast.kind !== "encoding"}
        <button class="close" aria-label="close" onclick={(e) => { e.stopPropagation(); hide(); }}><X size={16} /></button>
      {/if}
    </div>
    {#if actions}
      <div class="actions">
        {#if toast.kind === "saved"}
          <button onclick={(e) => { e.stopPropagation(); reveal(); }}><FolderOpen size={14} />{t("toast.open_folder")}</button>
        {:else if toast.kind === "cancelled"}
          <button onclick={undo}><Undo2 size={14} />{t("toast.undo")}</button>
        {:else if toast.kind === "update"}
          <button class="accent" onclick={(e) => { e.stopPropagation(); invoke("install_update"); }}>{t("toast.update_now")}</button>
        {:else if toast.kind === "recovery"}
          <button class="accent" onclick={(e) => recover(e, "open")}>{t("toast.open_editor")}</button>
          <button onclick={(e) => recover(e, "save")}>{t("toast.save")}</button>
          <button onclick={(e) => recover(e, "delete")}>{t("toast.delete")}</button>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .toast {
    height: 100vh;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 10px;
    padding: 0 8px 0 14px;
    border-radius: 12px;
    background: var(--hud);
    color: var(--hud-text);
    box-shadow: inset 0 0 0 1px var(--hud-border);
  }
  .saved {
    cursor: pointer;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .icon {
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    background: rgba(255, 255, 255, 0.1);
  }
  .saved .icon {
    background: #2f9e5a;
  }
  .failed .icon {
    background: #c42b1c;
  }
  .cancelled .icon,
  .warning .icon,
  .recovery .icon,
  .update .icon {
    background: var(--warning);
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
    color: var(--hud-muted);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Buttons line up under the text, past the icon. */
  .actions {
    display: flex;
    gap: 6px;
    padding-left: 40px;
  }
  button {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    border: 0;
    border-radius: 6px;
    padding: 0 10px;
    background: rgba(255, 255, 255, 0.1);
    font-size: 12px;
  }
  button:hover {
    background: rgba(255, 255, 255, 0.18);
  }
  button.accent {
    background: #4cc2ff;
    color: #000;
    font-weight: 600;
  }
  button.accent:hover {
    background: #47b1e8;
  }
  button.close {
    width: 28px;
    padding: 0;
    justify-content: center;
    background: transparent;
    color: var(--hud-muted);
  }
  button.close:hover {
    background: rgba(255, 255, 255, 0.1);
    color: var(--hud-text);
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
