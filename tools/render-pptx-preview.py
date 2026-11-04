"""Render four pptx cards and always restore the real config afterwards."""
import json
import os
import shutil
import struct
import subprocess
import time

import numpy as np
from PIL import Image

cfg_dir = os.path.join(os.environ["APPDATA"], "BetterDesktop")
cfg = os.path.join(cfg_dir, "config.json")
backup = cfg + ".bak"

names = [
    "产品发布会-带图.pptx",       # wide picture on slide 1
    "复盘-图在第2页.pptx",        # wide picture on slide 2
    "复盘-纯文字.pptx",           # no picture at all
    os.path.join("..", "..", "..", "E:", "Taichi", "太极拳_起源历史流派与发展.pptx"),  # square logo only
]
cards = []
for i, name in enumerate(names):
    path = name if os.path.isabs(name) else os.path.join(r"D:\better desktop\demo", name)
    cards.append({
        "x": 120.0 + i * 700, "y": 120.0, "width": 600.0, "height": 380.0,
        "path": os.path.normpath(path),
        "color": [120, 120, 120], "auto_fit": True, "collapsed": False,
    })

ok = False
try:
    shutil.copy(cfg, backup)
    json.dump({"cards": cards, "topmost": True, "rail": False},
              open(cfg, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Get-Process better-desktop -ErrorAction SilentlyContinue | Stop-Process -Force"])
    time.sleep(1)
    env = dict(os.environ, BD_DUMP="1")
    proc = subprocess.Popen([r"target\release\better-desktop.exe"], env=env)
    time.sleep(6)
    proc.kill()
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Get-Process better-desktop -ErrorAction SilentlyContinue | Stop-Process -Force"])

    data = open(os.path.join(cfg_dir, "frame.bmp"), "rb").read()
    w, h = struct.unpack_from("<ii", data, 18)
    h = abs(h)
    px = np.frombuffer(data, dtype=np.uint8, offset=54, count=w * h * 4).reshape(h, w, 4).astype(np.float32)
    a = px[:, :, 3:4] / 255.0
    rgb = np.where(a > 0.004, np.clip(px[:, :, :3] / np.maximum(a, 1e-4), 0, 255), 0)
    rgba = np.concatenate([rgb[:, :, 2:3], rgb[:, :, 1:2], rgb[:, :, 0:1], px[:, :, 3:4]], axis=2)
    layer = Image.fromarray(rgba.astype(np.uint8), "RGBA")
    bg = Image.new("RGBA", (w, h), (30, 36, 48, 255))
    bg.alpha_composite(layer)
    bg.convert("RGB").crop((90, 60, 2960, 640)).resize((1440, 290), Image.LANCZOS).save("docs/pptx-preview.png")
    ok = True
finally:
    if os.path.exists(backup):
        shutil.copy(backup, cfg)
        os.remove(backup)
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Start-Process '.\\better-desktop.exe'"])
    print("config restored, app started")

print("saved docs/pptx-preview.png" if ok else "render failed")
