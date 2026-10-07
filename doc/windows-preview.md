# Windows 0.2.10 preview

This is an unsigned engineering preview, not an accepted Windows sync release.
Full and Lite contain byte-identical Rust engines and native tray hosts.
Full additionally contains Flutter Fluent UI. Lite contains no Flutter/Dart runtime.

Run Start.ps1 with an explicit absolute disposable -StateDirectory. Add -Mock only
for synthetic tests. Omit -Mock for the signed-out shell, then sign in for read-only OneDrive browsing/cache:
there are no sample accounts/files in that mode. Modes cannot share a state root.
Do not point at an existing Linux data directory, real sync root or account.

Full: left tray click opens the activity window; right click opens the Win32 menu.
Lite: either click opens the simple native menu. Closing the window/tray leaves
the engine running. A second tray or engine for the same state root is rejected.
To switch editions, close the first tray, start the other package against the same
state directory and same mode. The running engine is reused. Do not delete state.

The mock remote is in memory, not persistent cloud storage. Mock uploaded remote
content does not survive engine restart; retained local cache must not be mistaken
for a persistent cloud copy. Use new disposable roots for new test scenarios.

Supported: framed IPC v1, live engine state, scheduler pause/resume persisted by
the engine, explicit mock downloads/uploads, cache release with existing dirty/
pinned/open-file protections, extension-only file glyphs, recent confirmations,
disconnected state. A pause drains already-started work and starts no new work.
The management list is bounded to 200 rows; transfer history to 32 entries.

Not implemented/enabled: CFAPI registration and callbacks, simultaneous multi-account
management, peer GUI integration, cloud writes, native
sync-root file operations, pin controls, bandwidth limits, persistent operation
journal, autostart, installer/updater/signing, aggregate batch throughput/ETA.
No FUSE or WSL process is used by the Windows binaries.
Flutter and native tray UI require interactive Windows. The Full UI may require
the Microsoft Visual C++ runtime used by the installed Flutter toolchain.

CLI: twodrive-engine ipc --state ABSOLUTE_DIRECTORY reads a UTF-8 JSON request
on stdin and prints exactly one structured JSON response. Example:

    {"version":1,"id":"inspect-1","command":{"type":"snapshot"}}

There is no human-text parsing by the UI. The bridge talks to the owner-only
local named pipe. IPC has a 1 MiB frame bound, timeouts, a 16-connection limit,
version/request ID checks and a 256-entry in-memory mutation replay cache.
Request IDs are not a durable exactly-once journal across engine restarts.

Never use this preview as the sole copy of any file. No installer or service
registration is performed by these scripts.

## 真实 OneDrive 只读浏览

Full 的文件页自动读取云端根目录，支持子目录、返回、面包屑、刷新和逐页加载。
界面明确显示“云端浏览，尚未启用本地同步”；真实模式不提供上传、删除、固定或双向同步；显式只读内容下载见下文缓存里程碑。
共享快捷方式、package/未知条目会标为暂不支持，不作为空文件夹处理。

Lite 可使用同一引擎 IPC 查询（先按 Start.ps1 启动引擎）：

```powershell
.\twodrive-engine.exe list --state E:\software\twodrive\windows-preview-state
.\twodrive-engine.exe list --state E:\software\twodrive\windows-preview-state --drive DRIVE_ID --item ITEM_ID
.\twodrive-engine.exe list --state E:\software\twodrive\windows-preview-state --next QUERY_ID
```

输出是结构化 JSON：account_id/drive_id/item_id 标识目录，items 为本页条目，
page_number 为页码，has_more 表示尚未完整加载。继续分页使用上一响应的 query_id，
Graph nextLink 留在后台，客户端不能传任意网络 URL。名称、类型、大小、修改时间来自 Graph。
空 items 只有 status=complete 才能确认该查询已完成；条目数不是全盘文件数。
一页元数据最多 256 KiB，超限明确失败，不悄悄截断。内存目录快照五分钟后标记过期，
继续分页需刷新。新目录查询取代该引擎当前浏览会话，其他客户端需刷新恢复；
旧查询、取消和注销的迟到结果不会写回新会话。

401 最多刷新一次；并发刷新互斥，凭据继续由 DPAPI/TokenStore 保存。
429 遵守 Retry-After，超过单次等待预算时显示限流并保留后台等待时限；重试次数有界。
网络不可用、超时、权限不足、条目不存在各有错误标识。没有新增权限范围。
退出登录会删除此状态目录的本地凭据（不会撤销 Microsoft 端授权或删除云端文件），
完成后可重新登录。真实账户注销未在交付实测中执行；合成凭据覆盖该机制。

API 依据：[Graph v1.0 children](https://learn.microsoft.com/en-us/graph/api/driveitem-list-children?view=graph-rest-1.0)、
[Graph throttling](https://learn.microsoft.com/zh-cn/graph/throttling)。
# Persistent OneDrive index and cache milestone (2026-09-17)

Full's Files page shows the persistent metadata index. Refresh performs Graph delta;
the previous committed list remains available while refreshing. The directory browser
from the previous preview remains available through the directory-browse button.
Download/resume, cancel, byte progress/speed, open and reveal operate through the Rust
engine. `cached` means a verified local copy, not synchronization. Uploads and CFAPI
are not enabled.

Lite commands against the same running engine and state directory:

```powershell
twodrive-engine.exe refresh --state ABSOLUTE_STATE_DIRECTORY
twodrive-engine.exe index --state ABSOLUTE_STATE_DIRECTORY 0
twodrive-engine.exe download --state ABSOLUTE_STATE_DIRECTORY ITEM_ID
twodrive-engine.exe tasks --state ABSOLUTE_STATE_DIRECTORY
twodrive-engine.exe cancel-download --state ABSOLUTE_STATE_DIRECTORY ITEM_ID
```

Refresh/download commands acknowledge worker submission. Poll `tasks` for completion
and `index` for persisted cache state; an acknowledged command is not a completed download.
Index pages contain up to 100 items, with offset/count. Interrupted downloads require an
explicit download/resume command after restart; partial bytes are never a valid cache.
One download runs at a time. Old version cache files are retained but not presented as
valid for a changed item; automatic cache garbage collection is not implemented.
CLI output contains private metadata: do not attach raw output to issue reports or Git.

## Combined dev packages (2026-10-07)

Build with `-IncludeDevTools` to bundle `twodrive-peer.exe` and `twodrive-dev.exe` in Full/Lite alongside the unchanged GUI engine. Follow [the combined guide](dev-integration.md); each component needs its own state root and trust/pairing flow. The UI still controls the OneDrive engine. WebDAV/QUIC and cloud peer control are separate CLI tools in this integration.
