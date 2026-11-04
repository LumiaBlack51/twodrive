# Google Drive in TwoDrive Development

[English homepage](../README.md) · [中文首页](../README.zh-CN.md) · [故障记录](incidents.md)

The `dev` branch adds Google Drive to the existing provider catalog. Ordinary Drive files use the same desktop browse, download, upload, folder creation, rename/move, trash, Linux FUSE and folder-backup services as the other dev providers. The stable OneDrive CLI/daemon and account state are separate.

`dev` 分支新增 Google Drive 提供者。普通文件接入既有桌面浏览、下载、上传、建目录、重命名/移动、回收站删除、Linux FUSE 和文件夹备份服务。Google Docs/Sheets/Slides 等在线文档显示为 `~google.url` 网页入口；它们不支持通过普通文件字节覆盖。共享盘及仅“与我共享”但未放入个人盘树的条目尚不属于此次挂载范围。

## Sign in / 登录

Create a **Desktop app** OAuth client, enable **Google Drive API**, and configure the OAuth consent screen. If the application is in Testing, add the account to its test users. This integration requests `https://www.googleapis.com/auth/drive` because browsing and changing existing files needs full Drive access. See Google's [installed app OAuth guide](https://developers.google.com/identity/protocols/oauth2/native-app) and [Drive scopes](https://developers.google.com/workspace/drive/api/guides/api-specific-auth). Testing refresh tokens can expire after seven days, as described in Google's [OAuth overview](https://developers.google.com/identity/protocols/oauth2/).

在 Development 的“添加连接”中选择 **Google Drive**，输入桌面应用凭据 JSON 的绝对路径与此账号专用私有登录目录，勾选允许修改。连接时打开系统浏览器，完成账号选择和授权后自动返回客户端。凭据目录必须在挂载和备份源之外，每个账号应使用不同目录。

A terminal entry point can prepare the same private login directory:

```bash
cargo build --manifest-path experiments/twodrive-dev/Cargo.toml --locked --features mount
experiments/twodrive-dev/target/debug/twodrive-dev \
  --state /absolute/private/google-account google login \
  --client-file /absolute/private/client_secret.json
experiments/twodrive-dev/target/debug/twodrive-dev \
  --state /absolute/private/google-account google status
```

Authorization uses a random numeric-loopback callback port, random state and S256 PKCE. Imported endpoint strings never control where secrets are sent. Access tokens refresh from the saved offline token. Unix state is 0700 and secret files are 0600; Windows uses the existing core DPAPI secret helper, but native Windows Google end-to-end verification remains pending. “退出登录” removes the local Google token/client copies and profile; it does not revoke grants in the Google account, which can be removed from Google's account permissions page. The imported credential JSON remains the user's file.

浏览器授权成功不代表 Drive API 已启用。连接遇到 403 时检查项目 API、账号权限与配额。网络默认使用系统路由与 HTTP(S) 环境代理；`direct` 禁用显式代理，本次不把它称为已验证的 TUN 物理出口绕过。

## Storage and recovery / 文件与恢复

The adapter uses Drive v2's explicit file ETag with `If-Match` for metadata mutations and final resumable upload requests. Google can accept resumable initialization with a stale ETag and reject the final PUT: an accepted upload session is not cloud completion. Downloads compare metadata before and after streaming, plus byte length, because a media response's HTTP ETag differs from the metadata ETag. Partial downloads remain temporary until verification succeeds.

Uploads use bounded 8 MiB chunks. A pre-generated remote ID, local content fingerprint and session are saved before upload; retries query the committed offset. A lost final response is reconciled against that same remote identity and SHA-256 rather than creating another object. A cancelled upload whose source bytes have changed is deliberately stopped for review; it cannot silently reuse the previous generation's session.

同名 Google 文件保留独立远端 ID，列表用 `~g~ID` 区分，操作解析列表里的独立路径。普通中文、百分号与波浪号名称保持可读；云端斜杠/控制字符使用显示别名。Google 允许其他客户端并发创建同名对象，本适配器不能声明按名称的跨客户端唯一约束；它拒绝已发现的同名目标并避免覆盖其他 ID。删除移入回收站，不永久删除、不清空回收站。

Mount and backup sessions bind the Google account identity as well as the profile revision. Index refresh uses a paginated full inventory and reconstructs the personal Drive tree in memory; it is snapshot polling rather than the native Google changes feed. The metadata limit is 100,000 objects / 16 MiB per page, matching the experiment's bounded provider approach. An indexing error does not publish a partial snapshot as deletions.

## Verification / 验证

Run the local checks:

```bash
cargo fmt --manifest-path experiments/twodrive-dev/Cargo.toml -- --check
cargo clippy --manifest-path experiments/twodrive-dev/Cargo.toml --all-targets --locked --features mount -- -D warnings
cargo test --manifest-path experiments/twodrive-dev/Cargo.toml --locked --features mount
cd apps/dav
flutter analyze --no-pub
flutter test --no-pub
```

[verify-google-drive.py](../scripts/verify-google-drive.py) runs actual IPC with an already authorized private account directory. It changes only a newly created disposable remote folder and moves that folder to trash afterwards. `--mount` adds actual Linux FUSE read/write cloud roundtrips. Its cleanup checks for mounted filesystems before removing local temporary directories. Example:

```bash
python3 scripts/verify-google-drive.py \
  --engine experiments/twodrive-dev/target/debug/twodrive-dav-engine \
  --peer experiments/twodrive-dev/target/debug/twodrive-dev \
  --client-file /absolute/private/client_secret.json \
  --google-state /absolute/private/google-account \
  --output /absolute/local/google-verification.json --mount
```

2026-10-14 已完成真实 Google 登录、离线令牌刷新、256 KiB 上传/下载 SHA-256 比对、正确版本覆盖、旧版本拒绝、中文文件名、建目录、移动、回收站删除、真实 Linux FUSE 读取及写入后的云端读回、文件夹备份创建/修改及本地删除不传播云端删除。自有临时对象均已移入回收站，测试挂载已卸载，见[脱敏摘要](google-drive-validation-2026-10-14.json)。独立服务端探针验证实际旧元数据 ETag 的最终上传 PUT 412 且原内容未改变。首轮 8 MiB+123 的分块上传成功，但下载被旧 ETag 逻辑拒绝，保留该失败结论；最后的默认无挂载独立运行已完成同大小分块上传/下载 SHA-256 一致、覆盖/移动/删除与备份，见[大文件独立摘要](google-drive-large-validation-2026-10-14.json)。中断后续传、下载中变更与重复名称覆盖由本地 HTTP 回归补充，未做真实网络中断注入。

全盘冷索引改为分页库存；此账号的独立只读探针在 142.61 秒读了 80 页仍未结束，因此 Google 的冷挂载预算为最多十分钟，仍可取消。浏览/单文件操作不需要等待整个索引。原 60 秒预算失败和下载 ETag 故障分别保留在 TD-20261014-02/03。

本轮不验证在线文档字节编辑、共享盘、Windows 或所有网络；没有重新测试真实 OneDrive。Google 模拟与真实验证结论分开保存，不能承诺所有同类故障永久消失。

Final local checks: 58 Rust tests in the standalone experiment, 13 Flutter tests, one Python verifier regression, Rust formatting and strict Clippy, Flutter analysis and Linux release build passed. These checks include local substitutes; the two linked reports identify the separately executed live Google checks. The stable workspace's full suite and native Windows Google execution were not rerun. Source remains on the local `dev` branch; no GitHub release was created for this work.

发布：[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2)（Linux/Windows、源码、规范、校验和与验证报告）；[最终构建与范围](development-0.1.2.md)。

## 2026-10-28 Windows 本机预览补充

用户已完成本机网页授权，当前原 state 中的四个账号均有匹配的桌面 session；Google 的离线令牌已保存，路径为 `D:\TwoDrive\GoogleDrive`。本次没有复制其他电脑的登录令牌。桌面应用 OAuth 凭据标识应用，不等于某台电脑已经登录；Windows 私密令牌由当前用户 DPAPI 保护。

Google Provider 现补齐 `storage_usage`，仅查询既有 Drive v2 `about` 的 `quotaType,quotaBytesTotal,quotaBytesUsedAggregate`。展示的是可能与 Gmail、Photos 共用的**账号空间**，不会把 Drive 单项用量当成账号总用量；不遍历私人文件估算，不为查询容量要求额外授权。不限量账号不捏造总容量，零值、超额使用及缺失字段保留真实含义；无效响应或请求失败返回错误，不冒充“不支持”。依据 [Google about 配额字段](https://developers.google.com/workspace/drive/api/reference/rest/v2/about)。

另显式开启 reqwest `system-proxy`：未设置 HTTP(S) 代理环境变量时，Windows 的静态用户系统代理也可用于 Google 令牌/API 请求。`direct` 仍明确禁用代理。不修改系统代理，未新增 PAC/WPAD 支持。无凭据探针在旧功能图下系统路由 15 秒超时、显式本机代理可达；修复后系统路由可达而 direct 仍超时。探针只有向公开 token 端点的 GET，不包含 OAuth 码、令牌或 Drive 文件请求；404 在这里表示网络可达，不等于登录成功。

四账号同时保持连接的既有实现不变：在线切换先返回 `retained=true`，不进入 Provider 登录；UI 对此显示“正在切换网盘”。启动通过已保存 session 恢复所有账号，遇到过期授权会单独报错，不静默混用其他账号。127.0.0.1 回调接收成功只表示收到授权码，后续令牌交换、身份探测成功后才是客户端登录成功。

验证：新增容量回归在旧代码因返回 None 失败；修复后覆盖账号总用量区别于 Drive 用量、只读查询、不扫描目录、不限量/零值/超额/缺失、无效数值/HTTP 错误和前后取消。126 次默认 Rust 执行、78 项 Flutter、严格检查及 Windows release 构建通过，5 项组件默认忽略不计入。实际包的独立 WinFsp 只读 5 组、密码保护编辑 7 组及新进程启动恢复 6 组回归通过，均未使用用户云盘。现已部署；真实 Google 容量数值、Windows Google 挂载读取及重新启动后的四账号真实恢复尚待验收，不把本机模拟当成这些结论。故障记录见 TD-20261028-22 至 27。

同批包随后包含 TD-20261028-26 的前端退出优化：退出时先隐藏窗口，后台继续正常卸载；后端拒绝退出会恢复原窗口，重复动作合并。新增 3 项 Workspace 回归后，最终完整 Flutter 测试为 81 项，分析和 Windows release 构建通过。用户已确认此前运行包的多账号切换正常；该确认不等同于容量或退出优化验收。

交付 `TwoDrive-0.1.7-google-20261028`：Rust 构建输入 `c8a2ce7fe06faa5e0d9a0f24f5ca57a3f36fd893`，最终 GUI 输入 `800cb9246113711511c62614a36856208f6c1dcf`；43 个运行和说明文件的大小及 SHA-256 已核对。代码已以 DavidXile 身份推送 [PR #47](https://github.com/LumiaBlack51/twodrive/pull/47)，保持 Ready for review。用户确认正常退出后，私密备份原 state 和导入凭据；新包只读升级检查确认配置字节一致。默认启动版本已切换，启动后本机核对一个新版 GUI/main engine、原四条完整配置、匹配 session 身份、首选项及四个 D 盘路径，全部未解锁。

只读窗口观察 Google 已连接并能显示根目录；另三个网盘的所属挂载进程存在。Google 自动挂载被提前存在的普通空目标目录拒绝，该目录已改名保留，原 `D:\TwoDrive\GoogleDrive` 路径释放；等待用户点击重试，没有鼠标控制或另行云盘探针。目录来源未确认，不将它归因于容量或代理改动。真实容量显示、Google 挂载读取和新退出体验仍待用户验收，不把本机进程检查当成文件可读或 hosted CI 已通过；未合并或发布正式 Release。

后续用户验收（2026-10-29 记录）：用户明确回复“容量和挂载正常，退出也改善了”，完成新包容量显示、挂载可用及退出体验的人工验收。保留以上部署时的观察记录；没有因此新增独立云盘探针、文件字节比对、真实写入或退出阶段计时。生产代码保持此前已通过 126 次 Rust 执行、81 项 Flutter 与实际包回归的版本；本轮仅提交验收记录，不重新运行相同构建或把用户反馈当作 hosted CI 结果。
