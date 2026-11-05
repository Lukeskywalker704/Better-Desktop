真实文件图标 + 标题对比度自适应（v0.1.11）

## 真实文件图标
标题栏不再用字母色块，改为**系统真实图标**：`SHGetFileInfoW(SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES)`
（用伪造文件名查询"类型图标"，不碰磁盘），`GetIconInfo` 取出 HBITMAP 后走既有的 `hbitmap_to_bgra` 转成 BGRA，按扩展名缓存。
图标取不到时（自定义扩展名等）回退到原来的字母色块。

效果：Word / Excel / PDF / PowerPoint 显示的就是资源管理器里那个图标。

## 标题对比度自适应
标题文字的墨色现在**跟着卡片下方的壁纸亮度走**：亮壁纸用深色字、暗壁纸用白字，不再依赖多层阴影"描边"
（之前白字 + 深阴影在浅色壁纸上会看成重影）。新加 `Backdrop::luminance()` 在 1/4 缩放的背景上采样。

## 顺带修的两个 bug
1. **`BD_DUMP` 模式会挂死**：`paint_header` 在绘制期间再次去锁 `BACKDROP`，与渲染路径已持有的锁形成死锁。改为由调用方把 `backdrop` 传进来，不再重入锁。
2. dump 模式渲染一帧后没有退出，会作为第二个覆盖层常驻（和单实例互斥体冲突）。现在写完 `frame.bmp` 直接 `exit(0)`。
