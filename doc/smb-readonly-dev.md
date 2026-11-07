# SMB2/3 只读连接（本地审查候选，2026-11-07）

本轮只实现 Development 通用桌面的手动连接、浅层浏览、元数据及流式下载；没有发布、安装或替换现用程序。入口仍为 **连接 → 添加连接 → SMB2/3（只读）→ 保存 → 登录并使用**。先安装下述独立 Python 组件，再从同一环境启动候选桌面/engine。稳定版 OneDrive 程序没有新增 SMB。


**兼容范围很保守**：服务器必须声明并正确实现 reparse/no-follow 语义。默认 Samba 4.19.5 和 4.22.6 在本机均因未声明该能力而被拒绝；没有绕过开关。允许服务器透明解析 Unix 链接、甚至人为伪造能力声明的模式不支持，不能承诺其子根隔离。真实 Windows/NAS 仍需另行验收。

## 基线及并行工作

核查日为 2026-11-07（Asia/Shanghai），已实际 fetch `LumiaBlack51/twodrive`：

| 项目 | 已核实的状态 |
| --- | --- |
| `origin/dev` | `f2d2ee40a1341f49b4f2f5d7280ee7df202be524`，本轮基线，含四个通用适配器、Provider 合同与 #48 二进制增量上传 |
| `origin/main` | `a64ce65c8282398aafa79e66f8279b518e8d6e1c`（完整值以验证汇总为准），稳定版 0.2.23 后文档合并 |
| [#47](https://github.com/LumiaBlack51/twodrive/pull/47) | 开放，head `b8c066f010c9fb16f9777f1976c8c346792b8712`，iCloud/百度、多账号、手动写入及 Windows 挂载 |
| [#50](https://github.com/LumiaBlack51/twodrive/pull/50) | 开放，head `dfedcb6da51cdc4a7d20778c11c8f5cf8874538d`，#47 的 Linux 适配及后续修改 |
| #45 / #46 | 仍开放，分别为 Windows Shell 所有权、CLI 验证工具；本轮未整合 |
| SMB / SFTP / FTP(S) / NFS | main、dev、上述开放 PR 的注册、源码及本地 worktree 没有发现已接入的这些适配器；这是已检查范围内的结论，不代表不存在未公开工作 |

主工作区为分离 HEAD，已有 `AGENTS.md`、故障记录修改和脱敏报告；其他已有 worktree 的两份故障记录修改也保留。本轮新建 `codex/smb-readonly-20261107`，没有切换已有分支、覆盖文件、合并 PR 或操作现用 daemon。构建使用独立 target、nice 15、ionice idle 和两路并行。

选择最新 dev 的原因是其通用 Provider/schema 已完整存在，而 #47/#50 尚未整合、接口继续变化。新增方法均有默认实现；未引入这两个 PR 的手动编辑/多账号架构。将来整合时必须保留 SMB 的后端拒写、`supports_mount=false`、空版本及显式重连约束；#47/#50 的“默认可编辑”不能给 SMB 增加写能力。需要协调的重叠文件为 `desktop_providers.rs`、`provider.rs`、`desktop.rs`、通用表单与测试、故障记录。未宣称与这些 head 自动合并已验证。

### main 修复的机制核对

这不是按祖先链推断：同时阅读两分支对应函数和 main 近期故障记录，发现以下差异，未展开迁移。

| 机制 | dev 的实际等效范围 / 缺口 |
| --- | --- |
| 上传认领、代次和未确认结果 | dev 有 `NativeCacheJournal` 的 generation、digest、lease、uncertain-upload 和 `Backend::upload_snapshot`；并非完全缺少保护。但一般 Linux FUSE 的 `queue_remote_delete` 仍无当前云端 ID 所有权检查，不能视为 main 58c4619 / ea07468 的全部等效实现 |
| 缺失云端对象冲突恢复 | dev `preserve_conflict_copy` 仍要求原对象存在，尚无 main 的 missing-target 完成事务；不能把另一套 native journal 当作该路径已经修复 |
| 恢复后命名空间 | dev inode/读取路径尚无 main 758f0c8 对退休身份的对应处理；仅为源码核对，未重放真实现场 |
| 普通 FUSE 缓存版本、回收与打开文件 | dev 的 `has_existing_cache` 主要检查状态和存在性；metadata upsert 未具有 main 的版本/长度变更退休事务，读句柄仍按路径重开。native journal 的租约/散列保护适用于另一条路径，不等效于全部普通 FUSE 修复 |
| 未修改的可写句柄 | dev 仍在 writable open 时标记 Writing，未有 main 87e8a11 的 `changed`/首次实际写入认领机制 |
| 热项目/known-folder 限流 | main 有后续预算与探索限制；SMB 不调用这些上传服务，本轮未迁移或验证等效性 |

因此 SMB 仅使用通用桌面临时下载发布，不启用任何持久同步缓存、worker 恢复、自动备份或挂载。原有其他 Provider 的这些缺口仍须另行审查；本轮不称为已修复。摘要见 TD-20261107-01。

## 客户端选择

核查上游 README、许可证、发布记录和已安装 1.17.0 源码后选择 `smbprotocol`。下表区分上游声明与本项目实现；未对每个候选进行互通测试。

| 方案 | 维护与许可、平台 | 安全、取消、文件语义及取舍 |
| --- | --- | --- |
| [smbprotocol](https://github.com/jborean93/smbprotocol)、[发布记录](https://github.com/jborean93/smbprotocol/releases/tag/v1.17.0) | 有持续发布/测试；MIT；Python 3.10+，Linux/Windows | 上游提供 SMB2/3、签名、加密、NTLM/Kerberos。低层 Open 可指定只读权限、share-deny-write/delete、reparse 标志；不使用高层全局池/自动 DFS。同步调用不能直接保证及时取消，因此放入每请求独立受控进程 |
| [libsmb2](https://github.com/sahlberg/libsmb2) | 维护中的 C 用户态客户端；库 LGPL-2.1-or-later；Linux/Windows 构建 | 异步 API、签名/加密、超时及低层文件操作适合长期方案；需要 C ABI/各平台组件及再分发安排。没有因默认选项可能不要求签名就放弃安全策略；若选它仍必须显式要求保护 |
| [Pavão/libsmbclient](https://github.com/veeso/pavao) | wrapper MIT；底层 Samba 另有许可；Linux/Unix 依赖较重，Windows 要另一条实现 | README 声明所有 native 调用全局串行，阻塞会影响其他连接；默认方言跟随 smb.conf。可显式限制 SMB2/3，但本轮避免平台分流、全局阻塞及隐藏 OS 会话池 |
| [smb-rs](https://github.com/afiffon/smb-rs) | 较新的纯 Rust 实现、MIT，活跃开发；上游声明 Linux/Windows | 上游声明全 SMB2/3、签名/加密、NTLM/Kerberos 与 async。长期可重新评估；本轮优先已有成熟 Python 实现的低层文件控制和进程隔离，未将它宣称为不安全或不支持取消 |

SMB 协议、认证和签名/加密全部由库处理；TwoDrive 只做适配、权限限制、凭据和流量边界。无自实现协议栈、内核 CIFS 挂载、管理员权限、WinFsp 或 Samba 客户端系统服务依赖。替代方案为今后评估 libsmb2 / smb-rs；不把用户自己挂载的路径当原生 SMB 适配器。

## 安装组件及配置

在独立、由当前用户控制的环境安装依赖（不需 sudo）：

```bash
python3 -m venv /absolute/private/smb-venv
/absolute/private/smb-venv/bin/python -m pip install \
  -r experiments/twodrive-dev/components/requirements-smb.txt
export TWODRIVE_SMB_PYTHON=/absolute/private/smb-venv/bin/python
# 在这一环境启动候选 Development 桌面；不要运行现用安装器。
```

Windows 同样需要 Python 3.10+ 的独立环境，设置 `TWODRIVE_SMB_PYTHON` 为该环境的 `Scripts/python.exe`。Windows 本轮没有构建或运行，不能据此宣称已兼容；凭据 ACL、进程回收及原生互通仍需 Windows 验收。本轮只修改 CI 配置，未推送或执行 hosted CI。

字段为服务器（不带 `smb://`、用户名或路径）、端口（默认 445）、共享名、共享内绝对根目录（默认 `/`）、用户名、域/工作组和密码。例如服务器 `127.0.0.1`、共享 `synthetic`、根 `/Documents`。这是格式示例，不会主动连接或扫描网络。

默认要求 SMB3 加密。高级选项 **仅签名，允许 SMB2** 是明确选择不加密内容；仍要求签名并验证协商（加密消息由加密完整性覆盖），不因连接失败自动改策略。不支持 SMB1、匿名/guest、OS 凭据自动回退或 Kerberos。本轮 Linux 使用 Python NTLM，固定 `LM_COMPAT_LEVEL=5` 拒绝 LM/NTLMv1 兼容回退；Windows SSPI 策略及真实 Active Directory/域认证未验收；实际只验证合成本地账号和空域。签名/加密不等同 SSH 主机密钥校验；server GUID 仅是活跃连接中防止服务端意外更换的附加检查，不宣传为密码学服务器身份钉扎。

使用 OS TCP 路由和 DNS，不使用 HTTP/SOCKS 代理、不宣称 VPN 绕过。通用界面“系统路由”选项在 SMB 仅代表系统网络路由。

密码是 transient secret：保存时从 Profile 移入现有 `State` 私有 JSON 机制，Unix 目录 0700、秘密文件 0600。`profiles.json` 只有私有引用，公开 IPC 仅给 `password_saved`；worker recipe、命令行、诊断不含密码。该机制为当前用户的私有文件保护，并非加密保险库。域/账号、server/port、share/root、schema 或 profile identity 变化不得复用密码；未变的身份可空密码编辑，并写入新 revision 的私有秘密文件。sign-out 删除当前 revision 的秘密文件；历史 revision 文件不自动跨配置重放，也不自动扫描清理。任何额外本机清理须单独核查归属。

每次显式连接生成新 binding 和内存观察 ID，旧 ID/worker session 拒绝恢复；密码相同也不复用另一条 profile 的状态。无持久内容缓存，观察表最多 20,000 项；超过预算后旧 ID 失效，需刷新。

## 路径、读取和限制

- 只使用选定共享的一棵子目录。拒绝 `..`、空组件、反斜杠、ADS 冒号、控制字符、通配符、危险 Win32 名称和末尾点/空格；合法 Unicode/内部空格及大小写原样交给服务器。大小写敏感性遵从服务端，不在客户端转小写或 Unicode 归一化。Samba 可能为 Unix 非法 Windows 名称返回安全短名；不会把包含分隔符的服务端名称拼成新路径。
- 不支持 symlink、junction、其他 reparse point 或 DFS。使用低层协议且不跟随 referrals；DFS share 拒绝。组件专属 Connection 在每次 receive 显式强制 resolve_symlinks=false，避免库低层默认的同共享链接自动重试。根内容访问前检查 `FILE_SUPPORTS_REPARSE_POINTS`，缺失即失败；目录项也要实际 no-follow 打开、检查后才公开，不能只相信列表行。逐层 `FILE_OPEN_REPARSE_POINT`、检查 reparse 属性，并持续保持所有祖先的只读、deny-write/delete 句柄，防止检查后替换目录；不满足这些打开语义的服务器会失败。服务端可能隐藏 Unix symlink；客户端不据此宣称已测试 Windows junction。选中根目录需具有相应读取/遍历权限。
- 每请求独立进程和 SMB session，用完关闭；显式断开销毁 backend。连接内固定 server GUID，变化要求重新连接。一次请求至多 10,000 个目录项、64 KiB 内容分块、128 KiB IPC 帧、有界两帧队列，30 秒无帧期限；取消轮询 50ms，终止并回收所属组件。Linux 父进程死亡信号避免遗留组件；Windows 父进程异常退出回收未验证。
- `remote_id` 是本次连接内的随机观察令牌；file index、change time 和长度仅作读取前后的附加变化检测，`etag` 始终为空。不会将其当跨重连可靠的强版本。读取期间 deny-write/delete 防止合规 SMB 写者改动；没有服务端不可变快照或远端内容散列承诺，不能保证绕过 SMB 锁、隐匿元数据变化的服务端本地修改。
- 内容流入现有桌面 `NamedTempFile`；完整长度、流的 SHA-256、结束状态/子进程成功、读句柄后验检查通过后才 flush/sync 和 `persist_noclobber`。此 SHA-256 核对组件到 Rust 的内容，不冒称 NAS 提供了内容哈希。取消、断线、截断、关闭失败不发布；已有本地文件保留。禁止一次性 `download(Vec)`，要求 `download_to`。
- 所有 `CloudBackend` 和 `Provider` 写入入口直接拒绝：upload、条件/文件上传、mkdir、rename、delete；能力全部 false。`supports_mount=false` 加通用引擎准入，递归库存/delta/worker restore 也拒绝。没有写网络请求或“先改后报不支持”。只读表单不显示允许写入开关，内部私有引用字段不显示；没有独立 SMB 页面或 UI 协议判断。

## 验证与复现

完整命令、版本和结果见 [本轮验证记录](smb-readonly-validation-2026-11-07.json)。所有数据均为临时合成数据；Samba 4.19.5 与 4.22.6 仅私有解包、普通用户回环高端口运行，未安装系统服务、改防火墙/系统挂载。测试不读取用户服务器或账号。

最终正向协议测试使用 **Samba 4.22.6 + `follow symlinks=no` + 显式测试能力声明 `share:fake_fscaps=128`**。后者仅用于合成 fixture 声明受控策略的能力，不是产品选项或生产绕过建议。Samba 即使人为声明该位，若仍透明跟随 Unix 链接，也不是本适配器支持的服务器；实际负面调查已确认这种配置不能保证子根范围。默认 Samba 的安全拒绝另有真实测试，不能把正向 fixture 当作默认 Samba 开箱兼容。

以下脚本自行产生/清理 fixture；需在 SMB venv 中执行，路径指向候选构建及私有 smbd：

```bash
python scripts/test_smb_component.py
# 受控的较新测试服务器，明确记录非默认能力声明；不用于用户 NAS 配置。
python scripts/test-smb-readonly.py --smbd /absolute/private/smbd22 \
  --engine /absolute/build/debug/twodrive-dav-engine \
  --peer /absolute/build/debug/twodrive-dev --advertise-test-reparse-capability
# 默认服务器：验证 fail-closed，不当作正向传输通过。
python scripts/test-smb-readonly.py --smbd /absolute/private/usr/sbin/smbd \
  --engine /absolute/build/debug/twodrive-dav-engine \
  --expect-reparse-rejection --follow-test-symlinks
```

正向 13 组覆盖真实 SMB 3.1.1 加密 / SMB 2.1 强制签名、元数据/浅层目录、大小写别名、空/Unicode/128 MiB 文件内容、权限/密码拒绝、取消/断线/服务端本地截断无发布、已有文件保全、重连 binding、share/root 隔离及秘密不继承。在支持的服务器策略下，目录和最终文件链接均不可见或返回错误，未公开其目标；不将“服务端隐藏链接”当作客户端独立发现链接的证据。组件七项单元测试覆盖能力缺失拒绝、低层禁止跟随、危险名称、reparse/祖先句柄、时间表示及日志脱敏；Rust 合同覆盖全部后端写入口。

真实 Windows 共享、实体 NAS、Active Directory/域认证、Kerberos、DFS/junction 互通、IPv6、独立网络、SMB 2.0.2/3.0/3.0.2 均未验证；mock/组件单元测试不代替这些互通结论。普通 FUSE 默认忽略测试、完整 GUI 桌面交互和 hosted CI 未执行。

## English

This local review candidate adds manual SMB2/3 read-only connections to the existing Development schema and Provider UI. Install the pinned Python component in a private environment and set `TWODRIVE_SMB_PYTHON`. SMB3 encryption is required by default; signed-only SMB2/3 needs explicit selection. There is no SMB1/guest fallback, writing, backup, mount, DFS or link following. Passwords use private State files rather than ordinary profile options or process arguments. Per-connect observations have no strong reconnect version, and partial downloads never publish. Servers must advertise and correctly enforce reparse/no-follow semantics. Stock Samba 4.19/4.22 were rejected; positive protocol tests used an explicitly configured synthetic Samba 4.22 fixture with server-transparent links disabled. This is not stock-Samba compatibility or a production capability-override recommendation. Windows and physical NAS remain unverified. No existing runtime has been replaced.


## dev 提交与审查（2026-11-07）

用户已授权提交至 dev，并要求清理本轮构建缓存/一次性脚本及等待 CI、Dot 意见。工作分支为 `codex/smb-readonly-20261107`，采用目标为 dev 的 PR 与必需检查流程；源码已提交为 [`7e600c6`](https://github.com/LumiaBlack51/twodrive/commit/7e600c6b7f1fc86492994c60f623cd476c77569c)，已推送并创建 [PR #59](https://github.com/LumiaBlack51/twodrive/pull/59)，目标 dev。最新 head 的 CI 与 Dot 审查仍待完成。用户要求的推送后清理已完成：本轮 target 缓存、候选调试程序、私有测试 venv/Samba 解包、一次性探针、日志与重复 patch 已移除；永久回归脚本和源码保留，其他工作目录与现用程序未动。此前本地验证结果不冒充 hosted CI。该授权不涉及 Release、现用程序替换或 SMB 写入阶段。
