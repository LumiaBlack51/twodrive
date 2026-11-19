import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:file_selector/file_selector.dart';
import 'status_tray.dart';
import 'tray_panel.dart';
import 'backup_page.dart';
import 'sharing_page.dart';
import 'package:window_manager/window_manager.dart';
import 'client.dart';
import 'desktop_typography.dart';
import 'profile_dialog.dart';
import 'local_file_preflight.dart';
import 'native_local_files.dart';
import 'connection_card.dart';
import 'mount_card.dart';
import 'workspace_navigation.dart';
import 'cfapi_page.dart';
import 'sync_policy_card.dart';
import 'overview_page.dart';

String bytes(num value) {
  if (value < 1024) {
    return '$value B';
  }
  if (value < 1048576) {
    return '${(value / 1024).toStringAsFixed(1)} KB';
  }
  if (value < 1073741824) {
    return '${(value / 1048576).toStringAsFixed(1)} MB';
  }
  return '${(value / 1073741824).toStringAsFixed(1)} GB';
}

String childPath(String parent, String name) =>
    '${parent == '/' ? '' : parent}/$name';
String parentPath(String path) {
  final index = path.lastIndexOf('/');
  return index <= 0 ? '/' : path.substring(0, index);
}

bool trayReady = false;

const _transferInset = Color(0xffeaeefb);
const _transferAccent = Color(0xff236ed5);
const _transferMuted = Color(0xff707987);
const _transferSuccess = Color(0xff328352);

enum _TransferSection {
  upload('上传'),
  backup('备份'),
  download('下载'),
  mount('挂载同步');

  const _TransferSection(this.label);
  final String label;

  static _TransferSection forRow(TransferRow row) {
    if (row.manual != null) {
      return row.info['kind'] == 'download' ? download : upload;
    }
    return row.info['source'] == 'backup' ? backup : mount;
  }
}

class Workspace extends StatefulWidget {
  const Workspace({super.key, required this.client, required this.desktop});
  final StorageClient client;
  final bool desktop;
  @override
  State<Workspace> createState() => _WorkspaceState();
}

class _WorkspaceState extends State<Workspace> with WindowListener {
  StorageClient get c => widget.client;
  final tray = StatusTray();
  bool panel = false;
  bool presenting = false;
  bool _windowHidden = false;
  bool _directoryReadQueued = false;
  int page = WorkspacePage.overview.index;
  _TransferSection? transferSection;
  String filter = '';
  String _traySignature = '';
  Future<void> _trayWork = Future.value();
  Future<void>? _quitWork;
  @override
  void initState() {
    super.initState();
    c.addListener(changed);
    if (widget.desktop) {
      windowManager.addListener(this);
      tray.onQuit = () => unawaited(quit());
      tray.onActivate = (position) => unawaited(showPanel(position));
      unawaited(initTray());
    }
  }

  Future<void> initTray() async {
    try {
      await tray.icon('offline');
      trayReady = true;
      await windowManager.setPreventClose(true);
      await updateTray();
    } catch (_) {
      c.error = '系统托盘不可用，关闭窗口将退出客户端。';
      changed();
    }
  }

  void changed() {
    if (mounted) {
      setState(() {});
    }
    if (widget.desktop && trayReady) {
      _trayWork = _trayWork.then((_) => updateTray()).catchError((Object _) {});
    }
  }

  Future<void> updateTray() async {
    final signature =
        '${c.connected}:${c.busy}:${c.error}:${c.mounted}:${c.proxyFallback}:${c.paused}:${c.syncing}:${c.syncError}:${c.trayTitle}';
    if (_traySignature == signature) {
      return;
    }
    _traySignature = signature;
    final icon = c.trayState;
    await tray.icon(icon, title: c.trayTitle);
  }

  void refreshWorkspace({bool forceDirectory = false}) {
    _windowHidden = false;
    c.setView(
      visible: true,
      files: !panel && page == WorkspacePage.files.index,
      transfers: !panel && page == WorkspacePage.transfers.index,
      quota:
          panel ||
          page == WorkspacePage.connections.index ||
          page == WorkspacePage.overview.index,
    );
    unawaited(c.refreshView(forceDirectory: forceDirectory));
  }

  void navigateTo(int destination) {
    setState(() => page = destination);
    if (widget.desktop) refreshWorkspace(forceDirectory: true);
  }

  @override
  void onWindowFocus() {
    if (!presenting) refreshWorkspace(forceDirectory: true);
  }

  @override
  void onWindowMinimize() => hideWorkspace();

  void hideWorkspace() {
    _windowHidden = true;
    c.setView(visible: false);
  }

  @override
  void onWindowClose() {
    if (c.quitting) return;
    hideWorkspace();
    unawaited(windowManager.hide());
  }

  @override
  void onWindowBlur() {
    // A visible Files window may lose focus during account selection. The
    // selection still needs to load its directory; hiding is handled separately.
    if (panel || page != WorkspacePage.files.index) c.setView(visible: false);
    if (!Platform.isLinux && !Platform.isWindows && panel && !presenting) {
      unawaited(windowManager.hide());
    }
  }

  Future<void> showPanel(Offset position) async {
    if (presenting) return;
    if (panel && await windowManager.isVisible()) {
      hideWorkspace();
      await windowManager.hide();
      return;
    }
    presenting = true;
    hideWorkspace();
    await windowManager.hide();
    setState(() => panel = true);
    await windowManager.setFullScreen(false);
    await windowManager.unmaximize();
    for (var i = 0; i < 20 && await windowManager.isMaximized(); i++) {
      await Future<void>.delayed(const Duration(milliseconds: 50));
    }
    await windowManager.setTitleBarStyle(TitleBarStyle.hidden);
    await windowManager.setSkipTaskbar(true);
    await windowManager.setAlwaysOnTop(true);
    if (Platform.isWindows) {
      await windowManager.setMinimumSize(const Size(390, 570));
    }
    await tray.panel(true);
    if (!Platform.isLinux && !Platform.isWindows) {
      await windowManager.show();
      await windowManager.focus();
    }
    presenting = false;
    if (mounted) refreshWorkspace();
  }

