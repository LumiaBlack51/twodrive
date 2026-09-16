# Windows Full 前端设计调整

参考：工作区根目录 `PROTOTYPE.html` 与 `preview-tray.png`，均已读取/查看。

## 已实现

- 按原型采用浅灰蓝背景、白色卡片、细边框和蓝色强调色。
- 管理中心使用 183px 侧栏、图标导航、选中竖条、账户区域和页面说明。
- 概览显示状态卡片、待同步项目/文件/已缓存大小三项摘要，以及近期活动。
- 托盘保留 392×620 尺寸，采用圆角边框、账户头部、状态与传输进度、活动列表和底部快捷入口。
- 文件图标使用扩展名配色和折角轮廓；大小使用 B/KiB/MiB。
- 保留后台能力检查、暂停确认、断连禁用以及现有文件下载/释放操作。

## 验证

完整 Flutter 6 项测试通过，覆盖后台协议/确认、托盘与管理中心、850×650 最小窗口导航和断连禁用。`flutter analyze --no-pub` 无问题；`flutter build windows --release` 成功。

`flutter-tray-redesign.png` 和 `flutter-management-redesign.png` 是实际 Flutter widget 渲染图，来自明确注入的测试数据，并非真实 OneDrive 或原生通知区操作截图。用 `TWODRIVE_CAPTURE_UI=1 flutter test` 可在 Windows 开发环境重新生成。

原型中的速率、账户邮件、完整文件菜单等，在后台尚无相应能力时不填入虚构值。未实现功能继续禁用。本次未部署或覆盖已有预览压缩包。
