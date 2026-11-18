# 修复：全新实例偶发静默退出（v0.1.21）

## 现象
单实例检查用的 `CreateMutexW` + `GetLastError() == ERROR_ALREADY_EXISTS` 偶尔在
**没有任何实例运行**时误判"已有实例"，导致新启动的 overlay 直接静默退出——
表现为桌面卡片"随机消失"，重启几次才回来。

## 原因
`CreateMutexW` 成功创建全新互斥体时不保证重置线程的 LastError；如果同一线程
之前的某个 API 调用恰好留下 `ERROR_ALREADY_EXISTS (183)`，紧随其后的
`GetLastError()` 就会读到陈旧的 183，被当成"已有实例"。

## 修复
创建互斥体之前先 `SetLastError(0)`，让 `ERROR_ALREADY_EXISTS` 检查只反映
本次调用的真实结果。

## 验证
- 无实例时启动 → 正常运行 ✓
- 实例运行中重复启动 → 第二实例退出，保持 1 个 ✓
