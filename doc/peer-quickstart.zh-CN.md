# TwoDrive Peer 实验版：第一次双机联调

本程序独立于 TwoDrive 挂载、daemon 和桌面组件，不安装服务，不读取旧配置、数据库、缓存或 token。Windows 只需解压 CI artifact，使用 PowerShell 运行 twodrive-peer.exe，无需 Rust。默认独立目录：Windows `%LOCALAPPDATA%\twodrive-peer-lab`；Linux `~/.local/share/twodrive-peer-lab`。可以用全局 `--state-dir` 指定另一个专用目录，勿放进任何 OneDrive 同步/挂载目录。

## 两端首次启动

Windows PowerShell：

```powershell
.\twodrive-peer.exe init
.\twodrive-peer.exe login
.\twodrive-peer.exe run --auto-update
```

Linux 使用本实验分支的 peer（二进制完全独立于已安装 TwoDrive）：

```bash
/path/to/twodrive-peer init
/path/to/twodrive-peer login
/path/to/twodrive-peer run
```

在浏览器中登录**相同 Microsoft/OneDrive 账号**。继续使用 TwoDrive 原有 public Application ID 和 PKCE，不需要 client secret。登录回调使用 localhost:53682，请勿同时运行另一份登录流程。既有挂载服务不用停止。peer 另外请求 Files.ReadWrite.AppFolder，在应用文件夹下只使用 `peer-control-v1` 命名空间；这部分云端文件可能被其它 OneDrive 客户端列出，但 peer 不会修改现用 TwoDrive 的本地状态。

在两端另开终端运行 `peers`，自动发现设备。对照**另一台机器本地 init 输出**的完整 64 字符指纹，然后两端各执行：

```text
twodrive-peer trust OTHER_DEVICE_FINGERPRINT
```

Windows 命令需加 `.\` 和 `.exe`。不要仅从云端列表复制陌生指纹后信任。云端不能证明物理设备归属，因此首次人工核对是安全边界。运行中的进程自动重新载入信任表，随后打印 `Authenticated ...`。每次启动新会话后自动重新握手；presence 在约三分钟后过期。

## 双向测试

Linux 向 Windows 发测试消息：

```text
twodrive-peer ping --to WINDOWS_FINGERPRINT
```

Windows 向 Linux 发测试消息：

```powershell
.\twodrive-peer.exe ping --to LINUX_FINGERPRINT
```

消息通过本地 outbox 交给正在运行的 peer；接收方打印 PING，发送方打印 `PONG ... round trip verified`。还可使用 `request-status --to ...` 和 `status`。请检查状态的 updated 时间；旧状态文件不表示当前在线。轮询间隔 5 秒，Graph 限流和网络延迟可能使握手更慢。停止使用 Ctrl+C。无需卸载挂载或重启现有 TwoDrive。

## 目录边界

默认没有授权任何备份目录。可在 peer 停止时执行：

```powershell
.\twodrive-peer.exe allow-directory 'D:\SelectedBackup'
```

选择只保存在本机，云端最多获得已选择目录的数量，没有绝对路径。本阶段是控制通道，尚无远程文件读取、扫描或备份传输命令；不会因收到消息遍历硬盘。`clear-directories` 清除本地授权。后续文件传输实现必须以目录句柄约束路径、拒绝 reparse point/symlink 逃逸，不能仅依赖字符串前缀。

## 更新与恢复

Windows `run --auto-update` 接受可信设备的 `notify-update --to ... --tag peer-v0.1.1` 通知。通知仅是 tag，不接受任意 URL、命令或公钥。程序到固定 TwoDrive GitHub Release 下载 `twodrive-peer-windows-x86_64.json` 与 `twodrive-peer.exe`，验证内置独立 Ed25519 发布公钥、产品、协议、严格递增版本、平台、名称、大小和 SHA-256，然后暂存。

也可执行 `update --tag ...`，或离线执行 `stage-update --manifest ... --binary ...`。离线暂存后重启 `run` 才激活。没有符合命名且签名的 Release 时会安全拒绝；CI artifact 的 SHA256SUMS 本身不是更新授权。bootstrap exe 保留，实际更新装在独立 state 目录 updates 下；健康检查/早期启动失败时退回上一版本。反回滚版本水位保留。网络中断不应触发程序降级。保留 bootstrap exe 和 updates 下的旧版本文件以便恢复。

## 验证边界

自动测试使用合成身份、模拟云存储和本地更新包；真实 Microsoft 登录、账号策略、Graph AppFolder/permanentDelete 可用性以及 Windows/Linux 真机双向链路必须按上面流程验证。测试结果和发布交付见 peer-implementation.md。该实验版本不包含文件备份、Explorer 集成或 Windows 服务。

## 0.1.1 真实 Graph 排障与替换

退出两端旧 peer（Ctrl+C），替换独立的 `twodrive-peer.exe` / `twodrive-peer`，保留原 peer state、身份与登录。先用 `--version` 确认为 0.1.1；不要删除 identity.dat，也不需要重复 init/login。没有新的签名 Release 时 `--auto-update` 不会自动获取本次 CI 修复，需要手动替换。

Windows：

```powershell
$env:TWODRIVE_PEER_DIAGNOSTICS = '1'
.\twodrive-peer.exe --version
.\twodrive-peer.exe doctor
.\twodrive-peer.exe trust LINUX_FINGERPRINT
.\twodrive-peer.exe run --auto-update
```

Linux：

```bash
export TWODRIVE_PEER_DIAGNOSTICS=1
./twodrive-peer --version
./twodrive-peer doctor
./twodrive-peer trust WINDOWS_FINGERPRINT
./twodrive-peer run
```

doctor 必须在本机 run 停止时执行。它在 AppFolder/peer-control-v1/diagnostics 内写入一个唯一探针、读回校验、删除并确认消失；不删除旧消息或其它文件。成功应有 `doctor put=ok`、`doctor get=ok content_match=true`、`doctor delete=ok`、`doctor list=ok deleted_object_absent=true`。失败时保留 operation/status/code，不要提供 token 文件或 Graph 完整正文。

诊断区分 approot.resolve-or-create、namespace.lookup/create、bucket.lookup/create、list、get.content、put、delete.lookup/drive/permanent/recycle。AppFolder 的第一次 GET 可自动创建应用目录。`status=401` 提示 token/audience；403 提示权限/scope/账号策略；`token-acquisition-or-refresh` 表示本地 token 获取/续期失败。doctor 中 configured_appfolder 仅表示配置请求了该权限，并非证明微软实际授予。网络故障仅报告 timeout/connect-or-tls/transport，不输出请求 URL。

真实账号上原来的空 POST permanentDelete 返回 411；添加 Content-Length: 0 后返回 400 invalidRequest / API not found。0.1.1 在此**精确响应**下回退为普通 DELETE（进入 OneDrive 回收站），只作用于本次控制对象。401/403、其它 400、429 或 5xx 不触发回退。每个进程首次回退会明确告知；回收站可能积累短期控制消息，不是永久删除保证。不要为排障清空整个 OneDrive 回收站。

确认双方 `Authenticated` 后再新发 ping；以前排队消息可能已过期。两端必须互相信任，Linux 单方 trust 不够。若需要重新登录，先停止 peer，只运行此实验 peer 的 login。取消详细成功日志：PowerShell `Remove-Item Env:TWODRIVE_PEER_DIAGNOSTICS`；Linux `unset TWODRIVE_PEER_DIAGNOSTICS`，错误诊断始终保留。
