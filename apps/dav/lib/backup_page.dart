import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'client.dart';

class BackupPage extends StatefulWidget {
  const BackupPage({super.key, required this.client});
  final StorageClient client;
  @override
  State<BackupPage> createState() => _BackupPageState();
}

class _BackupPageState extends State<BackupPage> {
  List<Json> folders = [];
  bool loading = true;
  bool selecting = false;
  bool adding = false;
  String? failure;
  String? ownerId;
  String? ownerRevision;
  StorageClient get c => widget.client;
  bool get currentOwner =>
      ownerId != null &&
      c.selected == ownerId &&
      c.profile?['revision'] == ownerRevision;
  bool get editable =>
      currentOwner &&
      c.available &&
      !c.quitting &&
      !c.busy &&
      !loading &&
      !selecting &&
      failure == null &&
      c.canBackup;

  String label(Json p) {
    final name = p['name'] as String? ?? p['provider_label'] as String? ?? '网盘';
    final account = p['account_name'] as String?;
    return account == null || account.isEmpty || account == name
        ? name
        : '$name · $account';
  }

  @override
  void initState() {
    super.initState();
    ownerId = c.selected;
    ownerRevision = c.profile?['revision'] as String?;
    load();
  }

  Future<void> load() async {
    setState(() {
      loading = true;
      failure = null;
    });
    try {
      if (ownerId != null) {
        final result = await c.request('backups', {
          'id': ownerId,
          'revision': ownerRevision,
        });
        if (!mounted || !currentOwner) return;
        folders = (result['folders'] as List).cast<Json>();
      }
    } catch (e) {
      if (!mounted || !currentOwner) return;
      failure = '$e';
    }
    if (mounted) {
      setState(() => loading = false);
    }
  }

  Future<void> selectTarget(String? id) async {
    if (id == null ||
        id == c.selected ||
        selecting ||
        adding ||
        c.busy ||
        !c.available ||
        c.quitting ||
        !c.profiles.any(
          (p) => p['id'] == id && p['capabilities']?['backup'] == true,
        )) {
      return;
    }
    setState(() => selecting = true);
    try {
      await c.connect(id);
    } finally {
      if (mounted) setState(() => selecting = false);
    }
  }

  Future<void> save(List<Json> next) async {
    if (!editable) return;
    await c.action('保存备份文件夹', () async {
      if (!currentOwner) throw StateError('备份目标已改变，请重新选择');
      final result = await c.request('save_backups', {
        'id': ownerId,
        'revision': ownerRevision,
        'folders': next,
      });
      if (mounted && currentOwner) {
        setState(() => folders = (result['folders'] as List).cast<Json>());
      }
      await c.refreshStatus();
    });
  }

