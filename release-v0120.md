# 内存占用 224 MB → 60 MB：脏区渲染 + 子进程 PDF 引擎（v0.1.20）

## 脏区渲染（流畅度）
- 渲染改为**脏区**：每帧只清理/拷贝卡片、工具栏、引导线实际覆盖的矩形，
  屏幕其余区域（本来透明）不再参与 23.6 MB 的全屏 memset + 拷贝
- Framebuffer 直接借用 Presenter 的 DIB 内存：渲染写入的就是
  UpdateLayeredWindow 读取的内存，**每帧少一次全屏拷贝，常驻少 22.5 MB**
- 卡片几何提取为 `card_dst_rect`，脏区矩形与实际绘制严格一致（BD_DUMP 逐像素验证）

## 子进程 PDF 引擎（内存大头）
- 定位：一张 PDF 卡片让 Private 从 33 MB 涨到 196 MB——Windows.Data.Pdf
  （Direct2D + DWrite + 字体缓存）一旦加载就在进程内常驻 ~160 MB
- 现在 PDF 栅格化在**短命子进程**里完成：`better-desktop.exe
  --render-pdf=<路径>|<页>|<宽>` 输出 PNG 到 stdout 后退出，引擎内存随子进程
  释放；主进程失败时回退进程内渲染，预览功能不受影响

## 缓存上限（防无界增长）
- 卡片位图缓存 48 → 16（每张 1–5 MB，随缩放增长）
- PDF 页栅格缓存 16 → 6（每页 ~6 MB）
- 文字位图缓存加 512 上限（滚轮缩放会无界铸造新字号位图）
- 纯文字文档（docx/xlsx/md/txt/csv）不再生成 640-px Shell 缩略图（从未被绘制）
- 幻灯片模型加缓存：PPTX 卡片折叠/悬停动画的每帧不再重新解压整个 deck

## 其他
- `BD_DUMP`/`BD_MEM`/`BD_PREVIEW` 加入单实例豁免列表（此前 dump 实例会被
  运行中的实例拒绝，静默拿不到帧）
- 新增 `BD_MEM` 探针：打印 fb/DIB/卡片/缩略图/文字/背景各大分配

## 验证
- 空配置 Private 28.3 MB；单 PNG 卡 33.1 MB；单 PDF 卡（子进程）33.2 MB
  （改动前同场景 196 MB）
- `BD_BENCH=200`：3.98 ms/frame（6 卡）；折叠/预览/翻页 dump 回归通过
- 子进程端到端：demo PDF 第一页栅格化 1120×1585 PNG ✓
