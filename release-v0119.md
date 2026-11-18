# 排版层级 + 纸面材质 + 元信息脚注（v0.1.19）

## 为什么改
上一版卡片预览"能读"但不够"高级"：标题只有一级、正文用 secondaryLabel 的浅灰（对比度不足）、表格表头 alpha 26 几乎看不见、数字左对齐参差、占位图硬编码墨色不理主题。这轮按 Apple 排版习惯把信息层级立起来。

## 排版层级（`TypeScale` 设计令牌）
- `type_scale(s)`：Title 17s w700 / H1 13s w600 / H2 11.5s w600 / Body 10.5s w400 / Caption 9.5s w400
- `Block::Heading(String)` → `Block::Heading(u8, String)`：DOCX 解析 `pStyle` 尾数字（Heading2 → level 2），Markdown 数 `#` 个数（`##` → level 2，≥3 钳到 2）
- 标题与正文的行高/段后距全部归一到令牌（1.35 / 1.40 / 1.55），绘制不再散落字面量

## 墨色
- 新增 `Palette::body`：浅色 (60,60,66)、深色 (216,216,220)——正文不再借用 secondaryLabel 的浅灰
- 表头文字改用 `ink` w600；脚注用 `ink2`

## 表格
- 表头带从 accent α26 改成中性 `rule` α90（深浅色通吃），表头下分隔线 α160
- 数据行斑马纹（奇数行 `rule` α40），行高 19s → 21s，单元格内边距 5s/4s → 8s/5.5s
- 数字单元格右对齐（`is_numeric_cell`：trim + 去千分位逗号后可 parse 为 f64），列宽仍等分

## 纸面
- 圆角 14s → 17s；漫射阴影 blur 24s/α46 → 26s/α40（offset 7s），接触阴影 blur 3s/α34 → 3.5s/α30——更大更软更低调

## 元信息脚注
- 文档类卡片底部新增 `EXT · 大小 · YYYY-MM-DD`（发丝线 + caption 字号），折叠态自动隐藏，图片/幻灯片/PDF 栅格页卡片不显示
- 日期 = mtime 经本地时区偏移（GetLocalTime vs GetSystemTime）换算，纯 std 实现
- 卡片位图缓存 key 追加 mtime secs，文件变更后脚注能刷新

## 文字渲染
- GDI quality 从写死的 ClearType(5) 换成灰度 AA(4)（`BD_FONT_AA` 环境变量可覆盖回 5）。管线是 `alpha = max(r,g,b)`，ClearType 的亚像素彩晕会被放大成彩色边——灰度版在 2× 放大对比下边缘更干净

## 占位图
- `draw_placeholder_page` 的硬编码墨色全部换成 palette（ink/ink2/rule），深色模式不再发灰

## 验证
- `tools/make-preview-config.py` + `BD_DUMP` + `compose-preview.py`：`docs/before-light|dark.png` vs `after-light|dark.png`
- `BD_PROBE="demo/会议纪要.md"`：`##` 现在输出 `Heading(1, ...)`（首个非 Title 块为 1 级，`##` 会被钳到 2——该文件只有一级标题）
- 表格数字右对齐/斑马纹/中性表头在 xlsx 卡片可见
- `BD_OPEN=0` 折叠回归：页眉胶囊，无脚注 ✓
- `BD_BENCH=200`：6.74 ms/frame（5 卡，与改前持平），冷首帧 124 ms
- AA A/B：`BD_FONT_AA=4` vs `5` 2× 放大对比后采用 4
