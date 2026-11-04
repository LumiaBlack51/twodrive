# 托盘面板定位 / Tray panel positioning

当前 `dev` 中，底部任务栏的 TwoDrive 状态面板以图标实际矩形的中心水平对齐，底边距任务栏保留工作区边界 8 个逻辑像素。顶部任务栏向下展开；图标过于靠近屏幕边缘时，仅为防止面板出屏而限位。定位不再跟随鼠标的纵向位置，也不使用偏向一侧的 `x + 20 - width` 公式。

2026-11-04 更新：为使状态面板隐藏 Dock/任务栏图标，启动器在 DISPLAY 可用时默认使用 X11/XWayland。显式 GDK_BACKEND 仍优先；原生 GTK 3 Wayland 的定位支持不代表其支持隐藏任务栏提示。验证和边界见 [TD-20261103-06](incidents.md#td-20261103-06打开托盘状态小窗口时出现额外-dock-图标)。

GNOME/Zorin 同时支持 GTK X11 和原生 GTK Wayland。TwoDrive 专用 GNOME 组件读取 Shell 中 TwoDrive 托盘 actor 的矩形及所在显示器工作区，不修改系统 AppIndicator 扩展。X11 在原生窗口映射后应用位置；Wayland 由 compositor 定位该进程自己的 TwoDrive 无装饰面板。发送者 PID、窗口 PID 和窗口类必须匹配，没有通用窗口移动或任意脚本执行接口。原生 Wayland 的 TwoDrive SNI 图标通过本组件保留单击展开，其他指示器不受影响。

安装源码构建时增加组件路径：

```bash
python3 scripts/install-dav-desktop.py \
  --bundle apps/dav/build/linux/x64/release/bundle \
  --engine-dir experiments/twodrive-dev/target/release \
  --gnome-anchor packaging/gnome-tray-anchor
```

运行中更新可加 `--preserve-running-runtime`，保留旧进程的可执行文件路径，安装器不重启客户端。首次安装新 GNOME 扩展时，当前 Shell 可能尚未发现它；安装器仅将 TwoDrive 的 UUID 加入启用列表，组件在下一次登录桌面时加载，不自动重启 Shell、不要求开启 unsafe Eval。组件可用时，启动器在 Wayland 会话选择原生 Wayland；否则使用 X11，用户显式 `GDK_BACKEND` 始终优先。

本机交付已安装新程序和组件，但遵照用户正在前台编码的约束，没有重启当前客户端或桌面，也没有在用户显示器上移动鼠标、切换窗口或弹出测试面板。当前旧进程仍运行在保留的 runtime；新定位在新桌面会话加载组件并重新启动 Development 后生效，Google 登录和连接配置保留。

实际验证：

- 同一中心对齐/任务栏边界断言在 HEAD 旧几何代码退出 -6，修复通过；C++ 与 GJS 几何回归覆盖上下任务栏、负坐标显示器、边缘限位和图标移动。
- [原生 Wayland 摘要](tray-wayland-validation-2026-10-14.json)：GNOME 46 headless compositor、原生 GTK Wayland、私有 D-Bus，三轮展开/收起实际 frame 矩形一致；故意传入无关激活坐标仍按真实图标定位，其他进程的移动请求被拒绝，后台启动保持隐藏。
- [X11 摘要](tray-x11-validation-2026-10-14.json)：独立 Xvfb + GNOME X11 compositor、实际 GTK X11/XEmbed 图标，通过该私有显示器的输入完成三轮单击展开/收起，400 ms 稳定后矩形一致。没有向用户显示器发送输入。
- Flutter analyze、14 项测试、Linux release build、安装器保留运行路径/配置与仅排队本组件的 2 项回归通过。

上述虚拟显示器为 1×，实际图标在顶部右边缘，发生合理横向限位。底部中心/负坐标/小屏是几何回归；当前实体 Zorin 2× 桌面的修复后弹窗、实体多屏、其他 Wayland compositor 和原生 Windows 本轮未实测。Windows 同步采用居中与任务栏边界公式，不能用 Linux 结果代替 Windows 结果。

后台隔离验证入口（GNOME 45+、AppIndicators、X11 需 Xvfb）：

```bash
python3 scripts/test-dav-panel-isolated.py --backend wayland \
  --bundle apps/dav/build/linux/x64/release/bundle \
  --engine experiments/twodrive-dev/target/release/twodrive-dav-engine \
  --peer experiments/twodrive-dev/target/release/twodrive-dev \
  --output /absolute/wayland-result.json
# X11 将 --backend 改为 x11；--xvfb 可指定私有 Xvfb 二进制。
```

实现参考：[GNOME D-Bus 服务](https://gjs.guide/guides/gio/dbus.html)、[Meta.Window.move_resize_frame](https://mutter.gnome.org/meta/method.Window.move_resize_frame.html)。故障记录：[TD-20261014-06](incidents.md#td-20261014-06托盘状态面板未固定在任务栏图标正上方)。[源码提交 37f8f3b](https://github.com/LumiaBlack51/twodrive/commit/37f8f3b69d25395952cb45e71bf5a1110fa91063)；[最终编译提交 863bf6d](https://github.com/LumiaBlack51/twodrive/commit/863bf6d6218853d79eecb57be6be8c8c2a4f6cef)。[Development 0.1.2 Release](https://github.com/LumiaBlack51/twodrive/releases/tag/dev-v0.1.2) 已发布；见[最终构建与范围](development-0.1.2.md)。

## English

The panel is centered horizontally over its actual tray icon and sits 8 logical pixels above the bottom taskbar workarea boundary. Screen-edge constraints keep it on-screen; top taskbars open below. The TwoDrive-only GNOME component provides actual actor geometry and permits the originating TwoDrive process to place its own undecorated panel. It supports native GTK Wayland and GTK X11 on GNOME/Zorin without modifying the system AppIndicator extension or enabling unsafe Shell evaluation.

Private GNOME 46 Wayland and Xvfb/X11 sessions passed three open/close cycles. These tests did not interact with the user's foreground desktop. Physical HiDPI/multi-monitor displays, other Wayland compositors and native Windows remain unverified for this change. The installed component may require the next desktop login to load; the running client and desktop were deliberately left intact while the user was coding.
