# zzalcam Design

Fast, highly-optimized GIF / animated WebP screen recorder for Windows.
License: GPL-3.0.

## 1. Product shape

- Rust core library shared by a **GUI** (Tauri 2 + Svelte 5 + TypeScript, tray-resident) and a **CLI**.
- Target: Windows 10 2004+ (x64), Per-Monitor DPI Aware v2. 2004 is the first build where `WDA_EXCLUDEFROMCAPTURE` keeps our overlays out of recordings.
- Outputs: GIF and animated WebP only. Encoders sit behind a trait so APNG/MP4 can be added later. No audio.
- UI languages: Korean, English (i18n from day one).

## 2. Capture

| Target | Backend | Notes |
|---|---|---|
| Monitor (single) | DXGI Desktop Duplication | OS-provided dirty/move rects |
| All monitors (virtual desktop) | DXGI per output, composited | Physical-pixel composition across mixed DPI |
| Free region (may span monitors) | DXGI per intersecting output, composited | |
| Window | Windows.Graphics.Capture | Follows the window; captures occluded windows |
| Window control (UI element) | DXGI, rect fixed at record start | Element found via UI Automation `ElementFromPoint`, HWND hit-test fallback; wheel/hotkey walks parent/child |

- All backends implement one `CaptureSource` trait producing frames + dirty rects + timestamps.
- HDR (FP16/scRGB) outputs are tone-mapped to sRGB 8-bit on the GPU right after capture; scaling and cursor compositing happen in the same pass to minimise readback.
- **Variable frame rate**: frames are stored only when content changes; idle time accumulates into the previous frame's delay. GIF output caps at 50 fps (20 ms); 10 ms rounding error is carried forward so total duration matches wall time.
- Window resized during window capture: output size stays at the start size; overflow is cropped, empty area filled with last content or a solid color (option).

## 3. Recording UX

- Global hotkeys: start / stop / pause. 3 s countdown (optional).
- Recording border and control bar are visible but excluded from capture (`WDA_EXCLUDEFROMCAPTURE`).
- Options: include cursor, click highlight, fps 5–60 (default 15), output scale.
- Selection overlay is a **native Win32 layered window per monitor** (magnifier, coordinates/size readout, pixel-precise keyboard nudging). Settings/editor UI is Tauri.

### M2 decisions

- **Layout**: Tauri 2 app in `app/` (`app/src-tauri` is a workspace member calling the core crates in-process; UI is Svelte 5 + TypeScript + Vite, pnpm). Native overlays live in the `zzalcam-overlay` crate.
- **Unified selection overlay** (one layered window per monitor, Direct2D, live desktop dimmed with the selection cut out, live magnifier): drag = region, hover + click = window, `Tab` = control mode (wheel walks parent/child), `F` = monitor under cursor, `A` = all monitors, `Enter` confirm, `Esc` cancel. The tray menu also enters each mode directly.
- **Default hotkeys** (one-handed, left hand; rebindable; registration failures are flagged as conflicts in settings):

  | Action | Hotkey |
  |---|---|
  | Select & record / stop | `Alt+Shift+1` |
  | Pause / resume | `Alt+Shift+2` |
  | Record last target again | `Alt+Shift+3` |
  | Cancel recording (discard) | `Alt+Shift+X` |

- **Cancel undo**: the last cancelled session is kept until the next recording starts; the toast offers "Undo".
- **Control bar** just outside the recorded area (below; above or a screen corner if there is no room): elapsed time, frame count, estimated size, pause / stop / cancel. Bar and border are excluded from capture.
- **After stop (M2)**: encode with the default preset to `Pictures\zzalcam\`, copy the file to the clipboard (CF_HDROP), show a toast (click opens the folder).
- **Settings window**: hotkeys, fps, cursor, format, preset, output folder. All strings are i18n keys from the start (ko, en).
- **Single instance**; closing the window hides to tray, quit only from the tray menu.

### M3 decisions

- **Editor**: one Tauri window per session, opened after recording (setting: open editor / save immediately). Frames reach the webview through a custom protocol (`zzalcam://…`): small thumbnails for the filmstrip, full-size lossy WebP (q85) for the preview, decoded from the session on demand.
- **Layout**: preview with crop handles in the centre; filmstrip below (thumbnails, trim handles, multi-select, playhead); side panel with format, preset, scale, max size, crop numbers and a live size estimate (sample encode, 0.5 s debounce); Save / Save as at the bottom.
- **Edits (M3)**: trim, delete frames, crop, real-time playback with frame stepping, undo/redo. Per-frame delays, speed and annotations come later.
- **Non-destructive**: edits are a spec (trim range, deleted frames, crop rect) applied at encode time through a `FrameSource` adapter; the session file is never rewritten.
- **Max size target**: sample-encode, then step down quality/colours → tolerance → fps (frame decimation) → scale (`fast_image_resize`), re-encoding once more if the final file still exceeds the target; the toast reports what was changed.
- **Scale** is an export option (editor + default in settings), not a capture option; recordings are always full resolution.
- **Save flow**: Save → default folder + filename pattern, clipboard copy, toast. Save as → file dialog. Closing unsaved asks first; discarding deletes the session unless "keep sessions" is on.
- **Crash recovery**: sessions left at startup are unsaved recordings; a prompt offers open in editor / save / delete (sessions older than 7 days are still removed).
- **Settings added**: after-record action, keep sessions, filename pattern (tokens), default export scale and max size, start with Windows.
- **Disk space**: warn before recording below 2 GB free; auto-stop below 500 MB with a toast.
- **WebP**: parallel encoding (own sub-frame diff, `WebPEncode` per frame in parallel, `WebPMux` assembly) if time allows, otherwise M4.

