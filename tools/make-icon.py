"""Draw the app icon and write icon.ico + docs/icon.png."""
import os

from PIL import Image, ImageDraw, ImageFilter

S = 1024  # master size, downsampled to every icon size afterwards

card = (252, 252, 253)
card_edge = (214, 224, 238)
accent = (74, 124, 240)      # same blue family as the UI selection colour
accent_dark = (54, 96, 200)
line = (196, 205, 219)
line_soft = (222, 228, 238)

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# ── soft drop shadow ──
shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(shadow)
margin = int(S * 0.13)
box = (margin, int(S * 0.15), S - margin, S - int(S * 0.13))
d.rounded_rectangle(box, radius=int(S * 0.15), fill=(16, 26, 48, 120))
shadow = shadow.filter(ImageFilter.GaussianBlur(S * 0.035))
img.alpha_composite(shadow)

# ── paper card ──
card_box = (margin, int(S * 0.11), S - margin, S - int(S * 0.10))
radius = int(S * 0.145)
paper = Image.new("RGBA", (S, S), (0, 0, 0, 0))
dp = ImageDraw.Draw(paper)
dp.rounded_rectangle(card_box, radius=radius, fill=card + (255,))
# glass rim: a brighter line just inside the edge, like the card's paper edge
dp.rounded_rectangle(
    (card_box[0] + int(S * 0.012), card_box[1] + int(S * 0.012),
     card_box[2] - int(S * 0.012), card_box[3] - int(S * 0.012)),
    radius=radius - int(S * 0.01), outline=(255, 255, 255, 235), width=int(S * 0.02))
dp.rounded_rectangle(card_box, radius=radius, outline=card_edge + (255,), width=int(S * 0.012))
img.alpha_composite(paper)

# ── accent header band (top of the card) ──
band_h = int(S * 0.26)
band_box = (card_box[0] + int(S * 0.03), card_box[1] + int(S * 0.03),
            card_box[2] - int(S * 0.03), card_box[1] + band_h)
# vertical gradient inside a rounded mask: glass-like, no hard seam
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle(band_box, radius=int(S * 0.075), fill=255)
grad = Image.new("RGBA", (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(grad)
for y in range(band_box[1], band_box[3]):
    t = (y - band_box[1]) / max(1, band_box[3] - band_box[1])
    col = tuple(int(accent[i] * (1 - 0.42 * t) + accent_dark[i] * (0.42 * t)) for i in range(3))
    gd.line([(band_box[0], y), (band_box[2], y)], fill=col + (255,))
band = Image.new("RGBA", (S, S), (0, 0, 0, 0))
band.paste(grad, (0, 0), mask)
img.alpha_composite(band)

# ── text lines on the paper ──
lines = Image.new("RGBA", (S, S), (0, 0, 0, 0))
dl = ImageDraw.Draw(lines)
top = card_box[1] + band_h + int(S * 0.075)
height = int(S * 0.055)
gap = int(S * 0.055)
widths = [0.72, 0.86, 0.58]
for i, wfrac in enumerate(widths):
    y = top + i * (height + gap)
    if y + height > card_box[3] - int(S * 0.06):
        break
    dl.rounded_rectangle(
        (card_box[0] + int(S * 0.075), y,
         card_box[0] + int(S * 0.075 + (card_box[2] - card_box[0] - S * 0.15) * wfrac), y + height),
        radius=height // 2, fill=(line if i < 2 else line_soft) + (255,))
img.alpha_composite(lines)

os.makedirs("docs", exist_ok=True)
img.resize((1024, 1024), Image.LANCZOS).save("docs/icon.png")

sizes = [256, 128, 96, 64, 48, 32, 24, 20, 16]
frames = [img.resize((s, s), Image.LANCZOS) for s in sizes]
frames[0].save("icon.ico", format="ICO", sizes=[(s, s) for s in sizes])
print("icon.ico", os.path.getsize("icon.ico") // 1024, "KB  sizes:", sizes)
