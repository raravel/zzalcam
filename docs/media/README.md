# README media

`docs/images/banner.gif` is not a screen recording: zzalcam keeps its own overlays (picker, border,
control bar, notifications) out of every capture, so a real recording could not show them.
Instead `banner.html` redraws the flow with the app's own colours and text (timings compressed),
and the frames go through zzalcam's encoder like any recording.

Requirements: Windows 11 (the scenes use Segoe UI Variable), Python with `playwright`
(`python -m playwright install chromium`), Pillow and `websockets`, and a release build of the
app (`cd app && cargo tauri build --no-bundle`).

```
# 1. The recorded card alone, as a session the editor can open
python docs/media/render.py clip target/clip.zzcs --scale 2 --fps 15

# 2. Editor and settings screenshots (quit zzalcam first; settings and sessions are restored)
python docs/media/shots.py target/release/zzalcam-app.exe target/clip.zzcs en
python docs/media/shots.py target/release/zzalcam-app.exe target/clip.zzcs ko

# 3. The banner (it shows editor-en-dark.png, so take the screenshots first)
python docs/media/render.py banner target/banner.zzcs
cargo run --release -p zzalcam-cli -- encode target/banner.zzcs --preset best -o docs/images/banner.gif
```

`best` is used because the wallpaper gradient bands visibly with `balanced`.