## 4. Session storage

- Raw frames go to a session file under `%LOCALAPPDATA%\zzalcam\sessions\`.
- Each frame stores only the dirty-rect diff against the previous frame, LZ4-compressed; only a few recent frames stay in memory.
- Crash recovery prompt on next launch. Sessions are deleted after save unless "keep sessions" is on.
- Free-space check before recording; auto-stop when space runs low.
- Fast-save path (no editor): quantisation runs in the background during recording so stop→saved is minimal.

## 5. Encoding

### GIF
Built on **libimagequant** (GPL-3.0) + the `gif` crate (MIT), with our own optimisation layer:

- Crop each frame to the changed bounding box.
- Unchanged pixels → transparent index.
- **Global palette first**: one palette sampled across the recording; switch to a per-frame local palette only when colour error exceeds a threshold.
- Floyd–Steinberg dithering via imagequant, but pixels unchanged since the previous frame reuse the previous output index (stable dithering: no flicker, maximal transparency).
- Merge duplicate frames, accumulating delays.
- Parallel quantisation across frames.

### WebP
libwebp `WebPAnimEncoder` (BSD) statically linked via `libwebp-sys`. Exposes lossy / lossless / mixed, `kmin`/`kmax`, `allow_mixed`.

### Presets
- Best quality / Balanced / Smallest, plus advanced settings (colours, dither strength, lossy tolerance).
- **Max size target** (e.g. 10 MB): encode a sample segment, estimate final size, binary-search quality parameters (then fps/scale).

## 6. Editor (MVP)

Trim start/end, delete frames, crop, preview.

## 7. Output

- Default folder `Pictures\zzalcam\`, filename pattern `zzalcam_{yyyyMMdd_HHmmss}.{ext}` (configurable).
- After save: copy file to clipboard (CF_HDROP) and show a toast.
- Setting: open editor after recording (default) vs. save immediately.
- Tray resident; start with Windows optional (off by default).

## 8. CLI

```
zzalcam record --monitor 1 --duration 5s -o out.gif
zzalcam record --window "Notepad" --fps 30 -o out.webp
zzalcam record --region 100,100,800,600 --max-size 8MB -o out.gif
zzalcam encode <session> --preset smallest -o out.gif
zzalcam list monitors|windows
```

Interactive region selection is GUI-only.

## 9. Performance targets

Reference: 1080p region, 15 fps, 10 s (150 frames), typical UI interaction, modern 8-core CPU.

- Average CPU while recording < 10 %.
- Stop → GIF saved ≤ 3 s (fast-save path).
- Output size ≤ gifski at comparable quality.
- Measured by CI benchmarks with regression alerts. Numbers are revisited after the first prototype.

## 10. Distribution

GitHub Releases: NSIS installer + portable zip. Tauri updater (can be disabled). GitHub Actions for build/test/release. winget/scoop after 1.0. Unsigned initially; apply to SignPath (free OSS signing) once there is traction.

## 11. Milestones

- **M1 — Core**: DXGI/WGC capture, session file, GIF/WebP encoders with frame optimisation, CLI, benchmarks.
- **M2 — GUI recording**: tray, hotkeys, native window/control/region overlay, recording border, countdown.
- **M3 — Editor & output**: trim/delete/crop/preview, presets, max-size mode, clipboard copy, i18n.
- **M4 — Distribution**: installer/portable, auto-update, CI release, README/docs.
