# TwoDrive documentation · 文档

[English homepage](../README.md) · [中文首页](../README.zh-CN.md)

Start with installation, then read the everyday-use guide before trusting the mount with important files. 请先完成安装，再阅读使用与安全说明。

| Guide / 指南 | English | 简体中文 |
| --- | --- | --- |
| Install and sign in / 安装与登录 | [Get started](getting-started.md) | [开始使用](getting-started.zh-CN.md) |
| Files On-Demand, desktop controls, safety / 按需文件、桌面操作与安全 | [Everyday use](usage.md) | [日常使用](usage.zh-CN.md) |
| Release history / 版本历史 | [Changelog](../CHANGELOG.md) | [版本历史](../CHANGELOG.zh-CN.md) |

Shared references / 共用参考：[Configuration / 配置示例](../config.example.toml) · [Contributing / 开发与贡献（英文）](../CONTRIBUTING.md) · [Assets / 图片来源](assets/README.md)

## 故障记录与维护流程

从 2026-09-15 起，每次故障须记录根因、解决办法和验证边界：[故障记录](incidents.md) · [维护指导](../AGENTS.md)。

## Engineering notes

These are historical investigations/design records, not the current user manual. The project description includes intended features that may not be implemented. 以下为历史调查与设计，包含尚未实现的设想，不应当作当前功能清单。

| Topic | Record |
| --- | --- |
| Default editable desktop accounts and Windows mounts | [2026-10-29 design and validation](default-editable-2026-10-29.md) |
| Local workspace cleanup and recovery | [2026-10-07](workspace-cleanup-2026-10-07.md) |
| Development 0.1.5 release and workspace cleanup | [2026-10-15](workspace-cleanup-2026-10-15.md) |
| Nautilus responsiveness monitor | [2026-09-24](nautilus-hang-monitor-2026-09-24.md) |
| Archived original relay-lab reference | [Source and audit boundary](../TwoDrive-relay-lab/README.md) |
| Archive extraction I/O error | [2026-09-15](archive-extraction-2026-09-15.md) |
| Nautilus startup investigation | [2026-09-11](nautilus-startup-2026-09-11.md) |
| Folder-move ordering | [2026-09-11](folder-move-sync-2026-09-11.md) |
| Download/placeholder race | [2026-09-10](download-placeholder-race-2026-09-10.md) |
| PDF save reliability | [2026-09-09](pdf-save-reliability-2026-09-09.md) |
| Download cancellation | [2026-09-08](download-cancellation-2026-09-08.md) |
| Directory browsing | [2026-09-08](directory-browsing-2026-09-08.md) |
| Responsiveness and recovery | [2026-09-08](responsiveness-recovery-2026-09-08.md) |
| Background download investigation | [2026-09-08](background-download-investigation-2026-09-08.md) |
| Local-first synchronization | [2026-09-03](local-first-sync-2026-09-03.md) |
| Earlier reliability fixes | [2026-07-24](reliability-fix-2026-07-24.md) |
| Original description and goals | [Design document](twodrive_project_description.md) |

Existing investigation paths are retained so earlier release links continue to work. 原有路径不变，避免破坏历史发布说明链接。

- [Experimental device peer: implementation and audit](peer-implementation.md)
- [Peer 第一次 Windows/Linux 联调](peer-quickstart.zh-CN.md)

- [TwoDrive Development — Linux Flutter client / Linux 桌面客户端](linux-dav-desktop.md) · [2026-10-13 验证报告](linux-desktop-validation-2026-10-13.json)

- [Desktop provider boundaries and verification (2026-10-13)](provider-boundaries-2026-10-13.md)

- [Provider delivery verification (2026-10-13)](provider-validation-2026-10-13.json)

- [Third-party provider/auth/upload design specification / 第三方网盘与配套组件设计规范](provider-integration-spec.md)
- [iCloud Drive 本机预览接入与验证边界](icloud-drive.md)
- [百度网盘官方授权、只读浏览与挂载预览](baidu-netdisk.md)
- [Tray positioning / 托盘居中与 X11、Wayland 验证](tray-positioning-2026-10-14.md)
- [Storage connections / 存储连接与设备发现](storage-connections-dev.md)
- [Desktop sharing GUI / 服务端 GUI、连接文件与验证](desktop-sharing-validation.md)
- [Live OneDrive desktop verification / 真实 OneDrive 脱敏验证](onedrive-desktop-validation-2026-10-14.json)
- [Google Drive Development setup / 接入与验证](google-drive-dev.md)
- [PR #47 复审修正、操作变化与跨平台验证（2026-10-29）](review-followup-2026-10-29.md) · [脱敏验证摘要](review-followup-validation-2026-10-29.json)

- [Development 0.1.4 发布与验证](development-0.1.4.md)：队列/自动刷新、显式登录与切换、真实账号名称、原生 CI 与附件校验。
- [Development 0.1.6 发布与标准打包](development-0.1.6.md)：SQLite 元数据写入竞争修复、完整离线 Debian 源码/二进制构建与 MSIX。
- [Development 0.1.5 发布与安装验证](development-0.1.5.md)：共享 GUI、更新检查、MSIX 实际安装/运行/卸载、Deb 隔离 FUSE 验收。
- [Development 0.1.2 发布与验证](development-0.1.2.md)：云账号、在线连接管理、X11/Wayland 组件、原生 CI 与分发范围。
