<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { ask, save } from "@tauri-apps/plugin-dialog";
  import {
    ArrowLeftToLine,
    ArrowRightToLine,
    Pause,
    Play,
    Redo2,
    RotateCcw,
    Scissors,
    StepBack,
    StepForward,
    Trash2,
    Undo2,
  } from "@lucide/svelte";
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

  // --- frame strip: the wheel scrolls it sideways; edges fade where more frames are hidden
  let strip = $state<HTMLElement | null>(null);
  let fadeStart = $state(false);
  let fadeEnd = $state(false);
  function updateFades() {
    if (!strip) return;
    fadeStart = strip.scrollLeft > 1;
    fadeEnd = strip.scrollLeft + strip.clientWidth < strip.scrollWidth - 1;
  }
  $effect(() => {
    if (!strip) return;
    const el = strip;
    const onWheel = (e: WheelEvent) => {
      if (e.deltaX !== 0 || e.shiftKey || e.ctrlKey) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    const ro = new ResizeObserver(updateFades);
    ro.observe(el);
    updateFades();
    return () => {
      el.removeEventListener("wheel", onWheel);
      ro.disconnect();
    };
  });

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
        <button class="btn icon" title={t("editor.prev")} onclick={() => (stop(), step(-1))}><StepBack size={16} /></button>
        <button class="btn icon play" title={playing ? t("editor.pause") : t("editor.play")} onclick={() => (playing ? stop() : play())}>
          {#if playing}<Pause size={16} fill="currentColor" />{:else}<Play size={16} fill="currentColor" />{/if}
        </button>
        <button class="btn icon" title={t("editor.next")} onclick={() => (stop(), step(1))}><StepForward size={16} /></button>
        <span class="pos">#{current + 1} · {info.timings[current][1]}ms</span>
        <span class="sep"></span>
        <button class="btn" onclick={trimStart}><ArrowLeftToLine size={16} />{t("editor.trim_start")}</button>
        <button class="btn" onclick={trimEnd}>{t("editor.trim_end")}<ArrowRightToLine size={16} /></button>
        <button class="btn" onclick={deleteSelected}><Trash2 size={16} />{t("editor.delete")}</button>
        {#if [...(selection.size ? selection : [current])].some((i) => deletedSet.has(i))}
          <button class="btn" onclick={restoreSelected}><RotateCcw size={16} />{t("editor.restore")}</button>
        {/if}
        <span class="sep"></span>
        <button class="btn icon" title={t("editor.undo")} onclick={undo}><Undo2 size={16} /></button>
        <button class="btn icon" title={t("editor.redo")} onclick={redo}><Redo2 size={16} /></button>
      </div>

      <div class="strip-wrap">
        <div class="strip" class:fade-start={fadeStart} class:fade-end={fadeEnd} bind:this={strip} onscroll={updateFades}>
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
        <span class="unit-input">
          <input type="number" min="0" step="0.5" placeholder={t("editor.no_limit")} bind:value={out.max_mb} />
          <span class="unit">MB</span>
        </span>
      </label>

      <h2>{t("editor.crop")}</h2>
      <button class="btn wide" class:on={cropping} onclick={() => (cropping = !cropping)}><Scissors size={16} />{t("editor.crop")}</button>
      {#if edit.crop}
        <div class="crop-fields">
          {#each ["x", "y", "w", "h"] as const as k}
            <label>{k}<input type="number" bind:value={edit.crop[k]} onfocus={() => (lastCrop = edit.crop && { ...edit.crop })} onchange={clampCrop} /></label>
          {/each}
        </div>
        <button class="btn wide" onclick={() => change(() => (edit.crop = null))}><RotateCcw size={16} />{t("editor.crop_reset")}</button>
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
        <button class="btn primary" disabled={saving || !kept.length} onclick={() => doSave(false)}>
          {saving ? t("editor.saving") : t("editor.save")}
        </button>
        <button class="btn" disabled={saving || !kept.length} onclick={() => doSave(true)}>{t("editor.save_as")}</button>
      </div>
    </aside>
  </div>
{:else if error}
  <p class="error">{error}</p>
{/if}

<style>
  .editor {
    display: grid;
    grid-template-columns: 1fr 272px;
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
    border-radius: 4px;
    box-shadow: 0 4px 24px #0004;
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
  .transport .btn {
    height: 30px;
    font-size: 13px;
  }
  .transport .btn.icon {
    width: 30px;
  }
  .transport .btn.play {
    width: 40px;
  }
  .pos {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    margin: 0 6px;
    font-size: 12px;
    min-width: 72px;
  }
  .sep {
    width: 1px;
    height: 20px;
    background: var(--control-border);
    margin: 0 6px;
  }
  .strip-wrap {
    background: var(--surface);
  }
  .strip {
    display: flex;
    gap: 6px;
    overflow-x: auto;
    padding: 8px 12px 4px;
    min-height: 112px;
    --fade: 40px;
  }
  .strip.fade-end {
    mask-image: linear-gradient(to right, #000 calc(100% - var(--fade)), transparent);
  }
  .strip.fade-start {
    mask-image: linear-gradient(to right, transparent, #000 var(--fade));
  }
  .strip.fade-start.fade-end {
    mask-image: linear-gradient(to right, transparent, #000 var(--fade), #000 calc(100% - var(--fade)), transparent);
  }
  .thumb {
    flex: none;
    width: 124px;
    padding: 3px 3px 2px;
    border: 2px solid transparent;
    border-radius: 8px;
    background: var(--control);
    display: flex;
    flex-direction: column;
    gap: 3px;
    transition: background 0.1s;
  }
  .thumb:hover {
    background: var(--control-hover);
  }
  .thumb img {
    width: 100%;
    height: 70px;
    object-fit: contain;
    background: #0002;
    border-radius: 4px;
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
    font-size: 13px;
    font-weight: 600;
    margin: 6px 0 0;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--muted);
  }
  label select,
  label input {
    color: var(--text);
    font-size: 13px;
  }
  .segmented {
    display: flex;
  }
  .segmented button {
    flex: 1;
  }
  .unit-input {
    position: relative;
    display: flex;
  }
  .unit-input input {
    flex: 1;
    min-width: 0;
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
    color: var(--muted);
    pointer-events: none;
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
    padding: 10px 12px;
    border-radius: var(--radius);
    background: var(--subtle);
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
  .actions .primary {
    height: 36px;
  }
  .error {
    color: var(--danger);
    font-size: 12px;
    margin: 0;
    word-break: break-word;
  }
</style>
