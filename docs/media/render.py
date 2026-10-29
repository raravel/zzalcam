"""Render banner.html frame by frame into a zzalcam session file.

    python docs/media/render.py banner target/banner.zzcs          # the README banner scene
    python docs/media/render.py clip target/clip.zzcs --scale 2    # the recorded card alone

Needs Python with `playwright` (and `playwright install chromium`) and Pillow. Frames are
streamed to the `frames_session` example of zzalcam-core; encode the result with zzalcam-cli.
"""
import argparse
import io
import os
import pathlib
import subprocess

from PIL import Image
from playwright.sync_api import sync_playwright

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SIZES = {"banner": (960, 480), "clip": (440, 250)}

ap = argparse.ArgumentParser()
ap.add_argument("part", choices=SIZES)
ap.add_argument("out")
ap.add_argument("--fps", type=int, default=20)
ap.add_argument("--scale", type=int, default=1)
args = ap.parse_args()
if args.fps <= 0 or args.scale <= 0:
    ap.error("--fps and --scale must be positive")

print("building frames_session…", flush=True)
subprocess.run(["cargo", "build", "--release", "-p", "zzalcam-core", "--example", "frames_session"],
               cwd=ROOT, check=True)
# Run the binary itself: killing `cargo run` would leave it behind on Windows.
exe = pathlib.Path(ROOT, os.environ.get("CARGO_TARGET_DIR", "target"), "release", "examples", "frames_session")

w, h = SIZES[args.part]
url = (HERE / "banner.html").as_uri() + ("?clip" if args.part == "clip" else "")
with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page(viewport={"width": w, "height": h}, device_scale_factor=args.scale)
    page.goto(url)
    page.wait_for_load_state("networkidle")
    n = round(page.evaluate("DURATION") * args.fps)
    pw, ph = w * args.scale, h * args.scale
    out = pathlib.Path(args.out).resolve()
    pack = subprocess.Popen([str(exe), str(pw), str(ph), str(args.fps), str(out)], cwd=ROOT, stdin=subprocess.PIPE)
    assert pack.stdin
    try:
        for i in range(n):
            page.evaluate(f"seek({i / args.fps})")
            img = Image.open(io.BytesIO(page.screenshot())).convert("RGBA")
            if img.size != (pw, ph):
                raise SystemExit(f"frame {i} is {img.size}, expected {(pw, ph)}")
            pack.stdin.write(img.tobytes())
    except BaseException:
        # Closing the pipe would look like a normal end, so stop the packer and drop the partial file.
        pack.kill()
        pack.wait()
        out.unlink(missing_ok=True)
        raise
    browser.close()
    pack.stdin.close()
    if pack.wait():
        raise SystemExit("frames_session failed")
    print(f"rendered {n} frames at {pw}x{ph}")
