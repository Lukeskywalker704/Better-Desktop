一键生成卡片：文件右键菜单（v0.1.9）

## 之前只有三条路
拖放（得先把桌面露出来）、点 `+` 走文件对话框、`Ctrl+Alt+D`（得先在资源管理器里选中/复制文件）。都绕。

## 现在
**右键任意文件 → 添加到 better desktop**，卡片直接出现，不用切窗口、不用先复制。

- 齿轮菜单 → 「加进文件右键菜单」一键安装（写 `HKCU\Software\Classes\*\shell\BetterDesktop`，只影响当前用户，随时可撤）
- 再次点击同一项即移除，或 `better-desktop.exe --uninstall-menu`
- 菜单项带程序图标，显示名就是「添加到 better desktop」

## 实现
1. exe 支持路径参数：`better-desktop.exe "D:\a.pdf" "D:\b.docx"`
2. 已有实例在跑时，第二个进程用 `WM_COPYDATA` 把路径列表转给托盘窗口，然后自己退出（单实例不受影响）
3. 收端把路径塞进队列，事件循环里走和拖放同一套 `add_paths()`（同样有螺旋找空位、去重、预览、横版适配）

顺带：单实例判断之前放在参数处理前面，导致 `--install-menu` 在程序已运行时直接退出，已调整为先处理命令行模式。

## 验证
- 右键安装后注册表确认：`(Default)=添加到 better desktop`、`Icon=<exe>`、`command="<exe>" "%1"` ✓
- 运行中执行 `better-desktop.exe "…复盘-图在第2页.pptx"` → 卡片数 **10 → 11**，新卡就是该文件 ✓
