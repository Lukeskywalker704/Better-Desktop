弹簧动效（v0.1.14）

## 之前
- 入场：0.35 s 三次缓出（`1 - (1-e)^3`），上滑 18 px —— 是"减速到停"，没有回弹
- 折叠 / 展开：瞬时切换，纸面直接消失或出现

## 现在

### 弹簧曲线
解析解阻尼振子，没有缓动表、没有逐帧积分：

```rust
fn spring(t, response, damping) -> f32 {
    let omega = TAU / response;
    let wd = omega * (1 - damping²).max(1e-4).sqrt();
    1 - e^(-damping·omega·t) · (cos(wd·t) + damping·omega/wd · sin(wd·t))
}
```

- 入场：response **0.42 s**、damping **0.82**，上滑从 18 px 收到 **8 px**
- 折叠：response **0.34 s**、damping **0.78**

### 折叠是真正的插值
`FoldAnim { from, to, at }` 每张卡一份，状态翻转时以"当前开合度"为起点重新起弹。渲染时：

- 纸面高度 `h = h_folded + (h_full - h_folded) * open`
- 纸面随 `open` 淡入，胶囊随 `(1-open)²` 淡出（平方曲线让胶囊只在最后 40% 才出现，不会和纸面重叠成一坨灰条）
- 动画期间绕过位图缓存（`anim < 0.999 || open < 0.999` 走直绘），结束后落回缓存

### 驱动
`sync_fold_anims()` 每帧检测状态翻转并清理过期项；只要还有折叠动画在跑，就用 `ControlFlow::WaitUntil(+16ms)` 维持 60 fps 重绘，跑完自动回到 `Wait`（不空转 CPU）。

## 验证
`BD_OPEN=0.6 / 0.3 / 0.0` 可把卡片冻结在半开状态；四连拍见 `docs/fold-spring.png`：整张纸 → 纸面变矮+胶囊淡入 → 只剩胶囊。

## 没做
`整理成右侧一列` 的逐张错峰（Stage Manager 那种 20–30 ms 错位）需要把"视觉位置"和"存储位置"拆开，属于下一轮。
