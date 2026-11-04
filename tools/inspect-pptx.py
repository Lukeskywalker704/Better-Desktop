"""What images does the user's deck actually carry?"""
import json
import os
import re
import zipfile

cfg = os.path.join(os.environ["APPDATA"], "BetterDesktop", "config.json")
cards = json.load(open(cfg, encoding="utf-8"))["cards"]
deck = next((c["path"] for c in cards if c["path"].lower().endswith(".pptx")), None)
print("deck:", deck, os.path.getsize(deck) // 1024, "KB")

z = zipfile.ZipFile(deck)
names = z.namelist()
media = [n for n in names if n.startswith("ppt/media/")]
print("media:", media[:12], "…" if len(media) > 12 else "")

for n in sorted(n for n in names if re.match(r"ppt/slides/slide\d+\.xml$", n))[:5]:
    xml = z.read(n).decode("utf-8", "replace")
    embeds = re.findall(r'r:embed="([^"]+)"', xml)
    print(f"{n}: len={len(xml)} embeds={embeds}")

for n in sorted(n for n in names if re.match(r"ppt/slideLayouts/slideLayout\d+\.xml$", n))[:3]:
    xml = z.read(n).decode("utf-8", "replace")
    embeds = re.findall(r'r:embed="([^"]+)"', xml)
    bg = "<p:bg>" in xml
    print(f"{n}: background={bg} embeds={embeds}")

master = [n for n in names if re.match(r"ppt/slideMasters/slideMaster\d+\.xml$", n)]
print("masters:", master)
for n in master[:2]:
    xml = z.read(n).decode("utf-8", "replace")
    print(f"{n}: embeds={re.findall(r'r:embed=\"([^\"]+)\"', xml)[:5]} bg={'<p:bg>' in xml}")
