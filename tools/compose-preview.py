"""Composite the dumped frame over a neutral gradient and save docs/preview.png."""
import os
import struct

import numpy as np
from PIL import Image

cfg = os.path.join(os.environ["APPDATA"], "BetterDesktop", "frame.bmp")
data = open(cfg, "rb").read()
w, h = struct.unpack_from("<ii", data, 18)
h = abs(h)
px = np.frombuffer(data, dtype=np.uint8, count=w * h * 4, offset=54).reshape(h, w, 4).astype(np.float32)
alpha = px[:, :, 3:4] / 255.0
rgb = np.where(alpha > 0.004, np.clip(px[:, :, :3] / np.maximum(alpha, 1e-4), 0, 255), 0)
rgba = np.concatenate([rgb[:, :, 2:3], rgb[:, :, 1:2], rgb[:, :, 0:1], px[:, :, 3:4]], axis=2)
layer = Image.fromarray(rgba.astype(np.uint8), "RGBA")

# neutral slate gradient: no third-party wallpaper gets redistributed
top = np.array([38, 44, 58], dtype=np.float32)
bottom = np.array([22, 26, 36], dtype=np.float32)
ramp = np.linspace(0.0, 1.0, h, dtype=np.float32)[:, None, None]
grad = (top[None, None, :] * (1 - ramp) + bottom[None, None, :] * ramp)
grad = np.repeat(grad, w, axis=1)
grad = np.dstack([grad, np.full((h, w, 1), 255.0, dtype=np.float32)])
bg = Image.fromarray(grad.astype(np.uint8), "RGBA")
bg.alpha_composite(layer)

out = bg.convert("RGB")
os.makedirs("docs", exist_ok=True)
# keep it light: 50 % scale, 8-bit PNG
out.resize((out.width // 2, out.height // 2), Image.LANCZOS).save("docs/preview.png", optimize=True)
print("docs/preview.png", os.path.getsize("docs/preview.png") // 1024, "KB")
