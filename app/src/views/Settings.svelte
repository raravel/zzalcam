<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open } from "@tauri-apps/plugin-dialog";
  import { FolderOpen } from "@lucide/svelte";
  import { t } from "../lib/i18n.svelte";
  import type { Hotkeys, Settings } from "../lib/types";
  import HotkeyInput from "./HotkeyInput.svelte";

  const FPS = [5, 10, 15, 20, 24, 30, 50, 60];
  const HOTKEYS: (keyof Hotkeys)[] = ["record", "pause", "repeat", "cancel"];
  const SAVE_DELAY_MS = 300;

  let settings = $state<Settings | null>(null);
  let conflicts = $state<string[]>([]);
  let savedFlash = $state(false);
  let portable = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastSaved = "";

  onMount(async () => {
    settings = await invoke<Settings>("get_settings");
    lastSaved = JSON.stringify(settings);
    conflicts = await invoke<string[]>("get_conflicts");
    portable = await invoke<boolean>("is_portable");
  });

  // Save shortly after any change.
  $effect(() => {
    if (!settings) return;
    const snapshot = JSON.stringify(settings);
    if (snapshot === lastSaved) return;
    clearTimeout(timer);
    timer = setTimeout(async () => {
      const [saved, c] = await invoke<[Settings, string[]]>("save_settings", { settings: JSON.parse(snapshot) });
      lastSaved = JSON.stringify(saved);
      conflicts = c;
      savedFlash = true;
      setTimeout(() => (savedFlash = false), 1200);
    }, SAVE_DELAY_MS);
  });

  async function chooseFolder() {
    if (!settings) return;
    const dir = await open({ directory: true, defaultPath: settings.output_dir });
    if (typeof dir === "string") settings.output_dir = dir;
  }

  async function record() {
    await getCurrentWindow().hide();
    invoke("start", { mode: "smart" });
  }
</script>

