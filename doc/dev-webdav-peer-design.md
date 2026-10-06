# dev 分支：WebDAV 与加密设备共享设计

日期：2026-10-06。实现位于 `experiments/twodrive-dev`，独立 Cargo workspace、锁文件、状态目录和二进制 `twodrive-dev`。稳定 `twodrive` CLI、daemon、OneDrive 数据库、服务、打包入口不变，不自动安装或启动。

Linux 通过类型重导出复用已有存储与 FUSE 接口；Windows 使用实验模块自己的元数据与存储接口，不引入稳定核心库的 Unix 权限、OAuth 或数据库实现。元数据构造兼容性由 Linux 回归测试核对，Windows WebDAV/设备共享由原生 CI 编译、协议测试与双进程冒烟验证；这不代表稳定 TwoDrive 已移植到 Windows。

## 数据与传输

1. `WebDavBackend` 实现已有 `CloudBackend`：HTTPS + Basic 身份验证、Depth:1 PROPFIND 递归枚举、GET 流式下载、PUT 流式上传与 If-Match、MKCOL、MOVE、DELETE。禁止重定向，不把凭据发送到其他站点；只允许 HTTPS 或本机数字回环 HTTP。URL 编码与 XML DAV 命名空间严格处理。
2. `serve` 只导出显式指定目录。WebDAV 在 QUIC 流内处理，不监听公网 HTTP。目录访问使用 cap-std 的目录能力，防止路径和符号链接逃逸。PUT 写临时文件，完整接收并同步后原子替换；未完成上传不覆盖原内容。写入默认关闭。
3. iroh 1.3 的 QUIC/TLS 身份直接绑定 Ed25519 设备公钥。QUIC 提供可靠有序流、拥塞控制与加密；不另造密码协议。默认 N0 基础设施帮助发现地址、打洞和密文中继，自动迁移到可用直连。自定义中继使用 Minimal preset，不联系 N0 DNS；离线/LAN 模式不使用公共基础设施。
4. `connect` 将已认证远端暴露为本机回环 WebDAV，并要求独立随机 Basic 凭据。本机普通 WebDAV 客户端可使用该地址；Linux 可通过独立 `mount` 功能复用现有 TwoDrive FUSE 引擎。每个挂载使用独立状态，绑定远端指纹与根 URL，防止重用缓存到不同服务。

## 配对与授权

服务器生成短期单次邀请文件（256 bit 随机秘密、设备公钥、寻址提示、协议版本、十分钟有效期）。通过可信渠道传到另一台机器，`connect --ticket-file` 验证并固定服务器设备 ID。服务器将邀请原子兑换为 QUIC 对端公钥的允许项；重连只需设备密钥。邀请秘密只保存哈希，消费与允许项在同一个原子状态更新中完成。邀请文件必须保密；持有者可抢先兑换，公开设备 ID 本身不授予访问。

私钥、邀请、允许项和 WebDAV 凭据保存在独立私有目录；Unix 目录 0700、文件 0600。Windows 继承当前用户私有目录的 ACL（部署者须保证 ACL）。同一个状态不能并行运行多个 endpoint；控制状态用文件锁与原子替换保护。`revoke` 移除授权，每条请求重新检查；已有连接也不能发起新请求。正在完成的一条传输可以结束，立即终止需停止服务器。

授权绑定共享目录及读写模式；换目录/权限需新状态目录。最初的设备范围是单用户、单共享目录；多用户 ACL 与 GUI 不是当前实现范围。

## 不能省略的边界

- 不能保证任意 NAT 都成功打洞。对称 NAT、UDP 全阻断等情形需要中继，中继路径可能经 TCP/TLS，不能把它称为 UDP 直连。双方仅需普通出站联网，无 VPN、虚拟网卡或路由变更。
- 中继看得到设备公钥、IP、时序及流量大小，看不到 QUIC 明文或解密密钥。公共基础设施的可用性与带宽没有本项目保证；自建可控制依赖。
- WebDAV 客户端按完整快照轮询；不支持服务端 delta/持久文件 ID。移动后的路径 ID 改变。挂载刷新不能被称为 Syncthing 的双向目录镜像/冲突合并。
- 不支持断点 PUT；失败保留 TwoDrive 本地待上传内容，重试整文件。外部 WebDAV 服务的原子性由服务决定。共享目录的本地其他写入者不受网络写锁约束。
- 本机回环 HTTP 和文件静态存储不提供磁盘加密；所有跨机器网络传输均加密。Basic 密码不出现在命令行参数或普通日志里。
- 测试用临时目录、独立设备状态及真实本机 QUIC 流。公共中继可达性和两台真实异地 NAT 设备的测试必须单独标明；本机测试不能证明全部路由环境或真实 OneDrive 兼容性。

## 依据

- [iroh 1.3.0](https://docs.rs/iroh/1.3.0/iroh/)，锁定版本的 endpoint presets 源码核实公共 DNS 与 relay 配置。
- [RFC 4918](https://www.rfc-editor.org/rfc/rfc4918) 与 [dav-server](https://docs.rs/dav-server/0.11.0/dav_server/)。
- [cap-std](https://docs.rs/cap-std/4/cap_std/fs/struct.Dir.html)：目录能力限制文件操作范围。
