import 'dart:async';
import 'package:flutter/material.dart';
import 'client.dart';
import 'workspace_navigation.dart';
import 'native_progress.dart';

/// A status overview, never a directory/content/quota reader.
class OverviewPage extends StatefulWidget {
  const OverviewPage({
    super.key,
    required this.client,
    required this.onNavigate,
    required this.onOpenMount,
  });
  final StorageClient client;
  final ValueChanged<int> onNavigate;
  final Future<void> Function(String) onOpenMount;

  @override
  State<OverviewPage> createState() => _OverviewPageState();
}

class _OverviewPageState extends State<OverviewPage> {
  Timer? timer;
  bool refreshing = false;

  @override
  void initState() {
    super.initState();
    timer = Timer.periodic(const Duration(seconds: 10), (_) async {
      final c = widget.client;
      if (!mounted ||
          refreshing ||
          !c.viewVisible ||
          !c.available ||
          c.busy ||
          c.quitting) {
        return;
      }
      refreshing = true;
      try {
        await c.refreshStatus();
      } catch (_) {
        // The shared status reader owns the visible error.
      } finally {
        refreshing = false;
      }
    });
  }

  @override
  void dispose() {
    timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.client;
    final native = c.status['cfapi'] as Map? ?? {};
    final progress = native['progress'] as Map? ?? {};
    final workLabel = native['running'] == true
        ? nativeWorkLabel(progress)
        : null;
    final summary = progress['summary'] as Map? ?? {};
    final cache = c.status['cache'] as Map? ?? {};
    final issue =
        progress['local']?['error'] ??
        progress['remote']?['error'] ??
        progress['residency']?['error'];
    final nativeLabel = issue != null
        ? '需要处理'
        : progress['phase'] == 'paused'
        ? '已暂停'
        : native['running'] == true
        ? '运行中'
        : native['preparing'] == true
        ? '准备中'
        : native['configured'] == true
        ? '已停止'
        : '未配置';
    final stateLabel = !c.available
        ? '正在连接引擎'
        : c.error != null || c.syncError || issue != null
        ? '有状态需要处理'
        : c.paused
        ? '同步已暂停'
        : c.syncing
        ? '正在处理同步活动'
        : '当前无传输活动';
    final stateColor = c.error != null || c.syncError || issue != null
        ? const Color(0xffbd531d)
        : const Color(0xff16766a);
    final enabled = c.available && !c.busy && !c.quitting;
    return ListView(
      key: const ValueKey('sync-overview'),
      children: [
        Container(
          padding: const EdgeInsets.all(24),
          decoration: BoxDecoration(
            gradient: const LinearGradient(
              colors: [Color(0xffe4eeff), Color(0xffeef7f5)],
            ),
            borderRadius: BorderRadius.circular(20),
          ),
          child: Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text(
                      '你的云盘，一处管理',
                      style: TextStyle(
                        fontSize: 25,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const SizedBox(height: 10),
                    Text(
                      stateLabel,
                      style: TextStyle(
                        color: stateColor,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const SizedBox(height: 6),
                    const Text(
                      '按需访问云端文件，集中查看连接、缓存与同步目录。',
                      style: TextStyle(color: Color(0xff506777)),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 14),
              Icon(
                c.syncing
                    ? Icons.cloud_sync_outlined
                    : Icons.cloud_done_outlined,
                size: 52,
                color: const Color(0xff326bd6),
              ),
            ],
          ),
        ),
        const SizedBox(height: 18),
        LayoutBuilder(
          builder: (context, constraints) {
            final width = constraints.maxWidth;
            final columns = width >= 760 ? 4 : 2;
            return Wrap(
              spacing: 12,
              runSpacing: 12,
              children: [
                metric(
                  '已连接账号',
                  '${c.connectedProfiles.length} / ${c.profiles.length}',
                  Icons.dns_outlined,
                  (width - 12 * (columns - 1)) / columns,
                ),
                metric(
                  '文件管理器挂载',
                  '${c.mounts.length}',
                  Icons.folder_open_outlined,
                  (width - 12 * (columns - 1)) / columns,
                ),
                metric(
                  '当前账户缓存',
                  cache['bytes'] is num ? size(cache['bytes'] as num) : '—',
                  Icons.storage_outlined,
                  (width - 12 * (columns - 1)) / columns,
                ),
                metric(
                  workLabel == null ? '原生待处理' : '原生状态',
                  workLabel != null
                      ? '处理中'
                      : native['configured'] == true
                      ? '${summary['durable_work_items'] ?? '—'}'
                      : '—',
                  Icons.cloud_sync_outlined,
                  (width - 12 * (columns - 1)) / columns,
                ),
              ],
            );
          },
        ),
        const SizedBox(height: 22),
        LayoutBuilder(
          builder: (context, constraints) {
            final accounts = Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Row(
                  children: [
                    const Expanded(
                      child: Text(
                        '同步目录',
                        style: TextStyle(
                          fontSize: 18,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                    TextButton(
                      onPressed: () =>
                          widget.onNavigate(WorkspacePage.connections.index),
                      child: const Text('管理连接'),
                    ),
                  ],
                ),
                Card(
                  color: Colors.white,
                  margin: EdgeInsets.zero,
                  child: Column(
                    children: [
                      if (c.profiles.isEmpty)
                        const Padding(
                          padding: EdgeInsets.all(22),
                          child: Text('添加存储账号或已配对设备，即可开始使用。'),
                        ),
                      for (final profile in c.profiles)
                        accountRow(profile, enabled),
                    ],
                  ),
                ),
              ],
            );
            final nativeCard = Card(
              color: Colors.white,
              margin: EdgeInsets.zero,
              child: Padding(
                padding: const EdgeInsets.all(20),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        const Icon(
                          Icons.cloud_sync_outlined,
                          color: Color(0xff326bd6),
                        ),
                        const SizedBox(width: 10),
                        const Expanded(
                          child: Text(
                            'OneDrive 原生按需同步',
                            style: TextStyle(
                              fontSize: 17,
                              fontWeight: FontWeight.w600,
                            ),
                          ),
                        ),
                        Text(
                          nativeLabel,
                          style: TextStyle(
                            color: issue == null
                                ? const Color(0xff16766a)
                                : stateColor,
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: 12),
                    if (native['root'] is String)
                      SelectableText(native['root'] as String)
                    else
                      const Text('使用 Windows CFAPI，占位文件在打开时下载。'),
                    const SizedBox(height: 8),
                    if (workLabel != null) ...[
                      Text(workLabel),
                      const Text('本轮尚未完成，统计来自最近一次检查。'),
                    ],
                    if (native['configured'] == true)
                      Text(
                        '文件 ${summary['catalogue_files'] ?? '—'} · 目录 ${summary['catalogue_directories'] ?? '—'} · 待处理 ${summary['durable_work_items'] ?? '—'}',
                      ),
                    if ((native['exclusions']?['excluded_objects'] as num? ??
                            0) >
                        0)
                      Text(
                        '特殊对象及子项 ${native['exclusions']['excluded_objects']} 项仍通过原挂载或 OneDrive 网页访问。',
                      ),
                    if (issue != null)
                      Text(
                        '本轮同步待重试：$issue',
                        style: TextStyle(color: stateColor),
                      ),
                    const SizedBox(height: 6),
                    const Text(
                      '在文件管理器中右键选择“始终保留在此设备上”或“释放空间”。',
                      style: TextStyle(color: Colors.blueGrey),
                    ),
                    const SizedBox(height: 12),
                    OutlinedButton.icon(
                      onPressed: () =>
                          widget.onNavigate(WorkspacePage.cfapi.index),
                      icon: const Icon(Icons.arrow_forward, size: 18),
                      label: const Text('管理原生同步'),
                    ),
                  ],
                ),
              ),
            );
            if (constraints.maxWidth >= 820) {
              return Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Expanded(child: accounts),
                  const SizedBox(width: 18),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.only(top: 8),
                      child: nativeCard,
                    ),
                  ),
                ],
              );
            }
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [accounts, const SizedBox(height: 18), nativeCard],
            );
          },
        ),
        const SizedBox(height: 18),
        Wrap(
          spacing: 12,
          runSpacing: 8,
          children: [
            OutlinedButton.icon(
              onPressed: () => widget.onNavigate(WorkspacePage.backup.index),
              icon: const Icon(Icons.backup_outlined, size: 18),
              label: const Text('备份与上传来源'),
            ),
            OutlinedButton.icon(
              onPressed: () => widget.onNavigate(WorkspacePage.settings.index),
              icon: const Icon(Icons.tune, size: 18),
              label: const Text('缓存与排除规则'),
            ),
            OutlinedButton.icon(
              onPressed: () => widget.onNavigate(WorkspacePage.transfers.index),
              icon: const Icon(Icons.swap_vert, size: 18),
              label: const Text('查看传输'),
            ),
          ],
        ),
        const SizedBox(height: 10),
        const Text(
          '活动状态不代表所有队列已成功完成；待处理与冲突请在对应页面查看。',
          style: TextStyle(fontSize: 12, color: Colors.blueGrey),
        ),
      ],
    );
  }

  Widget accountRow(Json profile, bool enabled) {
    final c = widget.client;
    final id = profile['id'] as String;
    final mount = c.mountForProfile(id);
    final connected = c.isProfileConnected(id);
    return ListTile(
      key: ValueKey('overview-account-$id'),
      contentPadding: const EdgeInsets.symmetric(horizontal: 18, vertical: 4),
      leading: Container(
        padding: const EdgeInsets.all(10),
        decoration: BoxDecoration(
          color: const Color(0xffedf3ff),
          borderRadius: BorderRadius.circular(12),
        ),
        child: const Icon(Icons.cloud_outlined, color: Color(0xff326bd6)),
      ),
      title: Text(
        profile['name'] as String? ??
            profile['provider_label'] as String? ??
            '存储账号',
      ),
      subtitle: Text(
        mount?['failed'] == true
            ? '挂载需要处理 · ${mount?['path'] ?? ''}'
            : mount?['path'] as String? ?? (connected ? '已连接 · 尚未挂载' : '未连接'),
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: mount != null
          ? OutlinedButton(
              onPressed: enabled ? () => widget.onOpenMount(id) : null,
              child: const Text('打开目录'),
            )
          : TextButton(
              onPressed: () =>
                  widget.onNavigate(WorkspacePage.connections.index),
              child: const Text('管理'),
            ),
    );
  }

  Widget metric(String label, String value, IconData icon, double width) =>
      SizedBox(
        width: width,
        child: Card(
          color: Colors.white,
          margin: EdgeInsets.zero,
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(icon, size: 21, color: const Color(0xff60738b)),
                const SizedBox(height: 10),
                Text(
                  value,
                  style: const TextStyle(
                    fontSize: 23,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const SizedBox(height: 4),
                Text(
                  label,
                  style: const TextStyle(fontSize: 12, color: Colors.blueGrey),
                ),
              ],
            ),
          ),
        ),
      );

  String size(num value) => value < 1048576
      ? '${(value / 1024).toStringAsFixed(1)} KB'
      : value < 1073741824
      ? '${(value / 1048576).toStringAsFixed(1)} MB'
      : '${(value / 1073741824).toStringAsFixed(1)} GB';
}