  Future<void> showManagement([int? destination]) async {
    if (presenting) return;
    presenting = true;
    hideWorkspace();
    final fromPanel = panel;
    var veiled = false;
    try {
      // The tray and management page share a native surface. Never expose
      // decoration/geometry changes with the old tray frame still visible.
      await windowManager.hide();
      if (!mounted) return;
      setState(() {
        panel = false;
        _windowHidden = false;
        page = destination ?? WorkspacePage.overview.index;
      });
      loadVisibleDirectory();
      await windowManager.setAlwaysOnTop(false);
      await windowManager.setSkipTaskbar(false);
      await tray.panel(false);
      await windowManager.setTitleBarStyle(TitleBarStyle.normal);
      await windowManager.setMinimumSize(const Size(840, 580));
      await windowManager.setSize(const Size(1120, 760));
      await windowManager.center();
      if (Platform.isLinux && fromPanel) {
        await windowManager.setOpacity(0);
        veiled = true;
      }
      await windowManager.show();
      if (veiled) {
        // A mapped surface receives its final GTK size. Allow that layout and
        // the following Flutter frame to replace the tray-sized scene before
        // revealing it. This is a framework-frame wait, not a GPU fence.
        await WidgetsBinding.instance.endOfFrame;
        await WidgetsBinding.instance.endOfFrame;
        await windowManager.setOpacity(1);
        veiled = false;
      }
      await windowManager.focus();
    } finally {
      presenting = false;
      if (veiled) await windowManager.setOpacity(1);
    }
    if (mounted) refreshWorkspace(forceDirectory: true);
  }

  Future<void> show() => showManagement(page);
  Future<void> openMount([String? profileId]) async {
    final id = profileId ?? c.selected;
    if (id != null) await c.openMountFolder(id);
  }

  Future<void> signOut() async {
    await showManagement(2);
    if (await confirm('退出登录？', '断开此账号并移除客户端中保存的登录信息。服务器文件、备份源和独立的设备配对状态会保留。')) {
      await c.signOut();
    }
  }

  Future<void> quit() => _quitWork ??= _quitOnce();

  Future<void> _quitOnce() async {
    try {
      final requested = <(Object?, Object?)>{};
      List<Json> closeProcesses = [];
      while (mounted) {
        if (widget.desktop) {
          hideWorkspace();
          try {
            await windowManager.hide();
          } catch (_) {}
        }
        if (await c.shutdown(closeMountProcesses: closeProcesses)) break;
        if (widget.desktop) {
          await windowManager.show();
          _windowHidden = false;
          await windowManager.focus();
        }
        if (!mounted || c.quitBlockers.isEmpty) return;
        final processes = <(Object?, Object?)>{
          for (final blocker in c.quitBlockers)
            for (final program in (blocker['programs'] as List).cast<Json>())
              if (program['can_close'] != false)
                (program['pid'], program['start_time']),
        }.difference(requested);
        if (!await _confirmQuitWithOpenMounts(processes)) return;
        requested.addAll(processes);
        closeProcesses = [
          for (final (pid, startTime) in processes)
            {'pid': pid, 'start_time': startTime},
        ];
      }
      if (!mounted) return;
      if (trayReady) {
        await _trayWork;
        await tray.destroy();
        trayReady = false;
      }
      if (widget.desktop) {
        await windowManager.setPreventClose(false);
        await windowManager.destroy();
      } else {
        await SystemNavigator.pop();
      }
    } finally {
      _quitWork = null;
    }
  }

