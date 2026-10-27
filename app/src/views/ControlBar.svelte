<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { t } from "../lib/i18n.svelte";
  import { humanSize, type RecStats } from "../lib/types";

  const empty: RecStats = { elapsed_ms: 0, frames: 0, est_bytes: 0, paused: false };
  let stats = $state<RecStats>({ ...empty });

  const elapsed = $derived.by(() => {
    const s = Math.floor(stats.elapsed_ms / 1000);
    return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  });

  onMount(() => {
    document.body.classList.add("hud");
    const un = [
      listen<RecStats>("rec-stats", (e) => (stats = e.payload)),
      listen("rec-start", () => (stats = { ...empty })),
    ];
    return () => un.forEach((p) => p.then((f) => f()));
  });

  const control = (action: string) => invoke("control", { action });
</script>

<div class="bar" class:paused={stats.paused}>
  <span class="dot" aria-hidden="true"></span>
  <span class="time">{elapsed}</span>
  <span class="meta">{t("bar.frames", { n: stats.frames })} · ≈{humanSize(stats.est_bytes)}</span>
  <div class="actions">
    <button title={stats.paused ? t("bar.resume") : t("bar.pause")} onclick={() => control("pause")}>
      {#if stats.paused}
        <svg viewBox="0 0 16 16"><path d="M4 2.5v11l9-5.5z" /></svg>
      {:else}
        <svg viewBox="0 0 16 16"><path d="M4 2.5h3v11H4zM9 2.5h3v11H9z" /></svg>
      {/if}
    </button>
    <button class="stop" title={t("bar.stop")} onclick={() => control("stop")}>
      <svg viewBox="0 0 16 16"><rect x="3" y="3" width="10" height="10" rx="1.5" /></svg>
    </button>
    <button title={t("bar.cancel")} onclick={() => control("cancel")}>
      <svg viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" /></svg>
    </button>
  </div>
</div>

<style>
  .bar {
    height: 100vh;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 6px 0 14px;
    border-radius: 999px;
    background: var(--hud);
    color: var(--hud-text);
    font-variant-numeric: tabular-nums;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--danger);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .paused .dot {
    background: var(--warning);
    animation: none;
  }
  .time {
    font-weight: 600;
  }
  .meta {
    color: #aab2c0;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }
  .actions {
    display: flex;
    gap: 2px;
  }
  button {
    width: 34px;
    height: 34px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 50%;
    background: transparent;
  }
  button:hover {
    background: rgba(255, 255, 255, 0.12);
  }
  button.stop {
    color: var(--danger);
  }
  svg {
    width: 16px;
    height: 16px;
    fill: currentColor;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