{#if settings}
  <main>
    <header>
      <h1>{t("settings.title")}</h1>
      <span class="saved" class:show={savedFlash}>{t("settings.saved")}</span>
      <button class="btn primary" onclick={record}>{t("tray.record")}</button>
    </header>

    <section>
      <h2>{t("settings.capture")}</h2>
      <div class="card">
        <label class="row">
          <span>{t("settings.fps")}</span>
          <select bind:value={settings.fps}>
            {#each FPS as f}<option value={f}>{f}</option>{/each}
          </select>
        </label>
        <label class="row">
          <span>{t("settings.after_record")}</span>
          <select bind:value={settings.after_record}>
            <option value="editor">{t("settings.after_record.editor")}</option>
            <option value="save">{t("settings.after_record.save")}</option>
          </select>
        </label>
        <label class="row">
          <span>{t("settings.cursor")}</span>
          <input type="checkbox" bind:checked={settings.cursor} />
        </label>
        <label class="row">
          <span>{t("settings.countdown")}</span>
          <input type="checkbox" bind:checked={settings.countdown} />
        </label>
      </div>
    </section>

    <section>
      <h2>{t("settings.output")}</h2>
      <div class="card">
        <div class="row">
          <span>{t("settings.format")}</span>
          <div class="segmented">
            {#each ["gif", "webp"] as const as f}
              <button class:active={settings.format === f} onclick={() => (settings!.format = f)}>{f.toUpperCase()}</button>
            {/each}
          </div>
        </div>
        <label class="row">
          <span>{t("settings.preset")}</span>
          <select bind:value={settings.preset}>
            {#each ["best", "balanced", "smallest"] as p}
              <option value={p}>{t(`settings.preset.${p}`)}</option>
            {/each}
          </select>
        </label>
        <label class="row">
          <span>{t("settings.scale")}</span>
          <select bind:value={settings.export_scale}>
            {#each [1, 0.75, 0.5, 0.33] as s}<option value={s}>{Math.round(s * 100)}%</option>{/each}
          </select>
        </label>
        <label class="row">
          <span>{t("settings.max_size")}</span>
          <span class="unit-input">
            <input class="num" type="number" min="0" step="0.5" placeholder={t("editor.no_limit")} value={settings.max_size_mb ?? ""}
              onchange={(e) => {
              const input = e.target as HTMLInputElement;
              const v = Number(input.value);
              settings!.max_size_mb = v > 0 ? v : null;
              if (!(v > 0)) input.value = "";
            }} />
            <span class="unit">MB</span>
          </span>
        </label>
        <label class="row tall">
          <span>{t("settings.filename")}</span>
          <span class="stack">
            <input class="text" bind:value={settings.filename_pattern} />
            <span class="hint">{t("settings.filename_hint")}</span>
          </span>
        </label>
        <label class="row">
          <span>{t("settings.keep_sessions")}</span>
          <input type="checkbox" bind:checked={settings.keep_sessions} />
        </label>
        <div class="row tall">
          <span class="stack folder">
            <span>{t("settings.folder")}</span>
            <span class="path" title={settings.output_dir}>{settings.output_dir}</span>
          </span>
          <button class="btn" onclick={chooseFolder}>
            <FolderOpen size={16} />{t("settings.choose")}
          </button>
        </div>
      </div>
    </section>

    <section>
      <h2>{t("settings.hotkeys")}</h2>
      <div class="card">
        {#each HOTKEYS as action}
          <div class="row">
            <span>
              {t(`settings.hotkey.${action}`)}
              {#if conflicts.includes(action)}<em class="conflict">{t("settings.hotkey.conflict")}</em>{/if}
            </span>
            <HotkeyInput bind:value={settings.hotkeys[action]} conflict={conflicts.includes(action)} />
          </div>
        {/each}
      </div>
    </section>

    <section>
      <h2>{t("settings.general")}</h2>
      <div class="card">
        <label class="row">
          <span>{t("settings.language")}</span>
          <select bind:value={settings.language}>
            <option value="auto">{t("settings.language.auto")}</option>
            <option value="ko">한국어</option>
            <option value="en">English</option>
          </select>
        </label>
        <label class="row">
          <span>{t("settings.autostart")}</span>
          <input type="checkbox" bind:checked={settings.autostart} />
        </label>
        <label class="row">
          <span>
            {t("settings.auto_update")}
            {#if portable}<em class="hint">{t("settings.portable")}</em>{/if}
          </span>
          <input type="checkbox" bind:checked={settings.auto_update} disabled={portable} />
        </label>
      </div>
    </section>
  </main>
{/if}

<style>
  main {
    height: 100vh;
    overflow-y: auto;
    padding: 20px 20px 28px 24px;
  }
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 18px;
  }
  h1 {
    font-size: 24px;
    font-weight: 600;
    margin: 0;
    flex: 1;
  }
  h2 {
    font-size: 14px;
    font-weight: 600;
    margin: 0 0 6px 2px;
  }
  section {
    margin-bottom: 20px;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0 16px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 48px;
    border-top: 1px solid var(--border);
  }
  .row:first-child {
    border-top: 0;
  }
  .row.tall {
    padding: 10px 0;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .folder {
    flex: 1;
  }
  .path {
    color: var(--muted);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .saved {
    color: var(--muted);
    font-size: 12px;
    opacity: 0;
    transition: opacity 0.2s;
  }
  .saved.show {
    opacity: 1;
  }
  .unit-input {
    position: relative;
    display: inline-flex;
  }
  input.num {
    width: 130px;
    padding-right: 36px;
  }
  .unit-input input::-webkit-inner-spin-button,
  .unit-input input::-webkit-outer-spin-button {
    appearance: none;
    margin: 0;
  }
  .unit {
    position: absolute;
    right: 10px;
    top: 50%;
    transform: translateY(-50%);
    font-size: 12px;
    color: var(--muted);
    pointer-events: none;
  }
  input.text {
    width: 240px;
  }
  .hint {
    display: block;
    font-style: normal;
    font-size: 11px;
    color: var(--muted);
    max-width: 240px;
  }
  .conflict {
    display: block;
    font-style: normal;
    font-size: 12px;
    color: var(--danger);
  }
</style>
