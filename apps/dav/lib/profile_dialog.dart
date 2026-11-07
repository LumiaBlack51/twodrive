import 'dart:io';
import 'package:flutter/material.dart';
import 'package:file_selector/file_selector.dart';
import 'client.dart';

class ProfileDialog extends StatefulWidget {
  const ProfileDialog({
    super.key,
    this.original,
    required this.providers,
    this.client,
  });
  final Json? original;
  final List<Json> providers;
  final StorageClient? client;
  @override
  State<ProfileDialog> createState() => _ProfileDialogState();
}

class _ProfileDialogState extends State<ProfileDialog> {
  final form = GlobalKey<FormState>();
  late final Map<String, TextEditingController> fields;
  late final Map<String, bool> flags;
  late String? kind;
  late bool writable;
  late String route;
  bool runningAction = false;
  String? actionMessage;
  String? selectionMessage;
  List<Json> actionItems = [];
  Json? pickedItem;
  Json? get provider =>
      widget.providers.where((p) => p['id'] == kind).firstOrNull;
  List<Json> get providerFields => ((provider?['fields'] as List?) ?? [])
      .cast<Json>()
      .where((f) => f['internal'] != true)
      .toList();
  List<String> get networkModes =>
      ((provider?['network_modes'] as List?) ?? ['auto', 'direct', 'system'])
          .cast<String>();
  void normalizeRoute() {
    if (!networkModes.contains(route)) route = networkModes.first;
  }

  @override
  void initState() {
    super.initState();
    final p = widget.original ?? {};
    route = p['network_route'] as String? ?? 'auto';
    final originalKind = p['kind'] as String?;
    kind = widget.providers.any((d) => d['id'] == originalKind)
        ? originalKind
        : widget.providers.firstOrNull?['id'] as String?;
    writable = p['writable'] == true;
    final keys = <String>{
      'name',
      'mount_path',
      for (final d in widget.providers)
        for (final f in d['fields'] as List) f['key'] as String,
    };
    fields = {
      for (final key in keys)
        key: TextEditingController(
          text: p[key] is String ? p[key] as String : '',
        ),
    };
    flags = {for (final key in keys) key: p[key] == true};
    if (fields['name']!.text.isEmpty) {
      fields['name']!.text = provider?['label'] as String? ?? '';
    }
    normalizeRoute();
  }

  @override
  void dispose() {
    for (final field in fields.values) {
      field.dispose();
    }
    super.dispose();
  }

