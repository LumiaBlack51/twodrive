# TwoDrive dev：WebDAV 与加密设备共享

[English](README.md) · [架构设计](../../doc/dev-webdav-peer-design.md) · [故障记录](../../doc/incidents.md)

这是 `dev` 分支上的独立实验入口 `twodrive-dev`。正在使用的稳定 TwoDrive、配置、数据库和原 main 工作目录保持原样。dev 现已整合 Windows 预览及 OneDrive peer-control 源码，见[组合指南](../../doc/dev-integration.md)。这里使用已有的文件系统引擎，但有独立 Cargo workspace、锁文件、设备状态和挂载缓存。

已实现：连接现有 HTTPS WebDAV；将显式目录共享；一次邀请完成设备配对；QUIC/TLS 加密、可靠 UDP 直连、NAT 打洞和加密中继回退；设备撤销；本机 WebDAV 网关；可选 Linux FUSE 挂载。没有 VPN、虚拟网卡或路由配置。它不是 Syncthing 式的双向目录镜像，暂时没有设置 GUI。

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

WebDAV 按完整元数据快照轮询，默认最大 100,000 条、单响应 16 MiB；无原生 delta 和持久远端文件 ID，移动使路径 ID 改变。自建服务器仅导出普通文件/目录，隐藏符号链接，不允许 capability 外部访问。PUT 最多 8 GiB（`--max-upload` 可调整），完整接收后同步并原子替换；中断不覆盖原文件。暂不支持断点 PUT、PATCH 和 Content-Range。网络写请求串行保护条件检查；本机其他进程写入不受此锁约束。外部 WebDAV 的原子性由对方决定。

Unix 私有目录 0700、文件 0600；Windows 继承目录 ACL，请使用当前用户的私有状态目录。静态文件不做磁盘加密。本机 QUIC、公共中继和真实异地 NAT 是不同验证范围；实际结果见故障记录与 PR，不把模拟云端或同机测试当成真实 OneDrive/异地 NAT 端到端证明。
