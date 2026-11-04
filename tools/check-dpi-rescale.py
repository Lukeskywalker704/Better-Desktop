"""Check that a card carried over to the 125 % monitor is rescaled by 120/168."""

import json
import os
import shutil
import struct
import subprocess
import time

import numpy as np

cfg_dir = os.path.join(os.environ["APPDATA"], "BetterDesktop")
cfg = os.path.join(cfg_dir, "config.json")
backup = cfg + ".bak"

# a card that says it was laid out for the 175 % monitor, but sits on the 125 % one
card = {
    "x": 400.0, "y": -1300.0, "width": 743.0, "height": 390.0,
    "path": r"D:\better desktop\demo\项目计划书.docx",
    "color": [120, 120, 120], "auto_fit": False, "collapsed": False, "dpi": 168.0,
}

try:
    shutil.copy(cfg, backup)
    json.dump({"cards": [card], "topmost": True, "rail": False},
              open(cfg, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    subprocess.run(["powershell", "-NoProfile", "-Command",
                    "Get-Process better-desktop -ErrorAction SilentlyContinue | Stop-Process -Force"])
    time.sleep(1)
    proc = subprocess.Popen([r"target\debug\better-desktop.exe"], env=dict(os.environ, BD_DUMP="1"))
    time.sleep(6)
    proc.kill()
    time.sleep(1)

    data = open(os.path.join(cfg_dir, "frame.bmp"), "rb").read()
    w, h = struct.unpack_from("<ii", data, 18)
    h = abs(h)
    px = np.frombuffer(data, dtype=np.uint8, offset=54, count=w * h * 4).reshape(h, w, 4)
    alpha = px[:, :, 3]
    ys, xs = np.where(alpha > 40)
    if len(xs) == 0:
        print("nothing rendered")
    else:
        # the window origin is the virtual screen origin, so the card rect is in
        # frame coordinates once shifted by the monitor offset
        print("rendered bbox: x %d..%d (w=%d)  y %d..%d (h=%d)" % (
            xs.min(), xs.max(), xs.max() - xs.min(), ys.min(), ys.max(), ys.max() - ys.min()))
    # what the app said about the monitors
    out = subprocess.run([r"target\debug\better-desktop.exe"], env=dict(os.environ, BD_DPI="1"),
                         capture_output=True, text=True)
    print(out.stderr.strip())
finally:
    if os.path.exists(backup):
        shutil.copy(backup, cfg)
        os.remove(backup)
    subprocess.run(["powershell", "-NoProfile", "-Command", "Start-Process '.\\better-desktop.exe'"])
    print("config restored, app started")
