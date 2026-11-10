# TwoDrive dev：WebDAV 与加密设备共享

[English](README.md) · [架构设计](../../doc/dev-webdav-peer-design.md) · [故障记录](../../doc/incidents.md)

这是 `dev` 分支上的独立实验入口 `twodrive-dev`。正在使用的稳定 TwoDrive、配置、数据库和原 main 工作目录保持原样。dev 现已整合 Windows 预览及 OneDrive peer-control 源码，见[组合指南](../../doc/dev-integration.md)。这里使用已有的文件系统引擎，但有独立 Cargo workspace、锁文件、设备状态和挂载缓存。

已实现：连接现有 HTTPS WebDAV；将显式目录共享；一次邀请完成设备配对；QUIC/TLS 加密、可靠 UDP 直连、NAT 打洞和加密中继回退；设备撤销；本机 WebDAV 网关；可选 Linux FUSE 挂载。没有 VPN、虚拟网卡或路由配置。它不是 Syncthing 式的双向目录镜像。最新 `dev` 的 [Development 桌面](../../doc/storage-connections-dev.md)可在“共享”中选目录、启停服务及导出连接文件，另一台电脑在“添加连接”中导入并连接；命令行服务保留。该功能晚于已发布的 0.1.4。[两机连接流程、自动更新检查与发行准备](../../doc/distribution-preparation.md)。

## 构建

Rust 1.91 或更高版本。Linux 挂载需要 `/dev/fuse` 和 `fusermount3`。

```bash
cd experiments/twodrive-dev
cargo build --release --locked --features mount
```

产物为 `target/release/twodrive-dev`。Windows 和只需要 WebDAV/设备共享时去掉 `--features mount`，Windows 产物为 `twodrive-dev.exe`。GitHub Actions 构建原生 Linux/Windows 工作流资产；资产不等于已发布的稳定安装包。两台机器都使用这个 dev 二进制即可，不需要 OneDrive 登录。

## 两台机器快速配对

服务器先选一个专门共享的目录，默认只读：

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/server serve \
  --root /path/to/shared --invite-file ./invite.json
```

把邀请文件通过可信渠道传给另一台电脑（Linux 文件权限须为 0600），十分钟内运行：

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/client connect \
  --ticket-file ./invite.json
```

连接输出服务器公钥指纹、当前传输类型、本机地址 `http://127.0.0.1:4918/` 和私有凭据 JSON 路径。通过凭据中的用户名/密码连接任意 WebDAV 客户端；本机 HTTP 请求经过 QUIC 转发，跨机器传输始终加密。服务端不开放公网 HTTP。

后续连接同一个已配对服务器，省略 `--ticket-file`。为第二个设备生成新邀请（会取消尚未消费的旧邀请）：

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/server invite --out ./invite2.json
twodrive-dev --state ~/.local/share/twodrive-dev/server peers
twodrive-dev --state ~/.local/share/twodrive-dev/server revoke DEVICE_ID
```

撤销后现有连接的新请求也被拒绝；正在执行的请求可以结束，立即中止需停止服务器。邀请只可兑换一次，持有邀请的人可抢先兑换，文件应保密并在配对后删除。授权绑定共享目录和读写权限；改变它们请用新 `--state`，避免把已有授权悄悄用于新目录。

要写入，服务器增加 `--write`。要直接在 Linux 文件管理器中使用，可在客户端连接时增加 `--mount /path/to/empty-mount`；读写挂载还需客户端 `--write`。挂载数据位于客户端私有状态的 `mount/`，不使用 `~/TwoDrive/OneDrive` 或原数据库。

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/client connect \
  --mount /path/to/empty-mount --write
fusermount3 -u /path/to/empty-mount
```

本地保存与远端上传完成仍是两件事；读写挂载复用 TwoDrive 持久待上传队列，网络失败后重试。关闭时先关闭打开的挂载文件，卸载该 dev 挂载，再退出连接。默认只读挂载由内核拒绝写入。

## 现有 WebDAV 与本机共享

把 `{"username":"...","password":"..."}` 存入私有 `auth.json`（Linux 0600）；不把密码写进 URL、命令行或日志。

```bash
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json list
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json get /report.pdf ./report.pdf
twodrive-dev webdav --url https://dav.example.org/files/ --auth-file ./auth.json --write put ./new.txt /new.txt
twodrive-dev --state ~/.local/share/twodrive-dev/dav-mount webdav \
  --url https://dav.example.org/files/ --auth-file ./auth.json mount /path/to/empty-mount
```

还有 `mkdir`、`mv`、`rm`。PUT 默认仅创建新文件；覆盖须用 `--if-match` 传入 `list` 显示的当前 ETag（保留引号）。服务端没有可靠 ETag 时拒绝条件覆盖；弱 ETag 不适合 If-Match。下载不覆盖现有本地文件。

需要单独的本机 WebDAV 服务：

```bash
twodrive-dev --state ~/.local/share/twodrive-dev/local-dav dav-serve \
  --root /path/to/shared --listen 127.0.0.1:4919
```

