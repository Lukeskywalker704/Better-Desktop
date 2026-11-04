"""Build a small deck with a picture + text so the pptx preview can be checked."""
import os

from pptx import Presentation
from pptx.util import Inches, Pt

prs = Presentation()
prs.slide_width = Inches(13.333)
prs.slide_height = Inches(7.5)

# slide 1: title + one picture (this is what the card should preview)
s1 = prs.slides.add_slide(prs.slide_layouts[5])
s1.shapes.title.text = "2026 产品发布会"
pic_path = "docs/preview.png"
if os.path.exists(pic_path):
    s1.shapes.add_picture(pic_path, Inches(0.6), Inches(1.9), width=Inches(12.1))
sub = s1.shapes.add_textbox(Inches(0.6), Inches(1.2), Inches(9), Inches(0.7))
sub.text_frame.text = "创新驱动未来 · 上海"

# slide 2 and 3: text only
for title, bullets in [
    ("议程", ["过去一年的增长", "新品矩阵", "开放生态", "下一步计划"]),
    ("关键数据", ["营收 12.4 亿，同比 +38%", "活跃用户 4200 万", "企业客户 8600 家"]),
]:
    s = prs.slides.add_slide(prs.slide_layouts[1])
    s.shapes.title.text = title
    body = s.placeholders[1].text_frame
    body.text = bullets[0]
    for b in bullets[1:]:
        p = body.add_paragraph()
        p.text = b

os.makedirs("demo", exist_ok=True)
out = "demo/产品发布会-带图.pptx"
prs.save(out)
print("saved", out, os.path.getsize(out) // 1024, "KB")
