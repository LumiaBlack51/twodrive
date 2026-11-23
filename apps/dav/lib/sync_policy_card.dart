import 'language.dart';
import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'client.dart';

enum RuleType {
  filename('文件名', 'File name', 'exclude_filenames'),
  extension('扩展名', 'Extension', 'exclude_extensions'),
  folder('指定备份文件夹', 'Specific backup folder', 'exclude_directory_paths'),
  directory('备份文件夹名称', 'Backup folder name', 'exclude_directory_names'),
  wildcard('通配符', 'Wildcard', 'exclude_wildcards'),
  regex('正则表达式', 'Regular expression', 'exclude_name_regexes');

  const RuleType(this.chineseLabel, this.englishLabel, this.field);
  final String chineseLabel;
  final String englishLabel;
  String get label => uiText(chineseLabel, englishLabel);
  final String field;
}

class SyncPolicyCard extends StatefulWidget {
  const SyncPolicyCard({super.key, required this.client});
  final StorageClient client;
  @override
  State<SyncPolicyCard> createState() => _SyncPolicyCardState();
}

class _SyncPolicyCardState extends State<SyncPolicyCard> {
  late final rules = {
    for (final type in RuleType.values)
      type: List<String>.from(
        widget.client.status['sync_policy']?[type.field] ?? [],
      ),
  };
  bool saving = false;
  String? message;
  Future<void> add(RuleType type) async {
    String? value;
    if (type == RuleType.folder) {
      value = await getDirectoryPath();
    } else {
      var text = '';
      value = await showDialog<String>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(type.label),
          content: TextField(
            autofocus: true,
            onChanged: (v) => text = v,
            onSubmitted: (v) => Navigator.pop(context, v),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: Text(tr('取消')),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, text),
              child: Text(tr('添加')),
            ),
          ],
        ),
      );
    }
    if (!mounted || value == null || value.trim().isEmpty) return;
    var rule = value.trim();
    if (type == RuleType.extension) {
      rule = rule.replaceFirst(RegExp(r'^\.'), '');
    }
    if (rule.isNotEmpty && !rules[type]!.contains(rule)) {
      setState(() {
        rules[type]!.add(rule);
        message = null;
      });
    }
  }

  Future<void> save() async {
    setState(() => saving = true);
    try {
      await widget.client.request('save_sync_policy', {
        for (final e in rules.entries) e.key.field: List.of(e.value),
      });
      await widget.client.refreshStatus();
      if (mounted) setState(() => message = tr('已保存，重新连接账户后生效。'));
    } catch (e) {
      if (mounted) {
        setState(() => message = e.toString().replaceFirst('Bad state: ', ''));
      }
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  @override
  Widget build(BuildContext context) => Card(
    child: Padding(
      padding: const EdgeInsets.all(20),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(tr('同步排除规则'), style: Theme.of(context).textTheme.titleMedium),
          for (final e in rules.entries)
            for (final value in e.value)
              ListTile(
                dense: true,
                title: Text(value),
                subtitle: Text(e.key.label),
                trailing: IconButton(
                  tooltip: tr('移除规则'),
                  icon: const Icon(Icons.close),
                  onPressed: saving
                      ? null
                      : () => setState(() {
                          e.value.remove(value);
                          message = null;
                        }),
                ),
              ),
          Wrap(
            spacing: 12,
            children: [
              PopupMenuButton<RuleType>(
                enabled: !saving,
                onSelected: add,
                itemBuilder: (_) => [
                  for (final type in RuleType.values)
                    PopupMenuItem(value: type, child: Text(type.label)),
                ],
                child: Padding(
                  padding: EdgeInsets.all(12),
                  child: Text(tr('添加排除项')),
                ),
              ),
              FilledButton.tonal(
                onPressed: saving || !widget.client.available ? null : save,
                child: Text(tr('保存排除规则')),
              ),
            ],
          ),
          if (message != null) Text(message!),
        ],
      ),
    ),
  );
}
