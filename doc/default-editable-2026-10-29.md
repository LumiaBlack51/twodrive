# 默认可编辑连接与挂载（2026-10-29）

按用户新的设计要求，桌面客户端不再提供本机只读模式、权限切换或写入密码。OneDrive、iCloud Drive、百度网盘和 Google Drive 连接后，客户端及 Windows 挂载直接提供适配器与远端授权允许的编辑操作。多账号登录、自动恢复和独立挂载路径沿用原配置。

## 升级和数据边界

- 新连接默认可编辑。旧云盘/WebDAV 配置中的 `writable=false` 在组合适配层按默认可编辑解释；启动及恢复不修改 profile ID、revision、凭据、账号 binding 或挂载路径。保存配置时使用新策略。
- 已存在的本机密码及许可文件不再读取或使用，升级不删除这些历史文件，不要求重新授权网盘。旧 worker recipe 继续按原 revision 和账号 binding 核验。
- 成对设备分享的授权由分享方决定，导入的授权及服务器实际限制继续生效。WebDAV 服务器只授予读取时，客户端不能绕过服务器限制。移除本机模式不等于提升网盘授权。
- 仍保留 create-only、版本检查、账号及存储根目录隔离、本机恢复副本。iCloud/百度未提供与 OneDrive 同等的原子覆盖前置条件；它们的手动编辑与 Windows 挂载仍使用既有版本核验、恢复副本及提交核验，不宣称获得原子 CAS 或自动备份能力。
- 未上传或冲突内容仍阻止普通卸载/退出，异常退出后保留原编辑内容和版本声明。重启后同一账号可继续提交不冲突的未上传内容，不再等待本机密码。换账号或存储根目录不能重放旧缓存。
- 本轮四网盘可编辑挂载交付范围是 Windows/WinFsp。Linux FUSE 仍仅对具备既有条件上传合同的适配器开放可编辑挂载，不把弱条件写入伪装成 Linux 自动同步能力。

## 删除和恢复

现有云盘删除调用沿用网盘的正常删除/移入回收站路径，不新增清空回收站或永久删除操作：

| 网盘 | 当前调用 | 恢复位置 |
| --- | --- | --- |
| OneDrive | Microsoft Graph `DELETE driveItem` | OneDrive 回收站 |
| Google Drive | Drive v2 `files.trash`，附原对象 ETag | Google Drive 垃圾箱 |
| iCloud Drive | 固定 Rclone 1.75.2 `Remove` 调用 `MoveItemToTrashByID` | iCloud 最近删除 |
| 百度网盘 | 官方 `filemanager` 的 `opera=delete` | 百度网盘回收站，受账号及平台规则限制 |

回收站负责误删后的恢复；它不覆盖所有覆盖写入和并发冲突场景，因此原有恢复副本与版本检查继续保留。回收站保留期限、共享文件权限和特殊空间由网盘决定。百度官方说明明确指出隐藏空间删除不进入回收站；本项目不新增隐藏空间接口或永久删除入口。WebDAV/NAS 与设备分享是否有回收站由对方服务器决定。

依据：[Microsoft Graph 删除语义](https://learn.microsoft.com/en-us/graph/api/driveitem-delete?view=graph-rest-1.0)、[Google Drive v2 trash](https://developers.google.com/workspace/drive/api/reference/rest/v2/files/trash)、[Rclone 1.75.2 iCloud 实现](https://github.com/rclone/rclone/blob/v1.75.2/backend/iclouddrive/iclouddrive.go)、[百度官方 SDK](https://github.com/baidu-netdisk/baidu-drive-sdk-go/blob/main/baidudriver/api/file_manager.go)、[百度官方恢复说明](https://yun.baidu.com/disk/help)。百度 API 文档未在本次提供独立回收站回执；未通过用户账号执行删除/恢复验收，不将源码调用与普通回收站规则等同于四家真实恢复测试。

## 验证记录

本轮源码验证：

- Windows Rust 143 项执行通过（另 7 项默认忽略）；Linux Rust 135 项通过（另 4 项忽略）。严格 Windows all-target Clippy、Rustfmt、78 项 Flutter 测试和静态分析通过；三个 Windows release 程序及新版 GUI 已构建。
- 新的“直接可编辑”原生回归对旧包运行，在默认权限断言处失败；对新包七组全部通过：默认写入、随机偏移修改/新建/改名/删除及客户端上传、第二账号保留、双账号重启可编辑、冲突拒绝覆盖与保留编辑、换账号不重放旧缓存、正常卸载。
- 新包另通过六组实际 WinFsp 双账号启动恢复检查、六组远端只授予读取的服务拒绝写入/挂载读取检查。WebDAV 能力描述协议支持；服务器在实际请求时拒绝无权写入，测试未将本地 VFS 接受暂存当成云端提交成功。
- 实际 Linux FUSE 四组通过：未提交内容阻止普通退出、旧只读 profile 不改 revision 即恢复可编辑、异常重启后恢复原待上传内容并接受新写入、正常清理。Linux 使用 debug 程序，未宣称 Linux release 构建。
- 一次性 WebDAV 桌面全流程、存储连接、QUIC 分享流程通过；三项字体/大字体视觉回归通过，并检查连接页及添加表单图片。

这些检查只使用一次性本地服务与合成数据，没有访问用户真实网盘或执行真实云端写入、删除、恢复。候选验证时尚未替换运行中的客户端；后续部署见下节。真实四云盘写入及回收站恢复尚待用户验收。分发包另附逐文件 SHA-256 清单及交付状态。详见 [脱敏验证摘要](default-editable-validation-2026-10-29.json)。

历史写入锁设计见 [2026-10-28 记录](windows-write-lock.md)，其密码与重启上锁策略已被本文替代。


## 本机部署（2026-10-29）

用户确认“可以，开始更新”后，先核对客户端与挂载进程均已退出。48 个分发文件逐一通过 SHA-256 校验；原完整私有状态及外部账号凭据已备份，备份目录应用当前用户/System 的私有 ACL，原状态文件哈希核对通过。新包仅执行本机 profiles IPC 预检，确认原配置文件逐字节保持、四账号均默认可编辑及可恢复登录，未进行额外云端请求。

默认启动器已切换至 `TwoDrive-0.1.7-default-editable-20261029`，继续使用原 `alpha3-gui/state`。本机元数据确认只有一个新版 GUI 及其主引擎；四条完整配置、profile/revision、保存会话 binding、选中账号及原 D 盘路径均保持。四组所属挂载 worker/原生组件自动恢复，同一账号 binding 对应的私有 VFS 状态持续更新，四者均为 pending=0、error=false。这验证可写挂载分支已运行，不等于真实云端写入/删除/回收站恢复验收。

本次没有操作鼠标、手动启动独立云端测试程序或创建/删除用户云盘文件。程序及 GUI 来源为已核实提交 `2a3245aa93fd2b6ed4cafffebc29507a2b22af7f`；本段只记录部署，无代码重建或再次重启。无需重装 WinFsp。
