"""Two decks: picture on slide 2, and text-only, to check the preview fallbacks."""
import os

from pptx import Presentation
from pptx.util import Inches

out_dir = "demo"


def make(path, picture_slide):
    prs = Presentation()
    prs.slide_width = Inches(13.333)
    prs.slide_height = Inches(7.5)

    # slide 1: text only
    s1 = prs.slides.add_slide(prs.slide_layouts[1])
    s1.shapes.title.text = "2026 年度复盘"
    body = s1.placeholders[1].text_frame
    body.text = "业务回顾与展望"
    p = body.add_paragraph()
    p.text = "内部资料 · 请勿外传"

    # slide 2 (optional): the picture
    s2 = prs.slides.add_slide(prs.slide_layouts[5])
    s2.shapes.title.text = "关键图表"
    if picture_slide and os.path.exists("docs/preview.png"):
        s2.shapes.add_picture("docs/preview.png", Inches(1.0), Inches(1.8), width=Inches(11.3))

    # slide 3: text
    s3 = prs.slides.add_slide(prs.slide_layouts[1])
    s3.shapes.title.text = "下一步"
    b3 = s3.placeholders[1].text_frame
    b3.text = "补齐数据看板"
    q = b3.add_paragraph()
    q.text = "Q1 完成灰度"

    prs.save(path)
    print("saved", path, os.path.getsize(path) // 1024, "KB")


make(os.path.join(out_dir, "复盘-图在第2页.pptx"), True)
make(os.path.join(out_dir, "复盘-纯文字.pptx"), False)
