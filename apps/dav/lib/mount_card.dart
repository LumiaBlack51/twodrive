import 'package:flutter/material.dart';
import 'client.dart';

Future<void> showMountHelp(
  BuildContext context, {
  StorageClient? client,
}) => showDialog<void>(
  context: context,
  builder: (context) => AlertDialog(
    title: const Text('安装挂载组件'),
    scrollable: true,
    content: SizedBox(
      width: 420,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (client?.status['mount_support']?['platform'] == 'linux')
            const Text(
              'Linux 挂载使用 FUSE 和 fusermount3。请确认已安装 fuse3 且可访问 /dev/fuse，已有网盘登录会保留。',
            )
          else ...[
            const Text('Windows 挂载需要 WinFsp。请从官方页面下载安装程序，完成后回到此处重试；已有网盘登录会保留。'),
            const SizedBox(height: 12),
            const SelectableText('https://winfsp.dev/rel/'),
          ],
          const SizedBox(height: 12),
          const Text('连接后即可在客户端和挂载目录中编辑，具体操作取决于网盘授权。'),
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('知道了'),
      ),
    ],
  ),
);

class MountCard extends StatelessWidget {
  const MountCard({
    super.key,
    required this.client,
    required this.onOpen,
    this.profileId,
  });
  final StorageClient client;
  final VoidCallback onOpen;
  final String? profileId;
  @override
  Widget build(BuildContext context) {
    final c = client;
    final id = profileId ?? c.selected;
    final p = c.profiles.where((p) => p['id'] == id).firstOrNull;
    final mount = id == null ? null : c.mountForProfile(id);
    final isMounted = mount != null;
    return Card(
      color: Colors.white,
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (profileId != null)
              Text(
                p?['name'] as String? ?? '网盘',
                style: Theme.of(context).textTheme.titleMedium,
              ),
            Text('文件管理器挂载', style: Theme.of(context).textTheme.titleLarge),
            const SizedBox(height: 10),
            SelectableText(
              mount?['path'] as String? ??
                  p?['mount_path'] as String? ??
                  '请先选择连接',
            ),
            const SizedBox(height: 12),
            if (c.mountDriverMissing) ...[
              Text(
                c.status['mount_support']?['platform'] == 'linux'
                    ? 'FUSE 组件不可用，请检查 fuse3 和 /dev/fuse。'
                    : '尚未安装 WinFsp，安装后即可挂载。',
              ),
              TextButton(
                onPressed: () => showMountHelp(context, client: c),
                child: const Text('查看安装说明'),
              ),
            ],
            Wrap(
              spacing: 12,
              runSpacing: 8,
              children: [
                FilledButton.tonalIcon(
                  onPressed:
                      c.busy ||
                          (id == null || (!c.canMountProfile(id) && !isMounted))
                      ? null
                      : () => c.mountToggle(id),
                  icon: Icon(isMounted ? Icons.eject : Icons.folder_open),
                  label: Text(isMounted ? '卸载' : '挂载'),
                ),
                OutlinedButton(
                  onPressed: isMounted && mount['failed'] != true
                      ? onOpen
                      : null,
                  child: const Text('打开挂载'),
                ),
              ],
            ),
            if (id != null) ...[
              const SizedBox(height: 12),
              OutlinedButton(
                onPressed: () => c.openWriteRecovery(id),
                child: const Text('打开写入恢复目录'),
              ),
            ],
            const SizedBox(height: 8),
            const Text(
              '恢复目录中的 before.bin 是原文件，edited.bin 或 .content 是保留的编辑内容，record.json 记录原路径。请先复制到其他位置，再恢复文件。',
            ),
            const SizedBox(height: 12),
            Text(
              '挂载按需下载。写入先保存到本地缓存，待上传完成后才代表服务器已收到。删除的文件请到网盘回收站恢复；覆盖前仍保留版本检查和恢复副本。卸载或退出前，请先完成文件操作。',
            ),
          ],
        ),
      ),
    );
  }
}