默认只读，显示私有凭据路径。HTTP 仅允许数字回环地址，远程共享用 `serve`/`connect` 的加密通道。WebDAV 客户端拒绝 HTTP 降级和重定向，不关闭 TLS 证书验证。

## NAT 与中继

默认使用 iroh/N0 公共中继和地址发现；需要普通出站联网，无需公网 IP 或端口转发。公共中继能看到设备公钥、连接 IP、时序和流量大小，不能读取文件明文或获取解密密钥。项目不保证公共基础设施的可用性或速度。

- 自建：双方加 `--relay https://your-iroh-relay.example/`，可重复指定；此模式不使用公共 N0 DNS。部署使用 [iroh 官方 relay](https://github.com/n0-computer/iroh/tree/main/iroh-relay)，两端提供同一个可达 HTTPS 服务。
- 局域网测试：双方加 `--no-relay`，关闭公共中继与发现。重启改变 UDP 地址后需新邀请更新提示。
- 诊断中继：双方加 `--relay-only`，禁用直接 IP 传输，输出应为 `transport=relay`。

`serve` 和 `connect` 默认使用 `--network-route auto`：自动选取已启用的物理网卡，为设备共享的 DNS、UDP、中继 TCP 和签名地址发布设置物理出口。系统 HTTP 代理环境变量和 TUN 的 fake-IP DNS 不参与首次直连尝试；程序使用只在内存中保存随机凭据的私有回环连接器转发中继 TLS，继续校验服务器证书和设备身份。无需关闭代理或修改全局路由。

直连初始化、DNS、TCP、地址发布或首次设备传输连接失败时，`auto` 允许系统/代理出口兜底，并输出 `network: route=system/proxy fallback` 及发生阶段。设备传输连接重试发生在认证之前；过期/已使用邀请或撤销授权不会触发这项重试。`connect` 还显示当前所选路径和 RTT。

- `--network-route direct`：关闭自动兜底，用于核实物理出口是否能单独连通。物理接口绑定不可用时可以失败。
- `--network-route system`：主动使用系统路由和 iroh 支持的 HTTP(S) 代理环境变量。
- `--network-interface NAME`：按系统名称、Windows 友好名称或接口索引指定物理网卡。指定网卡不可用时，`auto` 明确提示系统/代理兜底，`direct` 失败；可据此测试兜底，无需修改全局路由。

系统兜底优先使用显式 HTTP(S) 代理环境变量；Windows 再读取当前用户的静态 HTTP(S) 系统代理，并尊重已配置中继主机的代理绕过规则。日志仅报告代理来源及存在性，不打印地址、凭据或 PAC 内容。自动执行 PAC/WPAD 和 SOCKS 代理支持尚未实现。系统路由的 A/AAAA 查询失败或无结果时，还会尝试原生 OS 解析，限制等待时间与未完成调用数量；此路径允许 TUN fake-IP。物理 DNS 与物理 TCP 仍拒绝 fake-IP，TXT 保持原解析器。

Windows 实机验证脚本 `scripts/verify-network.ps1` 接收临时共享目录、既有私密状态及通过独立渠道核实的程序 SHA-256。它暂时替换唯一匹配的测试 serve，采样 TCP 物理源地址和路由稳定性，再测试不存在的网卡触发兜底。已配对的 Linux 验证端只通过共享发送固定的 `next`/`finish`/`abort` 信号。脚本恢复旧 serve，删除自己拥有的程序、脚本和样本；验证端取回并删除剩余脱敏报告。这份旧分阶段脚本会重启传输端；自主重试应使用下文的远程测试进程控制端。

这是尽力绕过策略。Linux 的 TCP/DNS 支持接口与源地址约束，Windows 使用接口索引约束；iroh 的 UDP 目前使用物理源地址绑定。强制 VPN、WFP/透明网关策略不一定能被应用绕过，也不保证识别每一种虚拟适配器或支持任意 SOCKS 代理兜底。物理网卡在启动时选择，切换网络后重启 dev 可重新选择。该策略覆盖本实验的设备共享；外部 WebDAV 挂载和稳定 OneDrive 客户端的出口策略未在此修改。

2026-10-11 本机保留 FlClash TUN 时，`direct` 和最终默认 `auto` 的真实 Windows 对端数据校验与清理结果见[实测记录](../../doc/peer-direct-egress-2026-10-11.json)。本机绕过代理仍可经过加密 relay，不能将它等同于 UDP 打洞成功。[Windows 原生与双端 TUN 实测](../../doc/peer-windows-egress-2026-10-11.json)已通过默认物理 TCP 出口与 Linux 兜底；Windows 兜底启动退出仍在调查（TD-20261011-10）。

复杂 NAT 或 UDP 阻断时会使用中继，底层可经 TCP/TLS，不能保证始终 UDP 直连。设备发现和中继不保存文件内容；私钥仅在本机。

## 验证与限制

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked --features mount -- -D warnings
cargo test --locked --features mount
python3 scripts/smoke.py --binary target/release/twodrive-dev --network lan --mount
python3 scripts/smoke.py --binary target/release/twodrive-dev --network lan --mount --read-only
python3 scripts/smoke.py --binary target/release/twodrive-dev --network relay-only
```

smoke 创建两套临时设备状态和文件，结束时只卸载自己的挂载；不操作现有服务或 OneDrive。`relay-only` 需要真实公共基础设施。默认自动测试不访问公共中继。

WebDAV 按完整元数据快照轮询，默认最大 100,000 条、单响应 16 MiB；无原生 delta 和持久远端文件 ID，移动使路径 ID 改变。自建服务器仅导出普通文件/目录，隐藏符号链接，不允许 capability 外部访问。PUT 最多 8 GiB（`--max-upload` 可调整），完整接收后同步并原子替换；中断不覆盖原文件。配对后端对已有版本的文件自动协商二进制增量上传（含 PDF、Word、PowerPoint），保留完整 PUT 回退；桌面设备共享高级选项可勾选“始终整文件上传”，CLI 可用 --full-upload。只发送 PUT 的第三方 WebDAV 客户端仍上传全文件。暂不支持断点 PUT 和 Content-Range。详见[增量协议、前端接口与验证边界](../../doc/peer-delta-upload.md)。网络写请求串行保护条件检查；本机其他进程写入不受此锁约束。外部 WebDAV 的原子性由对方决定。

Unix 私有目录 0700、文件 0600；Windows 继承目录 ACL，请使用当前用户的私有状态目录。静态文件不做磁盘加密。本机 QUIC、公共中继和真实异地 NAT 是不同验证范围；实际结果见故障记录与 PR，不把模拟云端或同机测试当成真实 OneDrive/异地 NAT 端到端证明。

## 远程测试进程控制

`test-agent` 是显式启动、有租期的测试控制端。它通过已有配对的可写 TwoDrive 共享接收指令，不停止旧 serve。控制 owner 必须已在原状态中授权；每个请求由该设备签名，响应由服务端原身份签名。随机会话、递增序号和短有效期拒绝篡改、重放及其他会话指令。

将原生构建及 `scripts/start-test-agent.ps1` 放入共享中的新 `td-control-<32 位十六进制>` 目录，核实程序 SHA256 后启动一次脚本，传入原 `-State`、`-Owner` 公钥和 `-ExpectedSha256`。默认租期两小时，不能远程续期；之后 `scripts/control-test-agent.py` 通过本机已经连接的回环 WebDAV 网关发出 `start`、`snapshot`、`stop`、`finish`。快照包含启动错误、退出码、有限长度日志和 Windows 路由/TCP 采样。控制通道在测试子进程失败后仍可取日志、重试及上传新候选构建。

每个测试 serve 都使用独立的临时身份、共享与临时客户端授权；控制端和测试客户端应使用不同身份，避免并发发布同一个设备地址。`start` 只能指定固定网络选项和候选 SHA256；候选从共享的固定 `candidate.bin` 复制到私有目录，核验后运行。没有任意 shell 或主机路径参数。签名 owner 能授权候选程序执行，程序仍具有启动者的系统权限，这不是操作系统沙箱。Windows 自有进程 Job 在控制端崩溃时终止测试子树。

`finish`、租期到期或 owner 被撤销时停止并回收测试子进程、删除临时 child 状态；bootstrap 退出后删除自己的私有程序副本和共享程序负载，保留原身份/授权。Linux 收集签名结果与小型清理回执后删除该次共享控制目录。旧版只有传输 API，首次启用仍需在 Windows 启动一次；启用后的测试和重试由控制端完成。

`scripts/verify-live-test-agent.py` 是纳入版本控制的双机验证入口。它等待签名就绪回执，使用新的临时客户端身份，依次验证双端默认物理出口和两端各自的缺失网卡兜底。每阶段执行 2 MiB 全量哈希、移动、256 KiB 范围读取及删除，并把有限长度诊断保存至 `--output`。`--network-context` 应填写当前实际网络；同在校园网不能证明独立 NAT。失败时停止临时 child、保留控制端，查看签名快照后用 `--resume` 明确重试。全阶段成功才发送 `finish`；随后另行收集 bootstrap 清理回执并删除自有共享控制目录。跨重启任务应把报告保存在 `/tmp` 之外。

2026-10-12 后续验证：同版两端默认物理出口、Linux 兜底及 Windows 兜底均完成 2 MiB 完整哈希、MOVE、Range、DELETE 404；Windows 系统 DNS 原生后备修复及双端清理证据见[实机记录](../../doc/peer-windows-egress-2026-10-12.json)。本轮两端校园网，未建立独立 NAT 条件；前一日热点/校园网记录保留。

## Google Drive

新增 Google 桌面 OAuth 与普通文件读写，通过既有桌面提供者接口接入 Linux FUSE 和文件夹备份。在线文档显示为网页入口。详见 [接入、安全与验证](../../doc/google-drive-dev.md)。

Development 桌面支持 WebDAV/NAS、OneDrive、Google Drive 和 PC/NAS 连接设置。分享端须显式 `serve --discoverable --device-name NAME` 才能被局域网发现；配对仍需可信邀请。见[连接使用与验证](../../doc/storage-connections-dev.md)。