  Widget field(
    String key,
    String label, {
    bool secret = false,
    bool required = false,
    bool readOnly = false,
    String? type,
  }) => Padding(
    padding: const EdgeInsets.only(bottom: 14),
    child: TextFormField(
      controller: fields[key],
      obscureText: secret,
      readOnly: readOnly,
      decoration: InputDecoration(
        labelText: label,
        suffixIcon: type == 'file' || type == 'directory'
            ? IconButton(
                tooltip: '选择文件或文件夹',
                icon: const Icon(Icons.folder_open),
                onPressed: runningAction
                    ? null
                    : () async {
                        final path = type == 'directory'
                            ? await getDirectoryPath()
                            : (await openFile(
                                acceptedTypeGroups: const [
                                  XTypeGroup(
                                    label: 'JSON 文件',
                                    extensions: ['json'],
                                  ),
                                ],
                              ))?.path;
                        if (mounted && path != null) {
                          setState(() => fields[key]!.text = path);
                        }
                      },
              )
            : null,
      ),
      validator: (text) =>
          required && (text == null || text.trim().isEmpty) ? '请填写此项' : null,
    ),
  );
  Widget providerField(Json f) {
    final key = f['key'] as String;
    if (f['type'] == 'boolean') {
      return SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: Text(f['label'] as String),
        value: flags[key] ?? false,
        onChanged: runningAction ? null : (v) => setState(() => flags[key] = v),
      );
    }
    final label =
        f['secret'] == true &&
            widget.original?['kind'] == kind &&
            widget.original?['${key}_saved'] == true
        ? '${f['label']} · 留空保留已保存值'
        : f['label'] as String;
    return field(
      key,
      label,
      secret: f['secret'] == true,
      required: f['required'] == true,
      readOnly: f['read_only'] == true,
      type: f['type'] as String?,
    );
  }

  Future<void> providerAction(Json action) async {
    final targetKind = action['provider'] as String? ?? kind;
    setState(() {
      runningAction = true;
      actionMessage = null;
      actionItems = [];
      pickedItem = null;
    });
    try {
      String? file;
      if (action['input'] == 'file') {
        file = (await openFile(
          acceptedTypeGroups: const [
            XTypeGroup(label: 'TwoDrive 连接文件', extensions: ['json']),
          ],
        ))?.path;
        if (file == null || !mounted) return;
      }
      final result = await widget.client!.request('provider_action', {
        'kind': targetKind,
        'action': action['id'],
        'file': ?file,
        if (targetKind == kind && fields.containsKey('expected_device'))
          'expected_device': fields['expected_device']!.text,
      });
      if (!mounted) return;
      setState(() {
        actionItems = ((result['items'] as List?) ?? []).cast<Json>();
        actionMessage = result['message'] as String?;
        selectionMessage = result['selection_message'] as String?;
        if (result['options'] is Map) {
          final previousLabel = provider?['label'];
          final keepName =
              widget.original != null || fields['name']!.text != previousLabel;
          if (kind != targetKind) {
            for (final field in providerFields.where(
              (f) => f['secret'] == true,
            )) {
              fields[field['key']]!.clear();
            }
          }
          kind = targetKind;
          normalizeRoute();
          for (final option in (result['options'] as Map).entries) {
            if (option.key == 'writable' && option.value is bool) {
              writable = option.value as bool;
            } else if (fields.containsKey(option.key) &&
                option.value is String &&
                !(option.key == 'name' && keepName)) {
              fields[option.key]!.text = option.value as String;
            }
          }
        }
      });
    } catch (e) {
      if (mounted) setState(() => actionMessage = '$e');
    } finally {
      if (mounted) setState(() => runningAction = false);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.original == null ? '添加连接' : '编辑连接'),
    content: SizedBox(
      width: 540,
      child: SingleChildScrollView(
        child: Form(
          key: form,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (widget.original == null &&
                  widget.providers.any(
                    (p) =>
                        p['id'] == 'peer' &&
                        ((p['actions'] as List?) ?? []).any(
                          (a) => a['id'] == 'import',
                        ),
                  ))
                Padding(
                  padding: const EdgeInsets.only(bottom: 16),
                  child: SizedBox(
                    width: double.infinity,
                    child: FilledButton.tonalIcon(
                      onPressed: runningAction || widget.client == null
                          ? null
                          : () => providerAction({
                              'id': 'import',
                              'input': 'file',
                              'provider': 'peer',
                            }),
                      icon: const Icon(Icons.file_open),
                      label: const Text('从 TwoDrive 连接文件添加'),
                    ),
                  ),
                ),
              field('name', '连接名称', required: true),
              DropdownButtonFormField<String>(
                key: ValueKey('provider-$kind'),
                initialValue: kind,
                decoration: const InputDecoration(labelText: '存储提供者'),
                items: [
                  for (final d in widget.providers)
                    DropdownMenuItem(
                      value: d['id'] as String,
                      child: Text(d['label'] as String),
                    ),
                ],
                onChanged: runningAction
                    ? null
                    : (value) => setState(() {
                        final previousLabel = provider?['label'];
                        for (final f in providerFields.where(
                          (f) => f['secret'] == true,
                        )) {
                          fields[f['key']]!.clear();
                        }
                        kind = value;
                        normalizeRoute();
                        actionMessage = null;
                        actionItems = [];
                        pickedItem = null;
                        if (fields['name']!.text == previousLabel) {
                          fields['name']!.text =
                              provider?['label'] as String? ?? '';
                        }
                      }),
              ),
              const SizedBox(height: 14),
              if (provider == null) const Text('引擎没有可用的存储提供者，请检查引擎版本。'),
              if (provider?['help'] is String)
                Padding(
                  padding: const EdgeInsets.only(bottom: 14),
                  child: Text(provider!['help'] as String),
                ),
              for (final action
                  in ((provider?['actions'] as List?) ?? []).cast<Json>())
                Align(
                  alignment: Alignment.centerLeft,
                  child: OutlinedButton.icon(
                    onPressed: runningAction || widget.client == null
                        ? null
                        : () => providerAction(action),
                    icon: runningAction
                        ? const SizedBox(
                            width: 16,
                            height: 16,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : Icon(
                            action['input'] == 'file'
                                ? Icons.file_open
                                : Icons.radar,
                          ),
                    label: Text(action['label'] as String),
                  ),
                ),
              if (actionMessage != null)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 10),
                  child: Text(actionMessage!),
                ),
              for (final item in actionItems)
                ListTile(
                  dense: true,
                  title: Text(item['label'] as String),
                  subtitle: Text(item['detail'] as String? ?? ''),
                  leading: const Icon(Icons.devices),
                  selected: pickedItem == item,
                  onTap: () => setState(() {
                    pickedItem = item;
                    for (final entry in (item['options'] as Map).entries) {
                      if (entry.key == 'writable' && entry.value is bool) {
                        writable = entry.value as bool;
                      } else if (fields.containsKey(entry.key) &&
                          entry.value is String) {
                        fields[entry.key]!.text = entry.value as String;
                      }
                    }
                    actionMessage = selectionMessage ?? '已选择。';
                  }),
                ),
              for (final f in providerFields.where(
                (f) => f['advanced'] != true,
              ))
                providerField(f),
              DropdownButtonFormField<String>(
                key: ValueKey('route-$kind'),
                initialValue: route,
                decoration: const InputDecoration(labelText: '网络连接方式'),
                items: [
                  if (networkModes.contains('auto'))
                    const DropdownMenuItem(
                      value: 'auto',
                      child: Text('优先直连，失败时提示并允许代理兜底'),
                    ),
                  if (networkModes.contains('direct'))
                    const DropdownMenuItem(
                      value: 'direct',
                      child: Text('仅直连（绕过代理）'),
                    ),
                  if (networkModes.contains('system'))
                    DropdownMenuItem(
                      value: 'system',
                      child: Text(
                        (provider?['network_labels'] as Map?)?['system']
                                as String? ??
                            '使用系统路由和代理',
                      ),
                    ),
                ],
                onChanged: runningAction
                    ? null
                    : (value) => setState(() => route = value!),
              ),
              const SizedBox(height: 14),
              if ((provider?['capabilities'] as Map?) == null ||
                  ['upload', 'mkdir', 'rename', 'delete'].any(
                    (key) => (provider!['capabilities'] as Map)[key] == true,
                  ))
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  title: const Text('允许修改远端文件'),
                  subtitle: const Text('可用操作取决于提供者和远端授权。'),
                  value: writable,
                  onChanged: runningAction
                      ? null
                      : (value) => setState(() => writable = value),
                ),
              ExpansionTile(
                title: const Text('高级设置'),
                tilePadding: EdgeInsets.zero,
                children: [
                  for (final f in providerFields.where(
                    (f) => f['advanced'] == true,
                  ))
                    providerField(f),
                  if (Platform.isLinux)
                    field('mount_path', '挂载目录 · 留空自动创建独立目录'),
                ],
              ),
            ],
          ),
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: runningAction ? null : () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: provider == null || runningAction
            ? null
            : () {
                if (form.currentState!.validate()) {
                  Navigator.pop(context, {
                    if (widget.original == null &&
                        kind == 'peer' &&
                        fields['invitation_file']?.text.isNotEmpty == true)
                      '_connect': true,
                    'id': widget.original?['id'] ?? '',
                    'kind': kind,
                    'network_route': route,
                    'writable': writable,
                    'name': fields['name']!.text.trim(),
                    'mount_path': fields['mount_path']!.text.trim(),
                    for (final f in providerFields)
                      f['key'] as String: f['type'] == 'boolean'
                          ? (flags[f['key']] ?? false)
                          : (f['secret'] == true
                                ? fields[f['key']]!.text
                                : fields[f['key']]!.text.trim()),
                  });
                }
              },
        child: Text(
          widget.original == null &&
                  kind == 'peer' &&
                  fields['invitation_file']?.text.isNotEmpty == true
              ? '保存并连接'
              : '保存',
        ),
      ),
    ],
  );
}
