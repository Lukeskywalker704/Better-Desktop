"""Build a clean hero-shot config, dump a frame, and restore the user's config."""
import json
import os
import shutil

cfg_path = os.path.join(os.environ["APPDATA"], "BetterDesktop", "config.json")
backup = cfg_path + ".bak"
shutil.copy(cfg_path, backup)

root = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "demo")
files = [
    ("项目计划书.docx", 290),
    ("季度数据汇总表.xlsx", 290),
    ("SWA2-2026-Summer.pdf", 330),
    ("产品发布会演讲.pptx", 290),
    ("会议纪要.md", 380),
]
cards = []
y = 70.0
for name, h in files:
    path = os.path.join(root, name).replace("/", "\\")
    cards.append({
        "x": 2259.0,
        "y": y,
        "width": 743.0,
        "height": float(h),
        "path": path,
        "color": [120, 120, 120],
        "auto_fit": False,
        "collapsed": False,
    })
    y += h + 28.0

assert y - 28.0 <= 1808 - 28, "column does not fit"
json.dump({"cards": cards, "topmost": True, "rail": False},
          open(cfg_path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("hero config:", len(cards), "cards, bottom", y - 28)
