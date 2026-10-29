"""Take the README screenshots of the editor and settings windows, in light and dark.

    python docs/media/shots.py target/release/zzalcam-app.exe target/clip.zzcs en
    python docs/media/shots.py target/release/zzalcam-app.exe target/clip.zzcs ko

Quit zzalcam first. The script switches the UI language, puts the clip session up for
recovery, starts the app with WebView2's DevTools port open, opens the session in the editor,
steps to the last frame and screenshots both windows through the DevTools protocol (the colour
scheme is emulated, so the Windows theme does not matter). Afterwards it restores settings.json
and any sessions it moved aside. Needs Python with `websockets`; installed (non-portable) builds only.
"""
import asyncio
import base64
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

import websockets

PORT = 9338
OUT = pathlib.Path(__file__).resolve().parent.parent / "images"

USAGE = "usage: shots.py <zzalcam-app.exe> <clip.zzcs> <en|ko>"
if len(sys.argv) != 4:
    sys.exit(USAGE)
exe, clip, lang = pathlib.Path(sys.argv[1]).resolve(), pathlib.Path(sys.argv[2]).resolve(), sys.argv[3]
if not (exe.exists() and clip.exists() and lang in ("en", "ko")):
    sys.exit(USAGE)
settings = pathlib.Path(os.environ["APPDATA"], "dev.zzalcam.app", "settings.json")
sessions = pathlib.Path(os.environ["LOCALAPPDATA"], "dev.zzalcam.app", "sessions")


async def cdp(page, calls):
    async with websockets.connect(page["webSocketDebuggerUrl"], max_size=None) as ws:
        results = []
        for n, (method, params) in enumerate(calls, 1):
            await ws.send(json.dumps({"id": n, "method": method, "params": params}))
            while True:
                m = json.loads(await asyncio.wait_for(ws.recv(), 30))
                if m.get("id") == n:
                    results.append(m.get("result", m.get("error")))
                    break
        return results


def run(page, calls):
    return asyncio.run(cdp(page, calls))


def ev(page, expr):
    r = run(page, [("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})])[0]
    return r.get("result", {}).get("value", r)


def pages():
    with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json") as r:
        return [p for p in json.load(r) if p.get("type") == "page"]


def wait_for(cond, what, timeout=30):
    """Poll the open pages until `cond(pages)` is true."""
    end = time.time() + timeout
    while time.time() < end:
        try:
            ps = pages()
            if cond(ps):
                return ps
        except OSError:
            pass  # DevTools port not up yet
        time.sleep(0.5)
    raise SystemExit(f"timed out waiting for {what}")


def shoot(page, name):
    for scheme in ("light", "dark"):
        res = run(page, [
            ("Emulation.setEmulatedMedia", {"features": [{"name": "prefers-color-scheme", "value": scheme}]}),
            ("Runtime.evaluate", {"expression": "new Promise(r => setTimeout(r, 400))", "awaitPromise": True}),
            ("Page.captureScreenshot", {"format": "png"}),
        ])
        path = OUT / f"{name}-{lang}-{scheme}.png"
        path.write_bytes(base64.b64decode(res[2]["data"]))
        print("saved", path)


def invoke(page, cmd, args):
    return ev(page, f"window.__TAURI_INTERNALS__.invoke({json.dumps(cmd)}, {json.dumps(args)})"
                    ".then(() => 'ok', e => 'ERR ' + e)")


backup = pathlib.Path(tempfile.mkdtemp(prefix="zzalcam-shots-"))
had_settings = settings.exists()  # the app writes it only once settings are saved
if had_settings:
    shutil.copy(settings, backup / "settings.json")
moved = []
seeded = sessions / "20261028-142310.zzcs"
app = None
try:
    for f in sessions.iterdir():
        shutil.move(f, backup / f.name)
        moved.append(f.name)
    s = json.loads(settings.read_text(encoding="utf-8")) if had_settings else {}
    s["language"] = lang
    settings.write_text(json.dumps(s, indent=2), encoding="utf-8")
    shutil.copy(clip, seeded)

    env = dict(os.environ, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={PORT}")
    app = subprocess.Popen([str(exe)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ps = wait_for(lambda ps: len(ps) >= 3, "the app windows")
    time.sleep(1)  # let the Svelte views mount
    main = next(p for p in ps if "editor=" not in p["url"])
    print("settings:", invoke(main, "plugin:window|show", {"label": "main"}))
    print("editor:", invoke(main, "recover", {"action": "open"}))
    ps = wait_for(lambda ps: any("editor=" in p["url"] for p in ps), "the editor")
    time.sleep(2)  # first frames and thumbnails
    for p in ps:
        text = ev(p, "document.body.innerText.slice(0, 60)") or ""
        if "editor=" in p["url"]:
            ev(p, "document.activeElement?.blur()")
            for _ in range(60):  # to the last frame
                for t in ("keyDown", "keyUp"):
                    run(p, [("Input.dispatchKeyEvent",
                             {"type": t, "key": "ArrowRight", "code": "ArrowRight", "windowsVirtualKeyCode": 39})])
            time.sleep(1.5)  # let the size estimate settle
            shoot(p, "editor")
        elif text.startswith(("Settings", "설정")):
            shoot(p, "settings")
finally:
    if app:
        app.kill()
        app.wait()
        time.sleep(1)
    if had_settings:
        shutil.copy(backup / "settings.json", settings)
    else:
        settings.unlink(missing_ok=True)
    seeded.unlink(missing_ok=True)
    for name in moved:
        shutil.move(backup / name, sessions / name)
    shutil.rmtree(backup)
    print(f"restored settings.json and {len(moved)} session(s)")