  Future<void> add() async {
    if (!editable || adding) return;
    final target = label(c.profile!);
    setState(() => adding = true);
    try {
      final local = await getDirectoryPath(confirmButtonText: '选择备份文件夹');
      if (local == null || !mounted || !currentOwner) return;
      final remote = await showDialog<String>(
        context: context,
        builder: (_) => _BackupDestinationDialog(local: local, target: target),
      );
      if (remote != null && mounted && currentOwner) {
        await save([
          ...folders,
          {'local': local, 'remote': remote, 'enabled': true},
        ]);
      }
    } finally {
      if (mounted) setState(() => adding = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListView(
    children: [
      const Text(
        '备份文件夹',
        style: TextStyle(fontSize: 22, fontWeight: FontWeight.w600),
      ),
      const SizedBox(height: 10),
      const Text('选择需要自动备份的本地文件夹。原文件保留在本机；服务器上的已有版本受到冲突保护。'),
      const SizedBox(height: 20),
      InputDecorator(
        decoration: const InputDecoration(labelText: '备份到'),
        child: DropdownButtonHideUnderline(
          child: DropdownButton<String>(
            key: const ValueKey('backup-target'),
            isExpanded: true,
            isDense: true,
            value: c.profiles.any((p) => p['id'] == c.selected)
                ? c.selected
                : null,
            hint: const Text('请选择备份网盘'),
            items: [
              for (final p in c.profiles)
                DropdownMenuItem(
                  value: p['id'] as String,
                  enabled: p['capabilities']?['backup'] == true,
                  child: Text(
                    p['capabilities']?['backup'] == true
                        ? label(p)
                        : '${label(p)}（暂不支持自动备份）',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
            ],
            onChanged:
                c.available && !c.busy && !c.quitting && !selecting && !adding
                ? selectTarget
                : null,
          ),
        ),
      ),
      const SizedBox(height: 8),
      const Text(
        '备份任务按网盘分别保存。切换网盘不会迁移已有任务。',
        style: TextStyle(color: Colors.blueGrey, fontSize: 12),
      ),
      if (c.profiles.isEmpty)
        const Padding(
          padding: EdgeInsets.only(top: 12),
          child: Text('请先在“连接”页面添加网盘。'),
        ),
      const SizedBox(height: 18),
      if (failure != null)
        Row(
          children: [
            Expanded(
              child: Text(
                failure!,
                style: const TextStyle(color: Colors.deepOrange),
              ),
            ),
            TextButton(
              onPressed: c.available && !c.busy ? load : null,
              child: const Text('重新加载'),
            ),
          ],
        ),
      if (loading || selecting) const LinearProgressIndicator(),
      for (var i = 0; i < folders.length; i++)
        Card(
          child: ListTile(
            leading: const Icon(Icons.folder_outlined),
            title: Text((folders[i]['local'] as String).split('/').last),
            subtitle: Text('${folders[i]['local']} → ${folders[i]['remote']}'),
            trailing: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Switch(
                  value: folders[i]['enabled'] == true,
                  onChanged: !editable || adding
                      ? null
                      : (value) => save([
                          for (var j = 0; j < folders.length; j++)
                            if (i == j)
                              {...folders[j], 'enabled': value}
                            else
                              folders[j],
                        ]),
                ),
                IconButton(
                  tooltip: '停止备份，保留两端文件',
                  onPressed: !editable || adding
                      ? null
                      : () => save([
                          for (var j = 0; j < folders.length; j++)
                            if (i != j) folders[j],
                        ]),
                  icon: const Icon(Icons.remove_circle_outline),
                ),
              ],
            ),
          ),
        ),
      const SizedBox(height: 12),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton.icon(
          onPressed: editable && !adding ? add : null,
          icon: const Icon(Icons.add),
          label: const Text('选择备份文件夹'),
        ),
      ),
      if (c.selected != null && !c.canBackup)
        const Padding(
          padding: EdgeInsets.only(top: 12),
          child: Text('当前网盘暂不支持自动备份，请选择其他网盘。'),
        ),
      const SizedBox(height: 24),
      const Text(
        '符号链接不会上传。暂停同步会暂停文件夹备份与挂载的新网络请求。',
        style: TextStyle(color: Colors.blueGrey, fontSize: 12),
      ),
    ],
  );
}

class _BackupDestinationDialog extends StatefulWidget {
  const _BackupDestinationDialog({required this.local, required this.target});
  final String local;
  final String target;
  @override
  State<_BackupDestinationDialog> createState() =>
      _BackupDestinationDialogState();
}

class _BackupDestinationDialogState extends State<_BackupDestinationDialog> {
  late final controller = TextEditingController(
    text:
        '/Backups/${widget.local.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty).lastOrNull ?? '备份文件夹'}',
  );

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('选择备份目标'),
    content: SizedBox(
      width: 440,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '备份到：${widget.target}',
              style: const TextStyle(fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 12),
            Text(widget.local),
            const SizedBox(height: 16),
            TextField(
              controller: controller,
              decoration: const InputDecoration(labelText: '网盘内的备份目录'),
            ),
            const SizedBox(height: 12),
            const Text(
              '自动上传新建和修改的文件。停止备份或删除本地文件，不会删除网盘副本。',
              style: TextStyle(fontSize: 12, color: Colors.blueGrey),
            ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, controller.text.trim()),
        child: const Text('开始备份'),
      ),
    ],
  );
}
