# zzalcam Design

Fast, highly-optimized GIF / animated WebP screen recorder for Windows.
License: GPL-3.0.

## 1. Product shape

- Rust core library shared by a **GUI** (Tauri 2 + Svelte 5 + TypeScript, tray-resident) and a **CLI**.
- Target: Windows 10 1903+ (x64), Per-Monitor DPI Aware v2.
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
