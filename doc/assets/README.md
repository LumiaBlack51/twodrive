# Documentation assets · 文档素材

[English homepage](../../README.md) · [中文首页](../../README.zh-CN.md)

## Screenshots / 真实截图

| Asset | Content | Dimensions |
| --- | --- | --- |
| [tray-menu.png](tray-menu.png) | Idle TwoDrive tray menu / 空闲托盘菜单 | 640 × 477 |
| [nautilus-file-status.png](nautilus-file-status.png) | File status emblems in Nautilus / 文件状态标记 | 1491 × 617 |
| [nautilus-context-menu.png](nautilus-context-menu.png) | TwoDrive actions with file-manager context / 带文件管理器背景的右键菜单 | 712 × 750 |

All screenshots were supplied by the project owner. The Nautilus captures retain their original framing, resolution, and pixels; the PNGs are losslessly optimized and contain no source metadata. No interface elements have been added, removed, translated, or redrawn.

截图均由项目所有者提供。两张 Nautilus 截图保留原始构图、分辨率和像素，仅进行 PNG 无损压缩并移除元数据；未增删、翻译或重绘界面元素。

The existing tray screenshot is unchanged. Its local token-file path was covered with an opaque redaction before resizing; the unredacted source is not committed. A screenshot documents appearance, not implementation of every visible control: **Pause sync is currently display-only**. See [desktop behavior](../usage.md#tray-and-settings).

原有托盘截图保持不变：缩放前已用不透明遮盖移除本地令牌文件路径，未提交未脱敏原图。截图用于展示界面，不代表所有可见控件均已完整实现；**Pause sync 目前仅改变托盘显示**。

## Shipped emblems / 应用内状态图标

The homepages reference the existing [status SVGs](../../packaging/icons/hicolor/scalable/emblems). Their state mapping is defined by the [Nautilus extension](../../packaging/nautilus/twodrive_nautilus.py).

| Asset | Purpose |
| --- | --- |
| [Cloud](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-cloud.svg) | Online-only; also used in the homepage header. |
| [Syncing](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-syncing.svg) | Hydrating, writing, queued, or uploading. |
| [Synced](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-synced.svg) | Locally cached content. |
| [Pinned](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-pinned.svg) | Always Keep. |
| [Error](../../packaging/icons/hicolor/scalable/emblems/emblem-twodrive-error.svg) | Error or conflict. |

The [tray](../../scripts/twodrive-tray), [autostart entry](../../packaging/autostart/twodrive-tray.desktop), and [Settings launcher](../../packaging/applications/twodrive-settings.desktop) use the desktop theme's `folder-cloud` icon. The homepage cloud is a TwoDrive status emblem, not a separate application logo.
