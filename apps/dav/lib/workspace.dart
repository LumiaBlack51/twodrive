import 'desktop_typography.dart';
import 'cloud_target_picker.dart';
import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'language.dart';
import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter/services.dart';
import 'package:file_selector/file_selector.dart';
import 'status_tray.dart';
import 'tray_panel.dart';
import 'backup_page.dart';
import 'package:window_manager/window_manager.dart';
import 'client.dart';
import 'profile_dialog.dart';
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

class _WorkspaceState extends State<Workspace>
    with WindowListener, WidgetsBindingObserver {
  StorageClient get c => widget.client;
  final tray = StatusTray();
  bool panel = false;
  bool nativeTrayVisible = false;
  bool dashboardFocused = false;
  DialogRoute<void>? foregroundPanel;
  bool presenting = false;
  bool _windowHidden = false;
  bool _directoryReadQueued = false;
  int page = WorkspacePage.overview.index;
  _TransferSection? transferSection;
  final fileFilter = TextEditingController();
  (String?, Object?)? _filterOwner;
  (String?, Object?) get fileOwner => (c.selected, c.profile?['revision']);
  bool _switchingFileTarget = false;
  String _traySignature = '';
  Future<void> _trayWork = Future.value();
  Future<void>? _quitWork;
  @override
  void initState() {
    super.initState();
    _filterOwner = fileOwner;
    c.addListener(changed);
    if (widget.desktop) {
      WidgetsBinding.instance.addObserver(this);
      windowManager.addListener(this);
      dashboardFocused = c.viewVisible;
      tray.onVisibility = (visible) {
        nativeTrayVisible = visible;
        updateViewInterests();
        if (visible) unawaited(c.refreshView());
      };
      tray.onQuit = () => unawaited(quit());
      tray.onActivate = (position) => unawaited(showPanel(position));
      unawaited(initTray());
    }
  }

  @override
  void didChangeMetrics() {
    if (mounted && Platform.isLinux) setState(() {});
  }

  Future<void> initTray() async {
    try {
      await tray.icon('offline');
      trayReady = true;
      await windowManager.setPreventClose(true);
      await updateTray();
    } catch (_) {
      c.error = tr('系统托盘不可用，关闭窗口将退出客户端。');
      changed();
    }
  }

  void changed() {
    if (_filterOwner != fileOwner) {
      _filterOwner = fileOwner;
      fileFilter.clear();
    }
    if (mounted) {
      setState(() {});
    }
    if (widget.desktop && trayReady) {
      _trayWork = _trayWork.then((_) => updateTray()).catchError((Object _) {});
    }
  }

  Future<void> updateTray() async {
    final icon = c.trayState;
    final title = c.trayTitle;
    final signature = '$icon:$title';
    if (_traySignature == signature) {
      return;
    }
    await tray.icon(icon, title: title);
    _traySignature = signature;
  }

  void updateViewInterests() {
    c.setView(
      visible: dashboardFocused || nativeTrayVisible,
      files: dashboardFocused && !panel && page == WorkspacePage.files.index,
      transfers:
          dashboardFocused && !panel && page == WorkspacePage.transfers.index,
      quota:
          nativeTrayVisible ||
          (dashboardFocused &&
              (panel ||
                  page == WorkspacePage.connections.index ||
                  page == WorkspacePage.overview.index)),
    );
  }

  void refreshWorkspace({
    bool forceDirectory = false,
    bool forceQuota = false,
  }) {
    updateViewInterests();
    unawaited(
      c.refreshView(forceDirectory: forceDirectory, forceQuota: forceQuota),
    );
  }

  void navigateTo(int destination) {
    setState(() => page = destination);
    if (widget.desktop) refreshWorkspace(forceDirectory: true);
  }

  @override
  void onWindowFocus() {
    dashboardFocused = true;
    _windowHidden = false;
    if (!presenting) refreshWorkspace(forceDirectory: true);
  }

  void hideWorkspace() {
    _windowHidden = true;
    dashboardFocused = false;
    updateViewInterests();
  }

  @override
  void onWindowMinimize() => hideWorkspace();

  @override
  void onWindowClose() {
    if (c.quitting) return;
    onWindowMinimize();
    unawaited(windowManager.hide());
  }

  @override
  void onWindowBlur() {
    dashboardFocused = false;
    updateViewInterests();
    if (!Platform.isLinux && !Platform.isWindows && panel && !presenting) {
      unawaited(windowManager.hide());
    }
  }

  Future<void> showPanel(Offset position) async {
    if (presenting) return;
    final existing = foregroundPanel;
    if (existing != null) {
      existing.navigator?.removeRoute(existing);
      return;
    }
    presenting = true;
    try {
      if (!panel &&
          await windowManager.isVisible() &&
          !await windowManager.isMinimized()) {
        if (!mounted || foregroundPanel != null) return;
        final popup = DialogRoute<void>(
          context: context,
          builder: (context) => Dialog(
            alignment: Alignment.bottomRight,
            child: SizedBox(
              width: 390,
              height: 570,
              child: ListenableBuilder(
                listenable: c,
                builder: (_, _) => buildTray(),
              ),
            ),
          ),
        );
        await windowManager.focus();
        if (mounted) {
          foregroundPanel = popup;
          unawaited(
            Navigator.of(context).push(popup).whenComplete(() {
              if (identical(foregroundPanel, popup)) foregroundPanel = null;
            }),
          );
        }
        return;
      }
      if (panel && await windowManager.isVisible()) {
        c.setView(visible: false);
        await windowManager.hide();
        return;
      }
      c.setView(visible: false);
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
      if (mounted) refreshWorkspace();
    } finally {
      presenting = false;
    }
  }

  Future<void> openDashboardFromTray([int? destination]) async {
    if (Platform.isLinux) {
      await tray.panel(false);
      if (!mounted) return;
      if (destination != null) navigateTo(destination);
      await windowManager.show();
      await windowManager.focus();
      return;
    }
    await showManagement(destination);
  }

  Future<void> showManagement([int? destination]) async {
    final popup = foregroundPanel;
    if (popup != null) {
      popup.navigator?.removeRoute(popup);
      if (destination != null) navigateTo(destination);
      await windowManager.focus();
      return;
    }
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
    await openDashboardFromTray(2);
    if (await confirm(
      tr('退出登录？'),
      tr('断开此账号并移除客户端中保存的登录信息。服务器文件、备份源和独立的设备配对状态会保留。'),
    )) {
      await c.signOut();
    }
  }

  Future<void> quit() => _quitWork ??= _quitOnce();

  Future<void> _quitOnce() async {
    try {
      while (mounted) {
        if (widget.desktop) {
          hideWorkspace();
          try {
            await windowManager.hide();
          } catch (_) {}
        }
        if (await c.shutdown()) break;
        if (widget.desktop) {
          await windowManager.show();
          _windowHidden = false;
          await windowManager.focus();
        }
        if (!mounted || c.quitBlockers.isEmpty) return;
        if (!await _confirmQuitWithOpenMounts()) return;
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

  Future<bool> _confirmQuitWithOpenMounts() async =>
      await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          scrollable: true,
          title: const Text('挂载目录正在使用'),
          content: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('请保存工作，正常关闭仍在使用这些目录的文件、窗口或终端，再重试退出。'),
              for (final blocker in c.quitBlockers) ...[
                const SizedBox(height: 12),
                Text(blocker['name'] as String),
                Text(blocker['path'] as String),
                for (final program
                    in (blocker['programs'] as List).cast<Json>())
                  Text(
                    '${program['name']}（PID ${program['pid']}）'
                    ' · 请手动关闭或离开挂载目录',
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
              child: const Text('重试退出'),
            ),
          ],
        ),
      ) ??
      false;

  @override
  void dispose() {
    c.removeListener(changed);
    fileFilter.dispose();
    if (widget.desktop) {
      WidgetsBinding.instance.removeObserver(this);
      windowManager.removeListener(this);
      tray.onVisibility = null;
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
                labelText: tr('名称'),
                errorText: validation,
              ),
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: Text(tr('取消')),
            ),
            FilledButton(
              onPressed: () {
                final value = text.text.trim();
                if (value.isEmpty ||
                    value == '.' ||
                    value == '..' ||
                    value.contains(RegExp(r'[/\\\x00]'))) {
                  update(() => validation = tr('请输入有效的单个文件名'));
                  return;
                }
                Navigator.pop(context, value);
              },
              child: Text(tr('确定')),
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
              child: Text(tr('取消')),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(tr('确认')),
            ),
          ],
        ),
      ) ??
      false;
  Future<void> upload() async {
    final identity = c.target;
    final directory = c.path;
    final files = await openFiles(confirmButtonText: tr('上传'));
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
      confirmButtonText: tr('下载'),
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
        if (c.isProfileMounted(file['profile'] as String)) {
          await c.viewCloudFile(file);
        } else {
          await download(file);
        }
      case 'download':
        await download(file);
      case 'rename':
        final name = await nameDialog(tr('重命名'), file['name'] as String);
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
          tr('删除 {arg0}？', [file['name']]),
          file['directory'] == true ? tr('文件夹及其内容将从服务器删除。') : tr('此文件将从服务器删除。'),
        )) {
          await c.mutate('delete', {
            'path': file['path'],
            'profile': file['profile'],
            'revision': file['revision'],
          });
        }
      case 'replace':
        final source = await openFile(confirmButtonText: tr('选择新内容'));
        if (source != null &&
            await confirm(
              tr('替换 {arg0}？', [file['name']]),
              tr('仅在服务器版本未发生变化时覆盖。'),
            )) {
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

  Widget buildTray() => TrayPanel(
    client: c,
    onManage: openDashboardFromTray,
    onRefresh: () => refreshWorkspace(forceQuota: true),
    onOpenFolder: () async {
      if (!c.canMount) {
        await openDashboardFromTray(0);
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

  @override
  Widget build(BuildContext context) {
    if (panel) return buildTray();
    final workspace = buildWorkspace();
    if (!widget.desktop || !Platform.isLinux) return workspace;
    final primary = View.of(context);
    final secondary = WidgetsBinding.instance.platformDispatcher.views
        .where((view) => view.viewId != primary.viewId)
        .firstOrNull;
    return ViewAnchor(
      view: secondary == null
          ? null
          : View(
              view: secondary,
              child: MaterialApp(
                locale: Localizations.localeOf(context),
                supportedLocales: const [Locale('zh'), Locale('en')],
                localizationsDelegates: GlobalMaterialLocalizations.delegates,
                debugShowCheckedModeBanner: false,
                theme: Theme.of(context),
                home: buildTray(),
              ),
            ),
      child: workspace,
    );
  }

  Widget buildWorkspace() {
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
                          c.status['version'] is String
                              ? 'Development · ${c.status['version']}'
                              : tr('文件与同步'),
                          textAlign: TextAlign.center,
                          style: TextStyle(color: Colors.blueGrey),
                        ),
                      ),
                      for (final destination in WorkspacePage.sidebar)
                        Container(
                          padding: const EdgeInsets.symmetric(vertical: 2),
                          margin: const EdgeInsets.symmetric(horizontal: 8),
                          decoration: BoxDecoration(
                            color: page == destination.index
                                ? const Color(0xffd2dbef)
                                : null,
                            borderRadius: BorderRadius.circular(12),
                          ),
                          child: ListTile(
                            dense: true,
                            contentPadding: const EdgeInsets.symmetric(
                              horizontal: 8,
                            ),
                            minLeadingWidth: 24,
                            horizontalTitleGap: 10,
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
                        ? tr('● {arg0} 个账号已连接', [c.connectedProfiles.length])
                        : tr('○ 未连接'),
                    style: TextStyle(
                      color: c.connected ? Colors.teal : Colors.blueGrey,
                    ),
                  ),
                ),
                if (widget.desktop)
                  TextButton.icon(
                    onPressed: c.quitting ? null : quit,
                    icon: const Icon(Icons.power_settings_new, size: 18),
                    label: Text(c.quitting ? tr('正在退出…') : tr('退出客户端')),
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
                          tooltip: tr('刷新状态'),
                          onPressed: !c.available || c.busy || c.quitting
                              ? null
                              : () => refreshWorkspace(),
                          icon: const Icon(Icons.refresh),
                        ),
                      const SizedBox(width: 12),
                      if (page == WorkspacePage.files.index)
                        FilledButton.tonalIcon(
                          onPressed:
                              c.busy || c.selected == null || !c.available
                              ? null
                              : c.connected
                              ? c.disconnect
                              : c.connect,
                          icon: Icon(c.connected ? Icons.link_off : Icons.link),
                          label: Text(c.connected ? tr('断开') : tr('连接')),
                        ),
                    ],
                  ),
                  if (page == WorkspacePage.files.index && c.connected)
                    Text(
                      '${c.profile?['name'] ?? c.providerLabel} · ${c.account}',
                    ),
                  const SizedBox(height: 12),
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
                            tooltip: tr('关闭提示'),
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
                                child: Text(tr('取消')),
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
                      WorkspacePage.settings => settingsPage(),
                      WorkspacePage.cfapi => CfapiPage(client: c),
                    },
                  ),
                  if (c.updates.available)
                    ListTile(
                      dense: true,
                      leading: const Icon(Icons.system_update_alt),
                      title: Text(
                        tr('TwoDrive {arg0} 可用', [c.updates.latest!.version]),
                      ),
                      trailing: TextButton(
                        onPressed: c.updates.openRelease,
                        child: Text(tr('查看更新')),
                      ),
                    ),
                  const SizedBox(height: 10),
                  if (c.mounts.isNotEmpty)
                    ConstrainedBox(
                      key: const ValueKey('mounted-folders'),
                      constraints: const BoxConstraints(maxHeight: 100),
                      child: SingleChildScrollView(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            for (final mount in c.mounts)
                              Text(
                                '${mount['name']} · ${mount['path']}${mount['failed'] == true ? tr(' · 需要处理') : ''}',
                                style: const TextStyle(
                                  fontSize: 12,
                                  color: Colors.blueGrey,
                                ),
                              ),
                          ],
                        ),
                      ),
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
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.all(16),
          child: CloudTargetPicker(
            fieldKey: const ValueKey('files-target'),
            label: tr('查看网盘'),
            client: c,
            onSelect:
                c.available && !c.busy && !c.quitting && !_switchingFileTarget
                ? (profile) => unawaited(selectFileTarget(profile))
                : null,
          ),
        ),
        const SizedBox(height: 16),
        Expanded(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: _switchingFileTarget
                ? const Center(child: CircularProgressIndicator())
                : c.connected
                ? fileDirectory()
                : empty(
                    Icons.cloud_outlined,
                    '连接你的文件',
                    c.profiles.isEmpty
                        ? '前往连接页选择存储提供者，再添加账号或设备。'
                        : '选择上方的网盘，或前往连接页添加新账号。',
                    OutlinedButton(
                      onPressed: () =>
                          navigateTo(WorkspacePage.connections.index),
                      child: Text(tr('管理连接')),
                    ),
                  ),
          ),
        ),
      ],
    );
  }

  Future<void> selectFileTarget(Json profile) async {
    if (_switchingFileTarget ||
        !c.available ||
        c.busy ||
        c.quitting ||
        (c.connected && c.selected == profile['id'])) {
      return;
    }
    setState(() => _switchingFileTarget = true);
    try {
      await connectProfile(profile);
    } finally {
      if (mounted) setState(() => _switchingFileTarget = false);
    }
  }

  Widget fileDirectory() {
    final entries = c.entries
        .where(
          (e) => (e['name'] as String).toLowerCase().contains(
            fileFilter.text.toLowerCase(),
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
              tooltip: tr('上一级'),
              onPressed: c.busy || c.path == '/'
                  ? null
                  : () => c.browse(parentPath(c.path)),
              icon: const Icon(Icons.arrow_upward),
            ),
            Expanded(child: SelectableText(c.path, maxLines: 2)),
            IconButton(
              tooltip: tr('刷新'),
              onPressed: c.busy || c.directoryLoading
                  ? null
                  : () => c.browse(c.path),
              icon: const Icon(Icons.refresh),
            ),
            if (c.canMkdir || c.canUpload) ...[
              if (c.canMkdir)
                IconButton(
                  tooltip: tr('新建文件夹'),
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
                  label: Text(tr('上传')),
                ),
            ],
          ],
        ),
        const SizedBox(height: 12),
        TextField(
          controller: fileFilter,
          decoration: InputDecoration(
            prefixIcon: Icon(Icons.search),
            hintText: tr('筛选当前目录'),
          ),
          onChanged: (_) => setState(() {}),
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
                            Text(tr('正在加载目录')),
                          ],
                        ),
                      )
              : entries.isEmpty
              ? empty(
                  Icons.folder_open,
                  c.entries.isEmpty ? '当前目录为空' : '没有匹配的文件',
                  c.entries.isNotEmpty ? '请调整筛选条件。' : '',
                )
              : ListView.separated(
                  key: ValueKey((fileOwner, c.path)),
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
                      dense: true,
                      contentPadding: const EdgeInsets.symmetric(
                        horizontal: 10,
                      ),
                      title: Tooltip(
                        message: file['name'] as String,
                        child: Text(
                          file['name'] as String,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
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
                          ? () => download(file)
                          : null,
                      trailing: PopupMenuButton<String>(
                        enabled: canActOnFiles,
                        onSelected: (action) => fileAction(action, file),
                        itemBuilder: (_) => [
                          if (!directory)
                            PopupMenuItem(value: 'view', child: Text(tr('查看'))),
                          if (!directory)
                            PopupMenuItem(
                              value: 'download',
                              child: Text(tr('下载')),
                            ),
                          if (c.canRename)
                            PopupMenuItem(
                              value: 'rename',
                              child: Text(tr('重命名')),
                            ),
                          if (c.canUpload &&
                              !directory &&
                              (file['etag'] as String?)?.isNotEmpty == true)
                            PopupMenuItem(
                              value: 'replace',
                              child: Text(tr('替换内容')),
                            ),
                          if (c.canDelete)
                            PopupMenuItem(
                              value: 'delete',
                              child: Text(tr('删除')),
                            ),
                        ],
                      ),
                    );
                  },
                ),
        ),
        if (c.directoryLoaded) ...[
          const SizedBox(height: 8),
          Align(
            alignment: Alignment.centerLeft,
            child: Text(
              directoryError != null
                  ? '刷新失败，正在显示上次读取的目录。$directoryError'
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
            if (task != null && task.phase == TransferPhase.running)
              IconButton(
                tooltip: '取消传输',
                onPressed: () => c.cancelTransfer(task.id),
                icon: const Icon(Icons.close, size: 18),
              ),
            if (task != null &&
                task.phase == TransferPhase.failed &&
                !task.view)
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
                        tr('退出登录？'),
                        tr('断开此账号并移除客户端中保存的登录信息。服务器文件、备份源和独立的设备配对状态会保留。'),
                      )) {
                        await c.signOut();
                      }
                    },
              icon: const Icon(Icons.logout),
              label: Text(tr('退出登录')),
            ),
          ),
        ),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton.icon(
          onPressed: c.busy || !c.available ? null : () => editProfile(),
          icon: const Icon(Icons.add),
          label: Text(tr('添加连接')),
        ),
      ),
      const SizedBox(height: 16),
      if (c.connectedProfiles.length > 1)
        Padding(
          padding: const EdgeInsets.only(bottom: 12),
          child: Text(
            tr('已连接 {arg0} 个网盘 · 点击卡片切换浏览', [c.connectedProfiles.length]),
          ),
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
          issue: c.connectionErrors[p['id']],
          connectLabel: c.needsAuthorization.contains(p['id'])
              ? tr('重新授权')
              : c.isProfileConnected(p['id'] as String)
              ? tr('查看文件')
              : c.connected
              ? tr('切换使用')
              : tr('登录并使用'),
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
                  if (await confirm(tr('删除连接配置？'), tr('服务器文件和已有配对状态会保留。'))) {
                    await c.remove(p['id'] as String);
                  }
                },
        ),
      if (defaultTargetPlatform == TargetPlatform.windows &&
          c.status['cfapi']?['supported'] == true &&
          c.profiles.any((p) => p['kind'] == 'onedrive'))
        ListTile(
          title: Text(tr('OneDrive 按需同步')),
          onTap: () => navigateTo(WorkspacePage.cfapi.index),
        ),
      if (c.profiles.isEmpty)
        Padding(
          padding: EdgeInsets.all(24),
          child: Text(tr('选择存储提供者并填写连接信息。')),
        ),
    ],
  );
  Widget settingsPage() {
    final cache = c.status['cache'] as Json?;
    return ListView(
      children: [
        if (c.status['version'] is String)
          Card(
            child: ListTile(
              leading: const Icon(Icons.verified_outlined),
              title: Text(
                'TwoDrive Development ${c.status['version']} · ${Platform.operatingSystem == 'windows'
                    ? 'Windows'
                    : Platform.operatingSystem == 'linux'
                    ? 'Linux'
                    : Platform.operatingSystem}',
              ),
            ),
          ),
        ListTile(
          title: Text(tr('语言')),
          trailing: DropdownButton<String>(
            value: c.preferences.language,
            items: const [
              DropdownMenuItem(value: 'zh', child: Text('简体中文')),
              DropdownMenuItem(value: 'en', child: Text('English')),
            ],
            onChanged: (value) {
              if (value != null) c.setLanguage(value);
            },
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
                  tr('软件更新'),
                  style: const TextStyle(
                    fontSize: 18,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                if (c.updates.managed)
                  Text(tr('请通过安装渠道检查更新。'))
                else ...[
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: Text(tr('自动检查更新')),
                    subtitle: Text(tr('每天检查一次')),
                    value: c.updates.enabled,
                    onChanged: c.updates.checking ? null : c.updates.setEnabled,
                  ),
                  if (c.updates.error != null) Text(c.updates.error!),
                  if (c.updates.lastSuccess != null && c.updates.error == null)
                    Text(
                      c.updates.available
                          ? tr('发现新版本 {arg0}', [c.updates.latest!.version])
                          : tr('本次检查未发现更新版本'),
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
                        child: Text(
                          c.updates.checking ? tr('检查中…') : tr('立即检查'),
                        ),
                      ),
                      if (c.updates.available)
                        FilledButton.tonal(
                          onPressed: c.updates.openRelease,
                          child: Text(tr('打开发布页面')),
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
            title: Text(tr('登录后自动启动')),
            subtitle: Text(
              c.updates.managed ? tr('请从开始菜单启动。') : tr('启动时显示托盘图标'),
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
            title: Text(tr('启动时恢复已登录账号')),
            value: c.restoreConnectionsOnStartup,
            onChanged: c.busy ? null : c.setRestoreConnectionsOnStartup,
          ),
        ),
        if (c.mountSupported)
          Card(
            color: Colors.white,
            child: SwitchListTile(
              title: Text(tr('连接后自动挂载')),
              value: c.autoMountOnConnect,
              onChanged: c.busy ? null : c.setAutoMountOnConnect,
            ),
          ),
        if (c.mountSupported) ...[
          Padding(padding: EdgeInsets.all(12), child: Text(tr('文件管理器挂载'))),
          if (c.mounts.isEmpty)
            Padding(
              padding: EdgeInsets.all(12),
              child: Text(tr('尚未挂载网盘，请在“连接”页点击对应网盘的“挂载”。')),
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
              child: Text(tr('查看挂载组件安装说明')),
            ),
        ],
        if (c.canMount && !c.readOnlyMount)
          Card(
            color: Colors.white,
            child: ListTile(
              title: Text(
                tr('挂载缓存 · {arg0}', [bytes(cache?['bytes'] as num? ?? 0)]),
              ),
              subtitle: Text(
                tr('待处理或冲突：{arg0} 项。清理保留尚未上传的数据。', [cache?['pending'] ?? 0]),
              ),
              trailing: OutlinedButton(
                onPressed: c.busy || c.selected == null ? null : c.clearCache,
                child: Text(tr('释放已同步缓存')),
              ),
            ),
          ),
        if (c.canMount && (cache?['deletes_pending'] as num? ?? 0) > 0)
          Card(
            color: Colors.white,
            child: ListTile(
              title: Text(tr('云端删除待确认：{arg0} 项', [cache?['deletes_pending']])),
              subtitle: Text(
                tr('本地已删除。被拒绝：{arg0} 项，等待重试：{arg1} 项。任务已保留；修复权限或登录后可重新尝试。', [
                  cache?['deletes_rejected'] ?? 0,
                  cache?['deletes_deferred'] ?? 0,
                ]),
              ),
              trailing: OutlinedButton(
                onPressed: c.busy || c.selected == null ? null : c.retryDeletes,
                child: Text(tr('重新尝试')),
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
