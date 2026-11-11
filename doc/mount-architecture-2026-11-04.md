# 挂载目录加载与Office关闭的架构对照

这里的加载指文件管理器进入挂载目录及显示文件列表，不指TwoDrive界面或上传吞吐量。

| 路径 | OneDrive原生挂载 | iCloud / 百度桥接挂载 |
| --- | --- | --- |
| 文件系统 | TwoDrive自有FUSE | Rclone VFS → 本机认证WebDAV桥 → Provider |
| 查找、属性、列目录 | 本机inode / SQLite；后台刷新云端元数据 | 桥索引未命中时仍需同步进入Provider |
| 目录更新 | 后台增量刷新并更新本机数据库 | 按目录刷新，保留当前挂载内的已验证访问目录 |
| 写入与关闭 | 本机事务和持久队列，网络工作放在后台 | 普通PUT/DELETE仍同步完成Provider校验；本批仅iCloud新建Office临时锁采用独立持久队列 |
| Provider | Graph对象ID、增量读取、条件写入 | 百度直接REST；iCloud部分操作需要独立认证组件 |

依据：[FUSE](../crates/twodrive-fs/src/filesystem/fuse.rs)、[本机子项读取](../crates/twodrive-fs/src/filesystem/mod.rs)、[原生后台刷新](../crates/twodrive-fs/src/mount.rs)、[上传队列](../crates/twodrive-fs/src/upload_queue.rs)、[桥目录索引](../experiments/twodrive-dev/src/readonly_mount.rs)、[桥写入](../experiments/twodrive-dev/src/mount_write.rs)、[临时锁队列](../experiments/twodrive-dev/src/editor_locks.rs)、[iCloud适配器](../experiments/twodrive-dev/src/rclone_drive.rs)。

OneDrive目录显示主要读取本机元数据。桥接路径中的重复属性查询和失效缓存容易放大网络等待；不能仅根据Provider不同就认定这些等待不可避免，也不能将所有历史故障归为同一原因。

## 本批改进

Linux百度和iCloud保留完整、验证成功的根及访问过的子目录，在原有64目录 / 100000条目预算内先响应本机显示，后台按目录合并刷新。嵌套属性查询共享浅层父列表；父目录确认删除或身份变化时失效旧子树，并保护在途结果。没有递归扫描整盘，也没有引入跨重启数据库。

iCloud新建且不超过16KiB的Office临时锁先持久化本机，再后台上传、清理。既有远端锁、版本变化和不确定应答保持恢复保护。正常待同步与失败恢复分别计数，未完成队列参与卸载保护；后台锁操作仅按已确认路径、身份和版本清理缓存，避免反复全局失效。百度普通文件及锁文件保留同步确认。

这是采用本机元数据与网络刷新分离的原则；正文读取、版本与权限校验仍需实时状态，不能直接继承OneDrive服务端条件写入能力。首次访问未知目录、LRU淘汰及重启后仍可能需要联网。

## 验证边界

本批干净检出的库、HTTP桥及显式合成FUSE回归均通过，旧目录策略有相同回归失败对照。实际文件管理器检查覆盖两家根与子目录在自然过期后的枚举；未做三家相同数据和网络下的全面冷访问横向基准，没有把单次系统调用时延当作完整窗口渲染时延。

本轮真实账号检查只读取目录元数据，不将合成写入测试称为真实云端写入验证。完整实现和结果见[目录索引说明](indexed-cloud-directories-2026-11-05.md)与[本次脱敏故障记录](incidents/TD-20261105-01.md)。
