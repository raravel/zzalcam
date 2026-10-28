<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { ask, save } from "@tauri-apps/plugin-dialog";
  import { t } from "../lib/i18n.svelte";
  import { humanSize } from "../lib/types";

  interface ExportSettings {
    format: "gif" | "webp";
    preset: "best" | "balanced" | "smallest";
    scale: number;
    max_mb: number | null;
  }
  interface Info {
    width: number;
    height: number;
    timings: [number, number][];
    defaults: ExportSettings;
  }
  interface Crop {
    x: number;
    y: number;
    w: number;
    h: number;
  }
  /** Everything undo/redo restores. */
  interface Edit {
    trim: [number, number];
    deleted: number[];
    crop: Crop | null;
  }

  const id = new URLSearchParams(location.search).get("editor") ?? "";
  const THUMB_WIDTH = 120;
  const SCALES = [1, 0.75, 0.5, 0.33];
  const ESTIMATE_DELAY_MS = 500;

  // Tauri serves custom protocols as http://<scheme>.localhost on Windows.
  const frameUrl = (i: number, width?: number) =>
    `http://zzalcam.localhost/${id}/frame/${i}${width ? `?w=${width}` : ""}`;
  const thumbUrl = (i: number) => `http://zzalcam.localhost/${id}/thumb/${i}?w=${THUMB_WIDTH}`;

  let info = $state<Info | null>(null);
  // Preview no larger than it is shown (in device pixels), rounded so the URL stays cacheable.
  let stageWidth = $state(0);
  const previewWidth = $derived(
    Math.min(info?.width ?? 0, Math.ceil((stageWidth * devicePixelRatio) / 256) * 256) || undefined,
  );
  let edit = $state<Edit>({ trim: [0, 0], deleted: [], crop: null });
  let out = $state<ExportSettings>({ format: "gif", preset: "balanced", scale: 1, max_mb: null });
  let current = $state(0);
  let selection = $state<Set<number>>(new Set());
  let anchor = 0;
  let playing = $state(false);
  let cropping = $state(false);
  let estimate = $state<number | null>(null);
  let estimating = $state(false);
  let saving = $state(false);
  let saved = $state(false);
  let error = $state("");
  let undoStack: Edit[] = [];
  let redoStack: Edit[] = [];

  const total = $derived(info?.timings.length ?? 0);
  const deletedSet = $derived(new Set(edit.deleted));
  const kept = $derived(
    Array.from({ length: total }, (_, i) => i).filter(
      (i) => i >= edit.trim[0] && i < edit.trim[1] && !deletedSet.has(i),
    ),
  );
  const keptDurationMs = $derived(kept.reduce((sum, i) => sum + (info?.timings[i][1] ?? 0), 0));

  onMount(() => {
    (async () => {
      info = await invoke<Info>("editor_info", { id });
      edit.trim = [0, info.timings.length];
      out = { ...info.defaults };
    })().catch((e) => (error = String(e)));

    const unlisten = getCurrentWindow().onCloseRequested(async (e) => {
      // Closing mid-save would retire the session under the running export.
      if (saving) {
        e.preventDefault();
        return;
      }
      if (!saved && !(await ask(t("editor.close_confirm"), { kind: "warning" }))) {
        e.preventDefault();
        return;
      }
      await invoke("editor_close", { id });
    });
    return () => unlisten.then((f) => f());
  });

  // --- edits with history
  function snapshot(): Edit {
    return { trim: [...edit.trim] as [number, number], deleted: [...edit.deleted], crop: edit.crop && { ...edit.crop } };
  }
  function change(apply: () => void) {
    undoStack.push(snapshot());
    redoStack = [];
    apply();
    saved = false;
  }
  function undo() {
    const prev = undoStack.pop();
    if (!prev) return;
    redoStack.push(snapshot());
    edit = prev;
    saved = false;
  }
  function redo() {
    const next = redoStack.pop();
    if (!next) return;
    undoStack.push(snapshot());
    edit = next;
    saved = false;
  }

  const trimStart = () => change(() => (edit.trim = [current, Math.max(current + 1, edit.trim[1])]));
  const trimEnd = () => change(() => (edit.trim = [Math.min(edit.trim[0], current), current + 1]));
  function deleteSelected() {
    stop();
    const targets = selection.size ? [...selection] : [current];
    change(() => (edit.deleted = [...new Set([...edit.deleted, ...targets])].sort((a, b) => a - b)));
    selection = new Set();
  }
  function restoreSelected() {
    const targets = new Set(selection.size ? [...selection] : [current]);
    change(() => (edit.deleted = edit.deleted.filter((i) => !targets.has(i))));
  }

  // --- selection & navigation
  function pick(i: number, e: MouseEvent) {
    current = i;
    if (e.shiftKey) {
      const [a, b] = [Math.min(anchor, i), Math.max(anchor, i)];
      selection = new Set(Array.from({ length: b - a + 1 }, (_, k) => a + k));
    } else if (e.ctrlKey) {
      const s = new Set(selection);
      s.has(i) ? s.delete(i) : s.add(i);
      selection = s;
      anchor = i;
    } else {
      selection = new Set([i]);
      anchor = i;
    }
  }
  function step(dir: 1 | -1) {
    const pos = kept.indexOf(current);
    const next = pos === -1 ? kept.find((i) => (dir > 0 ? i > current : i < current)) : kept[pos + dir];
    if (next !== undefined) current = next;
    document.getElementById(`f${current}`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  // --- playback at recorded timing, over kept frames only
  let timer: ReturnType<typeof setTimeout> | undefined;
  function play() {
    if (!kept.length) return;
    playing = true;
    if (!kept.includes(current)) current = kept[0];
    const tick = () => {
      if (!playing || !info) return;
      if (!kept.length) return stop();
      const pos = kept.indexOf(current);
      new Image().src = frameUrl(kept[(pos + 1) % kept.length], previewWidth); // preload
      timer = setTimeout(() => {
        current = kept[(kept.indexOf(current) + 1) % kept.length];
        tick();
      }, info.timings[current][1]);
    };
    tick();
  }
  function stop() {
    playing = false;
    clearTimeout(timer);
  }

  // --- crop by dragging on the preview
  let previewEl = $state<HTMLImageElement | null>(null);
  let dragFrom: { x: number; y: number } | null = null;
  function toImage(e: PointerEvent) {
    const r = previewEl!.getBoundingClientRect();
    const sx = info!.width / r.width;
    const sy = info!.height / r.height;
    return {
      x: Math.round(Math.min(Math.max(e.clientX - r.left, 0), r.width) * sx),
      y: Math.round(Math.min(Math.max(e.clientY - r.top, 0), r.height) * sy),
    };
  }
  let cropBefore: Edit | null = null;
  function cropDown(e: PointerEvent) {
    if (!cropping || !info) return;
    (e.target as Element).setPointerCapture(e.pointerId);
    cropBefore = snapshot();
    dragFrom = toImage(e);
  }
  function cropMove(e: PointerEvent) {
    if (!dragFrom) return;
    const p = toImage(e);
    const x = Math.min(p.x, dragFrom.x);
    const y = Math.min(p.y, dragFrom.y);
    edit.crop = { x, y, w: Math.max(1, Math.abs(p.x - dragFrom.x)), h: Math.max(1, Math.abs(p.y - dragFrom.y)) };
    saved = false;
  }
  function cropUp() {
    if (!dragFrom) return;
    dragFrom = null;
    if (edit.crop && (edit.crop.w < 4 || edit.crop.h < 4)) edit.crop = cropBefore?.crop ?? null;
    // Only a drag that actually changed the crop is an undo step.
    if (cropBefore && JSON.stringify(cropBefore.crop) !== JSON.stringify(edit.crop)) {
      undoStack.push(cropBefore);
      redoStack = [];
    }
    cropBefore = null;
  }
  function clampCrop() {
    if (!edit.crop || !info) return;
    undoStack.push({ ...snapshot(), crop: lastCrop });
    redoStack = [];
    const c = edit.crop;
    c.x = Math.min(Math.max(0, Math.round(c.x)), info.width - 1);
    c.y = Math.min(Math.max(0, Math.round(c.y)), info.height - 1);
    c.w = Math.min(Math.max(1, Math.round(c.w)), info.width - c.x);
    c.h = Math.min(Math.max(1, Math.round(c.h)), info.height - c.y);
    saved = false;
  }

  // Crop as it was when a numeric field gained focus, for its undo step.
  let lastCrop: Crop | null = null;

  // --- export
  function editPayload() {
    const c = edit.crop;
    return {
      trim: edit.trim[0] === 0 && edit.trim[1] === total ? null : edit.trim,
      deleted: edit.deleted,
      crop: c ? [c.x, c.y, c.w, c.h] : null,
    };
  }

  let estimateTimer: ReturnType<typeof setTimeout> | undefined;
  let estimateSeq = 0;
  $effect(() => {
    if (!info) return;
    const payload = { id, edit: editPayload(), export: { ...out } };
    clearTimeout(estimateTimer);
    estimating = true;
    const seq = ++estimateSeq;
    estimateTimer = setTimeout(async () => {
      try {
        const bytes = await invoke<number>("editor_estimate", payload);
        if (seq === estimateSeq) estimate = bytes;
      } catch (e) {
        if (seq === estimateSeq) estimate = null;
      } finally {
        if (seq === estimateSeq) estimating = false;
      }
    }, ESTIMATE_DELAY_MS);
  });

  async function doSave(saveAs: boolean) {
    if (saving) return;
    let path: string | null = null;
    if (saveAs) {
      path = await save({ filters: [{ name: out.format.toUpperCase(), extensions: [out.format] }] });
      if (!path) return;
    }
    saving = true;
    error = "";
    try {
      await invoke("editor_save", { id, edit: editPayload(), export: { ...out }, path });
      saved = true;
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if ((e.target as HTMLElement).tagName === "INPUT" || (e.target as HTMLElement).tagName === "SELECT") return;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.key.toLowerCase() === "z") (e.preventDefault(), undo());
    else if (ctrl && e.key.toLowerCase() === "y") (e.preventDefault(), redo());
    else if (ctrl && e.key.toLowerCase() === "s") (e.preventDefault(), doSave(false));
    else if (e.key === " ") (e.preventDefault(), playing ? stop() : play());
    else if (e.key === "ArrowRight") (e.preventDefault(), stop(), step(1));
    else if (e.key === "ArrowLeft") (e.preventDefault(), stop(), step(-1));
    else if (e.key === "Delete") deleteSelected();
    else if (e.key === "[") trimStart();
    else if (e.key === "]") trimEnd();
  }
</script>

<svelte:window {onkeydown} />

{#if info}
  <div class="editor">
    <section class="stage">
      <div class="preview-wrap" bind:clientWidth={stageWidth}>
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div class="preview" class:cropping onpointerdown={cropDown} onpointermove={cropMove} onpointerup={cropUp} role="img">
          <img bind:this={previewEl} src={frameUrl(current, previewWidth)} alt="" draggable="false" style:aspect-ratio="{info.width} / {info.height}" />
          {#if edit.crop}
            {@const c = edit.crop}
            <div
              class="crop"
              style:left="{(c.x / info.width) * 100}%"
              style:top="{(c.y / info.height) * 100}%"
              style:width="{(c.w / info.width) * 100}%"
              style:height="{(c.h / info.height) * 100}%"
            ></div>
          {/if}
          {#if deletedSet.has(current) || current < edit.trim[0] || current >= edit.trim[1]}
            <div class="excluded"></div>
          {/if}
        </div>
      </div>

      <div class="transport">
        <button title={t("editor.prev")} onclick={() => (stop(), step(-1))}>◀</button>
        <button class="play" title={playing ? t("editor.pause") : t("editor.play")} onclick={() => (playing ? stop() : play())}>
          {playing ? "❚❚" : "▶"}
        </button>
        <button title={t("editor.next")} onclick={() => (stop(), step(1))}>▶</button>
        <span class="pos">#{current + 1} · {info.timings[current][1]}ms</span>
        <span class="sep"></span>
        <button onclick={trimStart}>⇤ {t("editor.trim_start")}</button>
        <button onclick={trimEnd}>{t("editor.trim_end")} ⇥</button>
        <button onclick={deleteSelected}>🗑 {t("editor.delete")}</button>
        {#if [...(selection.size ? selection : [current])].some((i) => deletedSet.has(i))}
          <button onclick={restoreSelected}>{t("editor.restore")}</button>
        {/if}
        <span class="sep"></span>
        <button title={t("editor.undo")} onclick={undo}>↶</button>
        <button title={t("editor.redo")} onclick={redo}>↷</button>
      </div>

      <div class="strip">
        {#each info.timings as [, dur], i}
          <button
            id="f{i}"
            class="thumb"
            class:current={i === current}
            class:selected={selection.has(i)}
            class:deleted={deletedSet.has(i)}
            class:trimmed={i < edit.trim[0] || i >= edit.trim[1]}
            onclick={(e) => (stop(), pick(i, e))}
          >
            <img loading="lazy" src={thumbUrl(i)} alt="" draggable="false" />
            <span>{i + 1} · {dur}ms</span>
          </button>
        {/each}
      </div>
      <p class="hint">{t("editor.hint")}</p>
    </section>

    <aside>
      <h2>{t("editor.output")}</h2>
      <div class="segmented">
        {#each ["gif", "webp"] as const as f}
          <button class:active={out.format === f} onclick={() => (out.format = f)}>{f.toUpperCase()}</button>
        {/each}
      </div>
      <label>
        {t("settings.preset")}
        <select bind:value={out.preset}>
          {#each ["best", "balanced", "smallest"] as p}<option value={p}>{t(`settings.preset.${p}`)}</option>{/each}
        </select>
      </label>
      <label>
        {t("editor.scale")}
        <select bind:value={out.scale}>
          {#each SCALES as s}
            <option value={s}>{Math.round(s * 100)}% · {Math.round((edit.crop?.w ?? info.width) * s)}×{Math.round((edit.crop?.h ?? info.height) * s)}</option>
          {/each}
        </select>
      </label>
      <label>
        {t("editor.max_size")}
        <input type="number" min="0" step="0.5" placeholder={t("editor.no_limit")} bind:value={out.max_mb} />
      </label>

      <h2>{t("editor.crop")}</h2>
      <button class="wide" class:active={cropping} onclick={() => (cropping = !cropping)}>✂ {t("editor.crop")}</button>
      {#if edit.crop}
        <div class="crop-fields">
          {#each ["x", "y", "w", "h"] as const as k}
            <label>{k}<input type="number" bind:value={edit.crop[k]} onfocus={() => (lastCrop = edit.crop && { ...edit.crop })} onchange={clampCrop} /></label>
          {/each}
        </div>
        <button class="wide" onclick={() => change(() => (edit.crop = null))}>{t("editor.crop_reset")}</button>
      {/if}

      <div class="stats">
        <div>{t("editor.frames", { kept: kept.length, total })}</div>
        <div>{t("editor.duration", { s: (keptDurationMs / 1000).toFixed(1) })}</div>
        {#if selection.size > 1}<div>{t("editor.selection", { n: selection.size })}</div>{/if}
        <div class="estimate">
          {t("editor.estimate")}:
          <strong>{estimating ? t("editor.estimating") : estimate != null ? `≈${humanSize(estimate)}` : "—"}</strong>
        </div>
      </div>

      {#if error}<p class="error">{error}</p>{/if}
      <div class="actions">
        <button class="primary" disabled={saving || !kept.length} onclick={() => doSave(false)}>
          {saving ? t("editor.saving") : t("editor.save")}
        </button>
        <button disabled={saving || !kept.length} onclick={() => doSave(true)}>{t("editor.save_as")}</button>
      </div>
    </aside>
  </div>
{:else if error}
  <p class="error">{error}</p>
{/if}

<style>
  .editor {
    display: grid;
    grid-template-columns: 1fr 260px;
    height: 100vh;
  }
  .stage {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .preview-wrap {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 16px;
    background: repeating-conic-gradient(#0000 0 25%, #8881 0 50%) 0 0 / 16px 16px;
  }
  .preview {
    position: relative;
    max-width: 100%;
    max-height: 100%;
    display: flex;
  }
  .preview img {
    max-width: 100%;
    max-height: calc(100vh - 260px);
    object-fit: contain;
    box-shadow: 0 2px 16px #0003;
  }
  .preview.cropping {
    cursor: crosshair;
  }
  .crop {
    position: absolute;
    outline: 2px solid var(--accent);
    box-shadow: 0 0 0 9999px #0008;
    pointer-events: none;
  }
  .excluded {
    position: absolute;
    inset: 0;
    background: repeating-linear-gradient(45deg, #0006 0 10px, #0003 10px 20px);
    pointer-events: none;
  }
  .transport {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface);
    flex-wrap: wrap;
  }
  .transport button,
  aside button,
  select,
  input {
    height: 30px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: var(--bg);
    font: inherit;
    font-size: 13px;
    color: inherit;
  }
  .transport .play {
    min-width: 44px;
  }
  .pos {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    margin: 0 6px;
    font-size: 12px;
  }
  .sep {
    width: 1px;
    height: 20px;
    background: var(--border);
    margin: 0 6px;
  }
  .strip {
    display: flex;
    gap: 4px;
    overflow-x: auto;
    padding: 8px 12px;
    background: var(--surface);
    border-top: 1px solid var(--border);
    min-height: 112px;
  }
  .thumb {
    flex: none;
    width: 124px;
    padding: 2px;
    border: 2px solid transparent;
    border-radius: 6px;
    background: var(--bg);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .thumb img {
    width: 100%;
    height: 72px;
    object-fit: contain;
    background: #0002;
    border-radius: 3px;
  }
  .thumb span {
    font-size: 11px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .thumb.selected {
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .thumb.current {
    border-color: var(--accent);
  }
  .thumb.deleted img,
  .thumb.trimmed img {
    opacity: 0.25;
  }
  .thumb.deleted span {
    text-decoration: line-through;
  }
  .hint {
    margin: 0;
    padding: 4px 12px 8px;
    font-size: 11px;
    color: var(--muted);
    background: var(--surface);
  }
  aside {
    border-left: 1px solid var(--border);
    background: var(--surface);
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow-y: auto;
  }
  h2 {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin: 6px 0 0;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--muted);
  }
  .segmented {
    display: flex;
  }
  .segmented button {
    flex: 1;
    border-radius: 0;
  }
  .segmented button:first-child {
    border-radius: 7px 0 0 7px;
  }
  .segmented button:last-child {
    border-radius: 0 7px 7px 0;
    border-left: 0;
  }
  .active {
    background: var(--accent) !important;
    border-color: var(--accent) !important;
    color: var(--accent-text) !important;
  }
  .wide {
    width: 100%;
  }
  .crop-fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .crop-fields input {
    width: 100%;
  }
  .stats {
    margin-top: auto;
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .estimate strong {
    color: var(--text);
  }
  .actions {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .primary {
    background: var(--accent) !important;
    border-color: var(--accent) !important;
    color: var(--accent-text) !important;
    font-weight: 600;
    height: 36px !important;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .error {
    color: var(--danger);
    font-size: 12px;
    margin: 0;
    word-break: break-word;
  }
</style>
