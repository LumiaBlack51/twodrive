# Documentation assets · 文档素材

[English homepage](../../README.md) · [中文首页](../../README.zh-CN.md)

## Real screenshot / 真实截图

[tray-menu.png](tray-menu.png) is the owner's supplied screenshot of the actual idle TwoDrive tray menu. The entire local token-file path was replaced with an **opaque redaction before resizing**. The 640 × 477 PNG contains no source metadata. The unredacted original is not committed.

图片来自项目所有者提供的真实空闲托盘菜单，缩放前已用不透明遮盖移除完整本地令牌文件路径，导出图片无原图元数据。除脱敏、缩放和压缩外没有修改菜单，也没有补造 Nautilus、Settings 或传输画面。

A screenshot documents appearance, not implementation of every visible control. Pause sync is currently display-only; see [desktop behavior](../usage.md#tray-and-settings).

## Shipped emblems / 应用内状态图标

The homepages reference the existing [status SVGs](../../packaging/icons/hicolor/scalable/emblems) directly, without redrawing or duplicating the icon set. Their state mapping comes from [Nautilus](../../packaging/nautilus/twodrive_nautilus.py).

| Asset | Purpose |
| --- | --- |
| [Cloud](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-cloud.svg) | Online-only; reused for the homepage's cloud visual. |
| [Syncing](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-syncing.svg) | Hydrating, writing, queued, or uploading. |
| [Synced](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-synced.svg) | Locally cached content. |
| [Pinned](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-pinned.svg) | Always Keep. |
| [Error](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-error.svg) | Error or conflict. |

The [tray](../../scripts/twodrive-tray), [autostart entry](../../packaging/autostart/twodrive-tray.desktop), and [Settings launcher](../../packaging/applications/twodrive-settings.desktop) use `folder-cloud`. Its appearance is supplied by the installed icon theme; the repository has no standalone application logo. The README uses the existing cloud **status emblem**, identifies it as such, and does not invent a different brand mark.

应用入口和空闲托盘的 `folder-cloud` 来自桌面主题，并非仓库内置 Logo。首页复用已有云端状态图标，同时明确其来源，没有重画应用品牌标识。
