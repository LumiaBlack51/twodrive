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
  int page = 0;
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

  @override
  void onWindowFocus() => unawaited(c.pollNow(forceDirectory: true));

  @override
  void onWindowClose() {
    if (c.quitting) return;
    unawaited(windowManager.hide());
  }

  @override
  void onWindowBlur() {
    if (!Platform.isLinux && !Platform.isWindows && panel && !presenting) {
      unawaited(windowManager.hide());
    }
  }

  Future<void> showPanel(Offset position) async {
    if (presenting) return;
    if (panel && await windowManager.isVisible()) {
      await windowManager.hide();
      return;
    }
    presenting = true;
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
  }

  Future<void> showManagement([int destination = 0]) async {
    if (presenting) return;
    presenting = true;
    final fromPanel = panel;
    var veiled = false;
    try {
      // The tray and management page share a native surface. Never expose
      // decoration/geometry changes with the old tray frame still visible.
      await windowManager.hide();
      if (!mounted) return;
      setState(() {
        panel = false;
        page = destination;
      });
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
      if (widget.desktop) {
        // Keep the visible window independent of mount/pipe cleanup latency.
        // A refused shutdown must restore it instead of hiding unsaved writes.
        try {
          await windowManager.hide();
        } catch (_) {}
      }
      if (await c.shutdown()) {
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
      } else if (widget.desktop) {
        // Preserve the current page, window size and panel mode on refusal.
        await windowManager.show();
        await windowManager.focus();
      }
    } finally {
      _quitWork = null;
    }
  }

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
    final titles = ['文件', '传输', '连接', '备份', '共享', '设置'];
    return Scaffold(
      body: Row(
        children: [
          Container(
            width: 190,
            color: Colors.white,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Expanded(
                  child: ListView(
                    key: const ValueKey('navigation'),
                    children: [
                      const Padding(
                        padding: EdgeInsets.fromLTRB(22, 16, 20, 8),
                        child: Icon(
                          Icons.cloud_outlined,
                          size: 32,
                          color: Color(0xff326bd6),
                        ),
                      ),
                      const Text(
                        'TwoDrive',
                        textAlign: TextAlign.center,
                        style: TextStyle(
                          fontSize: 23,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const Padding(
                        padding: EdgeInsets.only(bottom: 12, top: 3),
                        child: Text(
                          '文件与同步',
                          textAlign: TextAlign.center,
                          style: TextStyle(color: Colors.blueGrey),
                        ),
                      ),
                      for (var i = 0; i < titles.length; i++)
                        Padding(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 12,
                            vertical: 2,
                          ),
                          child: ListTile(
                            dense: true,
                            selected: page == i,
                            selectedTileColor: const Color(0xffedf3ff),
                            shape: RoundedRectangleBorder(
                              borderRadius: BorderRadius.circular(12),
                            ),
                            leading: Icon(
                              [
                                Icons.folder_outlined,
                                Icons.swap_vert,
                                Icons.dns_outlined,
                                Icons.backup_outlined,
                                Icons.share_outlined,
                                Icons.tune,
                              ][i],
                            ),
                            title: Text(titles[i]),
                            onTap: () => setState(() => page = i),
                          ),
                        ),
                    ],
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.all(20),
                  child: Text(
                    c.connected ? '● 已连接' : '○ 未连接',
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
                        titles[page],
                        style: const TextStyle(
                          fontSize: 28,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const Spacer(),
                      const SizedBox(width: 12),
                      FilledButton.tonalIcon(
                        onPressed: c.busy || c.selected == null || !c.available
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
                      c.connected
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
                    child: switch (page) {
                      0 => filesPage(),
                      1 => transfersPage(),
                      2 => connectionsPage(),
                      3 => BackupPage(key: ValueKey(c.selected), client: c),
                      4 => SharingPage(client: c),
                      _ => settingsPage(),
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
    if (!c.connected) {
      return empty(
        Icons.cloud_outlined,
        '连接你的文件',
        '选择存储提供者并连接你的账号或设备。',
        OutlinedButton(
          onPressed: () => setState(() => page = 2),
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
              onPressed: c.busy ? null : () => c.browse(c.path),
              icon: const Icon(Icons.refresh),
            ),
            if (c.canMkdir || c.canUpload) ...[
              if (c.canMkdir)
                IconButton(
                  tooltip: '新建文件夹',
                  onPressed: c.busy
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
                  onPressed: c.busy ? null : upload,
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
          child: entries.isEmpty
              ? empty(
                  Icons.folder_open,
                  '当前目录为空',
                  filter.isEmpty ? '上传文件或新建文件夹即可开始。' : '没有匹配的文件。',
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
                      return ListTile(
                        leading: Icon(
                          directory ? Icons.folder : Icons.description_outlined,
                          color: directory
                              ? const Color(0xffe7b448)
                              : Colors.blueGrey,
                        ),
                        title: Text(file['name'] as String),
                        subtitle: Text(
                          directory ? '文件夹' : bytes(file['size'] as int),
                        ),
                        onTap: c.busy
                            ? null
                            : directory
                            ? () => c.browse(file['path'] as String)
                            : () => download(file),
                        trailing: PopupMenuButton<String>(
                          enabled: !c.busy,
                          onSelected: (action) => fileAction(action, file),
                          itemBuilder: (_) => [
                            if (!directory)
                              const PopupMenuItem(
                                value: 'download',
                                child: Text('下载'),
                              ),
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
        const SizedBox(height: 8),
        Align(
          alignment: Alignment.centerLeft,
          child: Text(
            '${entries.length} 项',
            style: const TextStyle(color: Colors.blueGrey),
          ),
        ),
      ],
    );
  }

  Widget transfersPage() => c.transfers.isEmpty
      ? empty(Icons.swap_vert, '暂无传输', '上传和下载的进度、结果与重试会显示在这里。')
      : ListView.builder(
          itemCount: c.transfers.length,
          itemBuilder: (context, index) {
            final task = c.transfers[index];
            return Card(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Icon(
                          task.operation == 'upload'
                              ? Icons.upload
                              : Icons.download,
                        ),
                        const SizedBox(width: 12),
                        Expanded(child: Text(task.name)),
                        Text(task.state),
                        if (task.state == '传输中')
                          IconButton(
                            tooltip: '取消传输',
                            onPressed: () => c.cancelTransfer(task.id),
                            icon: const Icon(Icons.close),
                          ),
                        if (task.state == '未完成')
                          TextButton(
                            onPressed:
                                c.busy ||
                                    !c.isProfileConnected(
                                      task.args['profile'] as String,
                                    )
                                ? null
                                : () => c.transfer(task.operation, task.args),
                            child: const Text('重试'),
                          ),
                      ],
                    ),
                    const SizedBox(height: 10),
                    LinearProgressIndicator(
                      value: task.state == '已完成'
                          ? 1
                          : task.total == 0
                          ? null
                          : task.done / task.total,
                    ),
                    const SizedBox(height: 8),
                    Text(
                      c.profiles
                                  .where((p) => p['id'] == task.args['profile'])
                                  .firstOrNull?['name']
                              as String? ??
                          '原连接',
                      style: const TextStyle(
                        fontSize: 12,
                        color: Colors.blueGrey,
                      ),
                    ),
                    Text('${bytes(task.done)} / ${bytes(task.total)}'),
                    if (task.error != null)
                      SelectableText(
                        task.error!,
                        style: const TextStyle(color: Colors.deepOrange),
                      ),
                  ],
                ),
              ),
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
        Card(
          color: Colors.white,
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  '软件更新 · ${c.updates.current}',
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
                  : '默认关闭。开启后在托盘中运行并连接上次使用的服务器。',
            ),
            value: c.status['autostart'] == true,
            onChanged: c.busy || !c.available || c.updates.managed
                ? null
                : c.autostart,
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
        if (Platform.isLinux && c.mountSupported)
          Card(
            color: Colors.white,
            child: SwitchListTile(
              title: const Text('连接后自动挂载'),
              subtitle: const Text('账户连接成功后自动挂载到文件管理器。'),
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
      setState(() => page = 0);
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