  Future<bool> _confirmQuitWithOpenMounts(
    Set<(Object?, Object?)> closePrograms,
  ) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          scrollable: true,
          title: const Text('挂载目录正在使用'),
          content: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                closePrograms.isNotEmpty
                    ? '请先保存工作并处理标记为手动关闭的程序。确认后 TwoDrive 会请求其余列出的程序退出。未保存的内容可能丢失。'
                    : '请关闭仍在使用这些目录的文件、窗口或终端，再重试退出。',
              ),
              for (final blocker in c.quitBlockers) ...[
                const SizedBox(height: 12),
                Text(blocker['name'] as String),
                Text(blocker['path'] as String),
                for (final program
                    in (blocker['programs'] as List).cast<Json>())
                  Text(
                    '${program['name']}（PID ${program['pid']}）'
                    '${closePrograms.contains((program['pid'], program['start_time'])) ? '' : ' · 请手动关闭或离开挂载目录'}',
                  ),
              ],
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(closePrograms.isNotEmpty ? '关闭程序并退出' : '重试退出'),
            ),
          ],
        ),
      ) ??
      false;

  @override
  void dispose() {
    c.removeListener(changed);
    if (widget.desktop) {
      windowManager.removeListener(this);
      tray.onActivate = null;
    }
    super.dispose();
  }

  Future<String?> nameDialog(String title, [String initial = '']) async {
    final text = TextEditingController(text: initial);
    String? validation;
    final result = await showDialog<String>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: Text(title),
          content: SizedBox(
            width: 380,
            child: TextField(
              controller: text,
              autofocus: true,
              decoration: InputDecoration(
                labelText: '名称',
                errorText: validation,
              ),
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () {
                final value = text.text.trim();
                if (value.isEmpty ||
                    value == '.' ||
                    value == '..' ||
                    value.contains(RegExp(r'[/\\\x00]'))) {
                  update(() => validation = '请输入有效的单个文件名');
                  return;
                }
                Navigator.pop(context, value);
              },
              child: const Text('确定'),
            ),
          ],
        ),
      ),
    );
    // The dialog route may animate out while its text field is still mounted.
    Future<void>.delayed(const Duration(seconds: 1), text.dispose);
    return result;
  }

  Future<bool> confirm(String title, String message) async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(title),
          content: Text(message),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('确认'),
            ),
          ],
        ),
      ) ??
      false;
  Future<void> upload() async {
    final identity = c.target;
    final directory = c.path;
    final files = await openFiles(confirmButtonText: '上传');
    for (final file in files) {
      if (!c.connected || c.busy) {
        break;
      }
      await c.transfer('upload', {
        ...identity,
        'source': file.path,
        'path': childPath(directory, file.name),
      });
      if (c.error != null) {
        break;
      }
    }
  }

  Future<void> download(Json file) async {
    final target = await getSaveLocation(
      suggestedName: file['name'] as String,
      confirmButtonText: '下载',
    );
    if (target != null) {
      await c.transfer('download', {
        'profile': file['profile'],
        'revision': file['revision'],
        'path': file['path'],
        'output': target.path,
        'size': file['size'],
      });
    }
  }

  Future<void> fileAction(String action, Json file) async {
    switch (action) {
      case 'view':
        await c.viewCloudFile(file);
      case 'download':
        await download(file);
      case 'rename':
        final name = await nameDialog('重命名', file['name'] as String);
        if (name != null) {
          await c.mutate('rename', {
            'profile': file['profile'],
            'revision': file['revision'],
            'path': file['path'],
            'to': childPath(parentPath(file['path'] as String), name),
          });
        }
      case 'delete':
        if (await confirm(
          '删除 ${file['name']}？',
          file['directory'] == true ? '文件夹及其内容将从服务器删除。' : '此文件将从服务器删除。',
        )) {
          await c.mutate('delete', {
            'path': file['path'],
            'profile': file['profile'],
            'revision': file['revision'],
          });
        }
      case 'replace':
        final source = await openFile(confirmButtonText: '选择新内容');
        if (source != null &&
            await confirm('替换 ${file['name']}？', '仅在服务器版本未发生变化时覆盖。')) {
          await c.transfer('upload', {
            'profile': file['profile'],
            'revision': file['revision'],
            'path': file['path'],
            'source': source.path,
            'etag': file['etag'],
          });
        }
    }
  }

  @override
  Widget build(BuildContext context) {
    if (panel) {
      return TrayPanel(
        client: c,
        onManage: showManagement,
        onRefresh: refreshWorkspace,
        onOpenFolder: () async {
          if (!c.canMount) {
            await showManagement(0);
            return;
          }
          if (!c.mounted) {
            await c.mountToggle();
          }
          await openMount();
        },
        onQuit: quit,
        onSignOut: signOut,
      );
    }
    return Scaffold(
      body: Row(
        children: [
          Container(
            width: 184,
            color: const Color(0xffe1e5f1),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Expanded(
                  child: ListView(
                    key: const ValueKey('navigation'),
                    children: [
                      const Padding(
                        padding: EdgeInsets.fromLTRB(22, 20, 16, 4),
                        child: Row(
                          children: [
                            Icon(
                              Icons.cloud,
                              size: 28,
                              color: Color(0xff326bd6),
                            ),
                            SizedBox(width: 10),
                            Expanded(
                              child: FittedBox(
                                fit: BoxFit.scaleDown,
                                alignment: Alignment.centerLeft,
                                child: Text(
                                  'TwoDrive',
                                  style: TextStyle(
                                    fontSize: 22,
                                    fontWeight: FontWeight.w700,
                                  ),
                                ),
                              ),
                            ),
                          ],
                        ),
                      ),
                      Padding(
                        padding: EdgeInsets.only(bottom: 12, top: 3),
                        child: Text(
                          c.status['build']?['core_version'] is String
                              ? 'Windows · ${c.status['build']['core_version']}'
                              : '文件与同步',
                          textAlign: TextAlign.center,
                          style: TextStyle(color: Colors.blueGrey),
                        ),
                      ),
                      for (final destination in WorkspacePage.sidebar)
                        Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 6,
                            vertical: 2,
                          ),
                          margin: const EdgeInsets.symmetric(horizontal: 8),
                          decoration: BoxDecoration(
                            color: page == destination.index
                                ? const Color(0xffd2dbef)
                                : null,
                            borderRadius: BorderRadius.circular(12),
                          ),
                          child: ListTile(
                            dense: true,
                            selected: page == destination.index,
                            selectedTileColor: const Color(0xffd2dbef),
                            selectedColor: const Color(0xff2472df),
                            shape: RoundedRectangleBorder(
                              borderRadius: BorderRadius.circular(12),
                            ),
                            leading: Icon(destination.icon),
                            title: Text(
                              destination.label,
                              style: TextStyle(
                                fontSize: 16,
                                fontWeight: page == destination.index
                                    ? FontWeight.w600
                                    : FontWeight.w400,
                              ),
                            ),
                            onTap: () => navigateTo(destination.index),
                          ),
                        ),
                    ],
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.all(20),
                  child: Text(
                    c.connectedProfiles.isNotEmpty
                        ? '● ${c.connectedProfiles.length} 个账号已连接'
                        : '○ 未连接',
                    style: TextStyle(
                      color: c.connected ? Colors.teal : Colors.blueGrey,
                    ),
                  ),
                ),
                if (widget.desktop)
                  TextButton.icon(
                    onPressed: c.quitting ? null : quit,
                    icon: const Icon(Icons.power_settings_new, size: 18),
                    label: Text(c.quitting ? '正在退出…' : '退出客户端'),
                  ),
                const SizedBox(height: 18),
              ],
            ),
          ),
          Expanded(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(28, 26, 28, 16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Row(
                    children: [
                      Text(
                        WorkspacePage.values[page].label,
                        style: const TextStyle(
                          fontSize: 28,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const Spacer(),
                      if (page != WorkspacePage.files.index)
                        IconButton(
                          tooltip: '刷新状态',
                          onPressed: !c.available || c.busy || c.quitting
                              ? null
                              : () => refreshWorkspace(),
                          icon: const Icon(Icons.refresh),
                        ),
                      const SizedBox(width: 12),
                      if (page != WorkspacePage.cfapi.index &&
                          page != WorkspacePage.overview.index)
                        FilledButton.tonalIcon(
                          onPressed:
                              c.busy || c.selected == null || !c.available
                              ? null
                              : c.connected
                              ? c.disconnect
                              : c.connect,
                          icon: Icon(c.connected ? Icons.link_off : Icons.link),
                          label: Text(c.connected ? '断开' : '连接'),
                        ),
                    ],
                  ),
                  Padding(
                    padding: const EdgeInsets.only(top: 6, bottom: 20),
                    child: Text(
                      page == WorkspacePage.overview.index
                          ? '${c.profile?['name'] ?? 'TwoDrive'} · 账户与同步状态'
                          : page == WorkspacePage.cfapi.index
                          ? 'OneDrive · Windows CFAPI'
                          : c.connected
                          ? '${c.profile?['name'] ?? c.providerLabel} · ${c.account}'
                          : c.profile?['name'] as String? ?? '添加存储账号或已配对设备',
                      style: const TextStyle(color: Colors.blueGrey),
                    ),
                  ),
                  if (c.proxyFallback)
                    const Card(
                      color: Color(0xfffff2cd),
                      child: Padding(
                        padding: EdgeInsets.all(12),
                        child: Row(
                          children: [
                            Icon(
                              Icons.warning_amber_rounded,
                              color: Color(0xffe0a000),
                            ),
                            SizedBox(width: 8),
                            Expanded(
                              child: Text('直连未能完成，已使用系统／代理兜底。可在设置中查看网络路径。'),
                            ),
                          ],
                        ),
                      ),
                    ),
                  if (c.error != null)
                    Container(
                      margin: const EdgeInsets.only(bottom: 12),
                      padding: const EdgeInsets.all(12),
                      decoration: BoxDecoration(
                        color: const Color(0xffffeee9),
                        borderRadius: BorderRadius.circular(10),
                      ),
                      child: Row(
                        children: [
                          const Icon(
                            Icons.error_outline,
                            color: Colors.deepOrange,
                          ),
                          const SizedBox(width: 10),
                          Expanded(
                            child: ConstrainedBox(
                              constraints: const BoxConstraints(maxHeight: 120),
                              child: SingleChildScrollView(
                                child: SelectableText(c.error!),
                              ),
                            ),
                          ),
                          IconButton(
                            tooltip: '关闭提示',
                            onPressed: () {
                              c.error = null;
                              changed();
                            },
                            icon: const Icon(Icons.close),
                          ),
                        ],
                      ),
                    ),
                  if (c.busy || c.quitting)
                    Column(
                      children: [
                        const LinearProgressIndicator(),
                        Row(
                          children: [
                            Expanded(
                              child: Padding(
                                padding: const EdgeInsets.symmetric(
                                  vertical: 8,
                                ),
                                child: Text(c.activity),
                              ),
                            ),
                            if (!c.quitting && c.activeId != null)
                              TextButton(
                                onPressed: c.cancel,
                                child: const Text('取消'),
                              ),
                          ],
                        ),
                      ],
                    ),
                  Expanded(
                    child: switch (WorkspacePage.values[page]) {
                      WorkspacePage.overview => OverviewPage(
                        client: c,
                        onNavigate: navigateTo,
                        onOpenMount: (id) => openMount(id),
                      ),
                      WorkspacePage.files => filesPage(),
                      WorkspacePage.transfers => transfersPage(),
                      WorkspacePage.connections => connectionsPage(),
                      WorkspacePage.backup => BackupPage(
                        key: ValueKey(
                          'backup:${c.selected}:${c.profile?['revision']}',
                        ),
                        client: c,
                      ),
                      WorkspacePage.sharing => SharingPage(client: c),
                      WorkspacePage.settings => settingsPage(),
                      WorkspacePage.cfapi => CfapiPage(client: c),
                    },
                  ),
                  if (c.updates.available)
                    ListTile(
                      dense: true,
                      leading: const Icon(Icons.system_update_alt),
                      title: Text('TwoDrive ${c.updates.latest!.version} 可用'),
                      trailing: TextButton(
                        onPressed: c.updates.openRelease,
                        child: const Text('查看更新'),
                      ),
                    ),
                  const SizedBox(height: 10),
                  if (page == WorkspacePage.overview.index)
                    Text(
                      'TwoDrive ${c.status['build']?['core_version'] ?? ''} · Windows CFAPI',
                      style: const TextStyle(
                        fontSize: 12,
                        color: Colors.blueGrey,
                      ),
                    )
                  else if (c.mounts.isNotEmpty)
                    ConstrainedBox(
                      key: const ValueKey('mounted-folders'),
                      constraints: const BoxConstraints(maxHeight: 100),
                      child: SingleChildScrollView(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            for (final mount in c.mounts)
                              Text(
                                '${mount['name']} · ${mount['path']}${mount['failed'] == true ? ' · 需要处理' : ''}',
                                style: const TextStyle(
                                  fontSize: 12,
                                  color: Colors.blueGrey,
                                ),
                              ),
                          ],
                        ),
                      ),
                    )
                  else
                    const Text(
                      '文件浏览 · 关闭窗口后可从云朵托盘打开',
                      style: TextStyle(fontSize: 12, color: Colors.blueGrey),
                    ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget empty(IconData icon, String title, String detail, [Widget? action]) =>
      Center(
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(icon, size: 60, color: Colors.blueGrey.shade300),
              const SizedBox(height: 16),
              Text(
                title,
                style: const TextStyle(
                  fontSize: 20,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 8),
              Text(
                detail,
                textAlign: TextAlign.center,
                style: const TextStyle(color: Colors.blueGrey),
              ),
              if (action != null)
                Padding(padding: const EdgeInsets.only(top: 18), child: action),
            ],
          ),
        ),
      );
  Widget filesPage() {
    if (!_windowHidden && c.needsDirectoryRead && !_directoryReadQueued) {
      _directoryReadQueued = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        _directoryReadQueued = false;
        if (mounted) loadVisibleDirectory();
      });
    }
    if (!c.connected) {
      return empty(
        Icons.cloud_outlined,
        '连接你的文件',
        '选择存储提供者并连接你的账号或设备。',
        OutlinedButton(
          onPressed: () => navigateTo(WorkspacePage.connections.index),
          child: const Text('管理连接'),
        ),
      );
    }
    final entries = c.entries
        .where(
          (e) => (e['name'] as String).toLowerCase().contains(
            filter.toLowerCase(),
          ),
        )
        .toList();
    final canActOnFiles = !c.busy && c.directoryReady;
    final directoryError = c.directoryError;
    return Column(
      children: [
        Row(
          children: [
            IconButton(
              tooltip: '上一级',
              onPressed: c.busy || c.path == '/'
                  ? null
                  : () => c.browse(parentPath(c.path)),
              icon: const Icon(Icons.arrow_upward),
            ),
            Expanded(child: SelectableText(c.path, maxLines: 2)),
            IconButton(
              tooltip: '刷新',
              onPressed: c.busy || c.directoryLoading
                  ? null
                  : () => c.browse(c.path),
              icon: const Icon(Icons.refresh),
            ),
            if (c.canMkdir || c.canUpload) ...[
              if (c.canMkdir)
                IconButton(
                  tooltip: '新建文件夹',
                  onPressed: !canActOnFiles
                      ? null
                      : () async {
                          final identity = c.target;
                          final directory = c.path;
                          final name = await nameDialog('新建文件夹');
                          if (name != null) {
                            await c.mutate('mkdir', {
                              'path': childPath(directory, name),
                              ...identity,
                            });
                          }
                        },
                  icon: const Icon(Icons.create_new_folder_outlined),
                ),
              if (c.canUpload)
                FilledButton.icon(
                  onPressed: canActOnFiles ? upload : null,
                  icon: const Icon(Icons.upload_outlined),
                  label: const Text('上传'),
                ),
            ],
          ],
        ),
        const SizedBox(height: 12),
        TextField(
          decoration: const InputDecoration(
            prefixIcon: Icon(Icons.search),
            hintText: '筛选当前目录',
          ),
          onChanged: (value) => setState(() => filter = value),
        ),
        const SizedBox(height: 12),
        Expanded(
          child: !c.directoryLoaded
              ? directoryError != null
                    ? empty(Icons.error_outline, '无法读取目录', directoryError)
                    : Center(
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            if (c.directoryLoading) ...[
                              const CircularProgressIndicator(),
                              const SizedBox(height: 16),
                            ],
                            const Text('正在加载目录'),
                          ],
                        ),
                      )
              : entries.isEmpty
              ? empty(
                  Icons.folder_open,
                  c.entries.isEmpty ? '当前目录为空' : '没有匹配的文件',
                  c.entries.isNotEmpty ? '请调整筛选条件。' : '',
                )
              : Card(
                  margin: EdgeInsets.zero,
                  color: Colors.white,
                  clipBehavior: Clip.antiAlias,
                  child: ListView.separated(
                    itemCount: entries.length,
                    separatorBuilder: (_, _) => const Divider(height: 1),
                    itemBuilder: (context, index) {
                      final file = entries[index];
                      final directory = file['directory'] == true;
                      final size = file['size'];
                      return ListTile(
                        leading: Icon(
                          directory ? Icons.folder : Icons.description_outlined,
                          color: directory
                              ? const Color(0xffe7b448)
                              : Colors.blueGrey,
                        ),
                        title: Text(file['name'] as String),
                        subtitle: Text(
                          directory
                              ? '文件夹'
                              : size is int
                              ? bytes(size)
                              : '文件',
                        ),
                        onTap: c.busy
                            ? null
                            : directory
                            ? () => c.browse(file['path'] as String)
                            : canActOnFiles
                            ? () => c.viewCloudFile(file)
                            : null,
                        trailing: PopupMenuButton<String>(
                          enabled: canActOnFiles,
                          onSelected: (action) => fileAction(action, file),
                          itemBuilder: (_) => [
                            if (!directory) ...[
                              const PopupMenuItem(
                                value: 'view',
                                child: Text('查看'),
                              ),
                              const PopupMenuItem(
                                value: 'download',
                                child: Text('下载'),
                              ),
                            ],
                            if (c.canRename)
                              const PopupMenuItem(
                                value: 'rename',
                                child: Text('重命名'),
                              ),
                            if (c.canUpload &&
                                !directory &&
                                (file['etag'] as String).isNotEmpty)
                              const PopupMenuItem(
                                value: 'replace',
                                child: Text('替换内容'),
                              ),
                            if (c.canDelete)
                              const PopupMenuItem(
                                value: 'delete',
                                child: Text('删除'),
                              ),
                          ],
                        ),
                      );
                    },
                  ),
                ),
        ),
        if (c.directoryLoaded) ...[
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerLeft,
            child: Text(
              directoryError != null
                  ? c.directoryFromMount
                        ? '刷新失败，正在显示挂载目录。$directoryError'
                        : '刷新失败，正在显示上次读取的目录。$directoryError'
                  : c.directoryLoading
                  ? '${entries.length} 项 · 正在刷新'
                  : '${entries.length} 项',
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(color: Colors.blueGrey),
            ),
          ),
        ],
      ],
    );
  }

  void loadVisibleDirectory() {
    if (!_windowHidden &&
        !panel &&
        page == WorkspacePage.files.index &&
        c.needsDirectoryRead) {
      unawaited(c.browse(c.path));
    }
  }

  Widget transfersPage() {
    final grouped = {
      for (final section in _TransferSection.values) section: <TransferRow>[],
    };
    for (final row in c.transferRows) {
      grouped[_TransferSection.forRow(row)]!.add(row);
    }
    final selected = transferSection ?? _TransferSection.upload;
    final sections = _TransferSection.values
        .where(
          (section) =>
              section == _TransferSection.upload ||
              section == _TransferSection.backup ||
              section == _TransferSection.download ||
              section == selected ||
              grouped[section]!.isNotEmpty,
        )
        .toList();
    final rows = grouped[selected]!;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        ConstrainedBox(
          constraints: BoxConstraints(maxWidth: sections.length * 132),
          child: SegmentedButton<_TransferSection>(
            segments: [
              for (final section in sections)
                ButtonSegment(value: section, label: Text(section.label)),
            ],
            showSelectedIcon: false,
            selected: {selected},
            onSelectionChanged: (selection) => setState(() {
              transferSection = selection.single;
            }),
          ),
        ),
        const SizedBox(height: 14),
        if (c.transferHistoryError case final String message
            when selected == _TransferSection.upload ||
                selected == _TransferSection.download)
          Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: Text(
              message,
              style: const TextStyle(color: Colors.deepOrange),
            ),
          ),
        Expanded(
          child: rows.isEmpty
              ? empty(Icons.swap_vert, '暂无${selected.label}记录', '')
              : transferList(selected, rows),
        ),
      ],
    );
  }

  Widget transferList(_TransferSection section, List<TransferRow> rows) =>
      Align(
        alignment: Alignment.topCenter,
        child: Card(
          margin: EdgeInsets.zero,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(20),
          ),
          clipBehavior: Clip.antiAlias,
          child: Material(
            type: MaterialType.transparency,
            child: ListView.separated(
              key: PageStorageKey(section),
              shrinkWrap: true,
              padding: EdgeInsets.zero,
              itemCount: rows.length,
              separatorBuilder: (_, _) => const Divider(
                height: 1,
                indent: 16,
                endIndent: 16,
                color: _transferInset,
              ),
              itemBuilder: (_, index) => transferRow(rows[index]),
            ),
          ),
        ),
      );

  Widget transferRow(TransferRow row) {
    final info = row.info;
    final task = row.manual;
    final download = info['kind'] == 'download';
    final hasLocalPath = info['local_path'] is String;
    final canReveal = hasLocalPath || c.mountedTransferPath(info) != null;
    final name = info['name']?.toString() ?? '传输任务';
    final state = c.transferState(info);
    final done = info['bytes_done'] as num? ?? 0;
    final total = info['bytes_total'] as num? ?? 0;
    final active = state == '正在备份' || state == '传输中';
    final statusWidth = MediaQuery.textScalerOf(
      context,
    ).scale(112).clamp(112.0, 210.0).toDouble();
    final color = switch (state) {
      '已备份' || '已完成' => _transferSuccess,
      '未完成' || '需要处理' || '结果未确认' => const Color(0xffa74721),
      _ => active ? _transferAccent : _transferMuted,
    };
    return InkWell(
      onTap: () => showTransferDetails(row, name, download: download),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        child: Row(
          children: [
            Container(
              width: 36,
              height: 36,
              decoration: BoxDecoration(
                color: _transferInset,
                borderRadius: BorderRadius.circular(10),
              ),
              child: Icon(
                download ? Icons.south_rounded : Icons.north_rounded,
                size: 18,
                color: active ? _transferAccent : _transferMuted,
              ),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontWeight: FontWeight.w600),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    c.transferLabel(info),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontSize: 12, color: _transferMuted),
                  ),
                ],
              ),
            ),
            const SizedBox(width: 12),
            if (task != null && task.state == '传输中')
              IconButton(
                tooltip: '取消传输',
                onPressed: () => c.cancelTransfer(task.id),
                icon: const Icon(Icons.close, size: 18),
              ),
            if (task != null && task.state == '未完成' && !task.view)
              IconButton(
                tooltip: '重试传输',
                onPressed: c.canRetryTransfer(task)
                    ? () => c.transfer(task.operation, task.args)
                    : null,
                icon: const Icon(Icons.refresh, size: 18),
              ),
            SizedBox(
              width: statusWidth,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Text(
                    state,
                    textAlign: TextAlign.right,
                    style: TextStyle(
                      fontSize: 12,
                      color: color,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  if (total > 0 || done > 0)
                    Text(
                      active && total > 0 && done < total
                          ? '${bytes(done)} / ${bytes(total)}'
                          : bytes(total > 0 ? total : done),
                      textAlign: TextAlign.right,
                      style: const TextStyle(
                        fontSize: 11,
                        color: _transferMuted,
                      ),
                    ),
                  if (c.transferShowsProgress(info)) ...[
                    const SizedBox(height: 4),
                    SizedBox(
                      width: 84,
                      child: LinearProgressIndicator(
                        minHeight: 3,
                        borderRadius: BorderRadius.circular(3),
                        backgroundColor: _transferInset,
                        color: _transferAccent,
                        value: total <= 0
                            ? null
                            : (done / total).clamp(0, 1).toDouble(),
                      ),
                    ),
                  ],
                ],
              ),
            ),
            const SizedBox(width: 10),
            SizedBox(
              width: 40,
              child: canReveal
                  ? IconButton(
                      tooltip: '打开所在位置',
                      onPressed: () =>
                          revealTransferLocation(info, cloud: !hasLocalPath),
                      icon: const Icon(Icons.folder_open_outlined, size: 18),
                    )
                  : null,
            ),
            const Tooltip(
              message: '查看传输详情',
              child: Icon(Icons.chevron_right, size: 18, color: _transferMuted),
            ),
          ],
        ),
      ),
    );
  }

  Future<void> revealTransferLocation(Json info, {required bool cloud}) async {
    final path = cloud ? c.mountedTransferPath(info) : info['local_path'];
    String? message;
    try {
      if (path is! String) {
        message = '该文件所在位置暂不可用';
      } else if (!await c.revealTransferFile(path)) {
        message = '已打开所在文件夹，当前文件管理器无法自动选中文件';
      }
    } catch (_) {
      message = '无法定位文件。文件可能已移动或所在位置暂不可用';
    }
    if (mounted && message != null) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(message)));
    }
  }

  Future<void> showTransferDetails(
    TransferRow row,
    String name, {
    required bool download,
  }) => showDialog<void>(
    context: context,
    builder: (context) {
      final info = row.info;
      final paths = [
        if (info['local_path'] case final String path)
          (download ? '本地目标' : '本地来源', path, false),
        if (info['path'] case final String path)
          (download ? '云端来源' : '云端目标', path, true),
      ];
      return AlertDialog(
        constraints: const BoxConstraints(maxWidth: 600),
        scrollable: true,
        title: SelectableText(name),
        content: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              c.transferLabel(info),
              style: const TextStyle(color: _transferMuted),
            ),
            for (final (label, path, cloud) in paths) ...[
              const SizedBox(height: 12),
              Row(
                children: [
                  Expanded(
                    child: Text(
                      label,
                      style: const TextStyle(fontWeight: FontWeight.w600),
                    ),
                  ),
                  if (!cloud || c.mountedTransferPath(info) != null)
                    IconButton(
                      tooltip: cloud ? '在挂载目录中定位文件' : '定位本地文件',
                      onPressed: () =>
                          revealTransferLocation(info, cloud: cloud),
                      icon: const Icon(Icons.folder_open_outlined, size: 18),
                    ),
                ],
              ),
              const SizedBox(height: 5),
              SelectableText(
                path,
                style: TextStyle(
                  fontSize: 12,
                  fontFamily: DesktopTypography.monospace,
                ),
              ),
            ],
            if (row.manual?.error case final String error) ...[
              const SizedBox(height: 18),
              SelectableText(
                error,
                style: const TextStyle(color: Colors.deepOrange),
              ),
            ],
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('关闭'),
          ),
        ],
      );
    },
  );

  Widget connectionsPage() => ListView(
    children: [
      if (c.connected)
        Padding(
          padding: const EdgeInsets.only(bottom: 16),
          child: Align(
            alignment: Alignment.centerLeft,
            child: OutlinedButton.icon(
              onPressed: c.busy
                  ? null
                  : () async {
                      if (await confirm(
                        '退出登录？',
                        '断开此账号并移除客户端中保存的登录信息。服务器文件、备份源和独立的设备配对状态会保留。',
                      )) {
                        await c.signOut();
                      }
                    },
              icon: const Icon(Icons.logout),
              label: const Text('退出登录'),
            ),
          ),
        ),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton.icon(
          onPressed: c.busy || !c.available ? null : () => editProfile(),
          icon: const Icon(Icons.add),
          label: const Text('添加连接'),
        ),
      ),
      const SizedBox(height: 16),
      if (c.connectedProfiles.length > 1)
        Padding(
          padding: const EdgeInsets.only(bottom: 12),
          child: Text('已连接 ${c.connectedProfiles.length} 个网盘 · 点击卡片切换浏览'),
        ),
      for (final p in c.profiles)
        ConnectionCard(
          key: ValueKey('connection-${p['id']}'),
          mountSupported: c.mountSupported,
          mounted: c.isProfileMounted(p['id'] as String),
          onMount:
              c.busy ||
                  !c.available ||
                  (!c.isProfileMounted(p['id'] as String) &&
                      !c.isProfileConnected(p['id'] as String))
              ? null
              : () =>
                    c.mountDriverMissing &&
                        !c.isProfileMounted(p['id'] as String)
                    ? showMountHelp(context, client: c)
                    : c.mountToggle(p['id'] as String),
          onOpenMount:
              c.busy ||
                  !c.available ||
                  c.mountForProfile(p['id'] as String) == null ||
                  c.mountForProfile(p['id'] as String)?['failed'] == true
              ? null
              : () => openMount(p['id'] as String),
          profile: {
            ...p,
            if (c.connected && c.status['profile'] == p['id'])
              'account_name': c.account,
          },
          online: c.isProfileConnected(p['id'] as String),
          selected: c.connected && c.status['profile'] == p['id'],
          connectLabel: c.isProfileConnected(p['id'] as String)
              ? '查看文件'
              : c.connected
              ? '切换使用'
              : '登录并使用',
          usage: c.quotas[p['id']],
          onSelect: c.busy || !c.available
              ? null
              : () => unawaited(connectProfile(p)),
          onRefresh: c.isProfileConnected(p['id'] as String)
              ? () => unawaited(c.refreshQuota(p['id'] as String, force: true))
              : null,
          onDisconnect: c.busy || !c.available
              ? null
              : () => unawaited(c.disconnect(p['id'] as String)),
          onEdit:
              c.busy || !c.available || c.isProfileConnected(p['id'] as String)
              ? null
              : () => editProfile(p),
          onDelete:
              c.busy || !c.available || c.isProfileConnected(p['id'] as String)
              ? null
              : () async {
                  if (await confirm('删除连接配置？', '服务器文件和已有配对状态会保留。')) {
                    await c.remove(p['id'] as String);
                  }
                },
        ),
      if (c.profiles.isEmpty)
        const Padding(
          padding: EdgeInsets.all(24),
          child: Text('选择存储提供者并填写连接信息。'),
        ),
    ],
  );
  Widget settingsPage() {
    final cache = c.status['cache'] as Json?;
    final network = (c.status['network'] as List?)?.cast<String>() ?? [];
    return ListView(
      children: [
        if (c.status['build'] is Map)
          Card(
            child: ListTile(
              leading: const Icon(Icons.verified_outlined),
              title: Text('共享引擎 ${c.status['build']['core_version']}'),
              subtitle: Text(
                '${c.status['build']['edition']}\n构建 ${c.status['build']['revision']}',
              ),
            ),
          ),
        if (c.status['sync_policy'] is Map) SyncPolicyCard(client: c),
        Card(
          color: Colors.white,
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  'Windows 开发通道 · ${c.updates.current}',
                  style: const TextStyle(
                    fontSize: 18,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                if (c.updates.managed)
                  const Text(
                    '此 MSIX 安装包通过其分发渠道更新。商店安装请在 Microsoft Store 中检查更新。',
                  )
                else ...[
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('自动检查更新'),
                    subtitle: const Text(
                      '每 24 小时向 GitHub 检查 Development 新版本，不自动下载或安装。',
                    ),
                    value: c.updates.enabled,
                    onChanged: c.updates.checking ? null : c.updates.setEnabled,
                  ),
                  if (c.updates.error != null) Text(c.updates.error!),
                  if (c.updates.lastSuccess != null && c.updates.error == null)
                    Text(
                      c.updates.available
                          ? '发现新版本 ${c.updates.latest!.version}'
                          : '本次检查未发现更新版本',
                    ),
                  if (c.updates.latest != null)
                    SelectableText(c.updates.latest!.url),
                  const SizedBox(height: 8),
                  Wrap(
                    spacing: 12,
                    children: [
                      OutlinedButton(
                        onPressed:
                            c.updates.checking || c.updates.current.isEmpty
                            ? null
                            : () => c.updates.check(manual: true),
                        child: Text(c.updates.checking ? '检查中…' : '立即检查'),
                      ),
                      if (c.updates.available)
                        FilledButton.tonal(
                          onPressed: c.updates.openRelease,
                          child: const Text('打开发布页面'),
                        ),
                    ],
                  ),
                ],
              ],
            ),
          ),
        ),
        Card(
          color: Colors.white,
          child: SwitchListTile(
            title: const Text('登录后自动启动'),
            subtitle: Text(
              c.updates.managed
                  ? 'MSIX 自启动尚待适配；请从开始菜单启动。'
                  : '默认关闭。开启后在托盘中运行；连接和挂载遵循下方设置。',
            ),
            value: c.status['autostart'] == true,
            onChanged: c.busy || !c.available || c.updates.managed
                ? null
                : c.autostart,
          ),
        ),
        Card(
          color: Colors.white,
          child: SwitchListTile(
            title: const Text('启动时恢复已登录账号'),
            subtitle: const Text(
              '默认恢复全部已保存登录的账号。“断开”仅对本次运行生效；关闭此项后，下次启动不自动连接，仍可手动连接。',
            ),
            value: c.restoreConnectionsOnStartup,
            onChanged: c.busy ? null : c.setRestoreConnectionsOnStartup,
          ),
        ),
        if (c.canPreflightLocalFile) LocalFilePreflightCard(client: c),
        if (c.canInspectNativeItems) NativeLocalFilesCard(client: c),
        if (c.nativeItemCapabilities['owner_unavailable'] == true)
          const Card(
            child: Padding(
              padding: EdgeInsets.all(20),
              child: Text('本机文件所有者不可用。原生操作已关闭，请保留目录与恢复日志。'),
            ),
          ),
        if (c.mountSupported)
          Card(
            color: Colors.white,
            child: SwitchListTile(
              title: const Text('连接后自动挂载'),
              subtitle: const Text('手动连接或启动恢复成功后自动挂载到文件管理器。关闭不会卸载已有挂载。'),
              value: c.autoMountOnConnect,
              onChanged: c.busy ? null : c.setAutoMountOnConnect,
            ),
          ),
        if (c.mountSupported) ...[
          const Padding(padding: EdgeInsets.all(12), child: Text('文件管理器挂载')),
          if (c.mounts.isEmpty)
            const Padding(
              padding: EdgeInsets.all(12),
              child: Text('尚未挂载网盘，请在“连接”页点击对应网盘的“挂载”。'),
            ),
          for (final mount in c.mounts)
            MountCard(
              key: ValueKey('mount-${mount['profile']}'),
              client: c,
              profileId: mount['profile'] as String,
              onOpen: () => openMount(mount['profile'] as String),
            ),
          if (c.mountDriverMissing)
            TextButton(
              onPressed: () => showMountHelp(context, client: c),
              child: const Text('查看挂载组件安装说明'),
            ),
        ],
        if (c.canMount && !c.readOnlyMount)
          Card(
            color: Colors.white,
            child: ListTile(
              title: Text('挂载缓存 · ${bytes(cache?['bytes'] as num? ?? 0)}'),
              subtitle: Text('待处理或冲突：${cache?['pending'] ?? 0} 项。清理保留尚未上传的数据。'),
              trailing: OutlinedButton(
                onPressed: c.busy || c.selected == null ? null : c.clearCache,
                child: const Text('释放已同步缓存'),
              ),
            ),
          ),
        if (c.canMount && (cache?['deletes_pending'] as num? ?? 0) > 0)
          Card(
            color: Colors.white,
            child: ListTile(
              title: Text('云端删除待确认：${cache?['deletes_pending']} 项'),
              subtitle: Text(
                '本地已删除。被拒绝：${cache?['deletes_rejected'] ?? 0} 项，等待重试：${cache?['deletes_deferred'] ?? 0} 项。任务已保留；修复权限或登录后可重新尝试。',
              ),
              trailing: OutlinedButton(
                onPressed: c.busy || c.selected == null ? null : c.retryDeletes,
                child: const Text('重新尝试'),
              ),
            ),
          ),
        Card(
          color: Colors.white,
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  '网络路径',
                  style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
                ),
                const SizedBox(height: 12),
                Text(switch (c.profile?['network_route']) {
                  'direct' => '仅直连：绑定物理网卡并绕过代理；失败时停止连接。',
                  'system' => '使用系统路由及系统配置的代理。',
                  _ => '优先直连，失败时明确提示并允许系统／代理兜底。',
                }),
                const SizedBox(height: 12),
                for (final line in network)
                  Padding(
                    padding: const EdgeInsets.symmetric(vertical: 3),
                    child: SelectableText(
                      line,
                      style: TextStyle(
                        fontFamily: DesktopTypography.monospace,
                        fontFamilyFallback: DesktopTypography.fallback,
                        fontSize: 12,
                      ),
                    ),
                  ),
                if (network.isEmpty)
                  const Text(
                    '当前没有网络路径报告。',
                    style: TextStyle(color: Colors.blueGrey),
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }

  Future<void> connectProfile(Json profile) async {
    final provider = c.providers
        .where((p) => p['id'] == profile['kind'])
        .firstOrNull;
    final action = provider?['login_action'];
    if (!c.isProfileConnected(profile['id'] as String) &&
        profile['login_required'] == true &&
        action is String) {
      await editProfile(profile, action, true);
    } else {
      await c.connect(profile['id'] as String);
    }
    if (mounted && c.connected && c.status['profile'] == profile['id']) {
      navigateTo(WorkspacePage.files.index);
    }
  }

  Future<void> editProfile([
    Json? original,
    String? initialAction,
    bool connectOnLogin = false,
  ]) async {
    final profile = await showDialog<Json>(
      context: context,
      builder: (_) => ProfileDialog(
        original: original,
        providers: c.providers,
        client: c,
        initialAction: initialAction,
        connectOnLogin: connectOnLogin,
      ),
    );
    if (profile != null) {
      final connect = profile.remove('_connect') == true;
      final id = await c.save(profile);
      if (connect && id != null) await c.connect(id);
    }
  }
}
