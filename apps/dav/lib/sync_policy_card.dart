import 'package:flutter/material.dart';
import 'client.dart';

class SyncPolicyCard extends StatefulWidget {
  const SyncPolicyCard({super.key, required this.client});
  final StorageClient client;
  @override
  State<SyncPolicyCard> createState() => _SyncPolicyCardState();
}

class _SyncPolicyCardState extends State<SyncPolicyCard> {
  final patterns = TextEditingController();
  final directories = TextEditingController();
  bool initialized = false;
  bool saving = false;
  String? message;
  @override
  void dispose() {
    patterns.dispose();
    directories.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final policy = widget.client.status['sync_policy'];
    if (!initialized && policy is Map) {
      patterns.text = (policy['exclude_name_regexes'] as List? ?? []).join(
        '\n',
      );
      directories.text =
          (policy['exclude_directory_names'] as List? ?? ['node_modules']).join(
            '\n',
          );
      initialized = true;
    }
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '同步排除规则',
              style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 8),
            const Text(
              r'挂载中的 Office ~$ 临时文件仅保存在本机，不产生上传任务。备份默认跳过 node_modules 整个目录。',
            ),
            const SizedBox(height: 16),
            TextField(
              controller: patterns,
              minLines: 2,
              maxLines: 5,
              decoration: const InputDecoration(
                labelText: '文件名正则表达式（每行一条）',
                hintText: r'(?i)\.bak$',
              ),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: directories,
              minLines: 1,
              maxLines: 4,
              decoration: const InputDecoration(labelText: '备份排除目录名（每行一个）'),
            ),
            const SizedBox(height: 12),
            const Text('保存后重新连接账户生效。已排除的本地文件继续保留；此设置适用于文件管理器挂载与文件夹备份。'),
            const SizedBox(height: 12),
            FilledButton.tonal(
              onPressed: saving || !widget.client.available ? null : save,
              child: Text(saving ? '正在保存…' : '保存排除规则'),
            ),
            if (message != null)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(message!),
              ),
          ],
        ),
      ),
    );
  }

  Future<void> save() async {
    setState(() {
      saving = true;
      message = null;
    });
    List<String> lines(String value) => value
        .split('\n')
        .map((s) => s.trim())
        .where((s) => s.isNotEmpty)
        .toList();
    try {
      await widget.client.request('save_sync_policy', {
        'exclude_name_regexes': lines(patterns.text),
        'exclude_directory_names': lines(directories.text),
      });
      await widget.client.refreshStatus();
      if (mounted) setState(() => message = '已保存，重新连接账户后生效。');
    } catch (e) {
      if (mounted) {
        setState(() => message = '未保存：${'$e'.replaceFirst('Bad state: ', '')}');
      }
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }
}
