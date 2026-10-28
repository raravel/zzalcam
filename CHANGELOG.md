# Changelog

All notable changes to zzalcam are listed here. Versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] — unreleased

First release.

### Recording
- Capture a window (follows it as it moves), a single UI control, a free region (may span
  monitors), one monitor or all monitors; DXGI Desktop Duplication and Windows.Graphics.Capture.
- Unified picker with live dimming, size label and magnifier; hotkeys `Alt+Shift+1/2/3/X`
  (rebindable, conflicts are reported).
- Variable frame rate, 5–60 fps, cursor capture, 3-second countdown, pause, cancel with undo.
- Border, control bar, picker and notifications are excluded from capture.
- Disk space warning before recording and automatic stop when the drive is nearly full.

### Encoding
- GIF: global palette with per-frame fallback, change-box cropping, transparency for unchanged
  pixels, stable dithering, parallel remapping and LZW.
- WebP: parallel sub-frame encoding assembled with WebPMux (lossless / mixed); lossy via libwebp's
  animation encoder.
- Presets (best / balanced / smallest), export scale, maximum file size targets.

### Editing and output
- Editor: trim, delete frames, crop, real-time preview, undo/redo, live size estimate, save as.
- Saved files are copied to the clipboard; configurable file names and output folder.
- Crash recovery for unsaved recordings; option to keep raw recordings.

### App
- Tray app with Korean and English UI, start with Windows, automatic updates, portable mode.
- `zzalcam-cli` for recording and encoding from the command line.
