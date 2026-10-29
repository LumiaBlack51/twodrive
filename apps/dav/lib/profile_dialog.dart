import 'dart:io';
import 'package:flutter/material.dart';
import 'package:file_selector/file_selector.dart';
import 'client.dart';
import 'provider_auth_dialog.dart';

class ProfileDialog extends StatefulWidget {
  const ProfileDialog({
    super.key,
    this.original,
    required this.providers,
    this.client,
    this.initialAction,
    this.connectOnLogin = false,
  });
  final Json? original;
  final List<Json> providers;
  final StorageClient? client;
  final String? initialAction;
  final bool connectOnLogin;
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
  bool authenticated = false;
  int? activeActionRequest;
  String? actionMessage;
  String? selectionMessage;
  List<Json> actionItems = [];
  Json? pickedItem;
  Json? get provider =>
      widget.providers.where((p) => p['id'] == kind).firstOrNull;
  List<Json> get providerFields =>
      ((provider?['fields'] as List?) ?? []).cast<Json>();
  List<String> credentialFields(Json action) =>
      ((action['credential_fields'] as List?) ?? [])
          .cast<String>()
          .where(
            (key) => providerFields.any(
              (f) =>
                  f['key'] == key &&
                  f['transient'] == true &&
                  f['omit_from_profile'] == true,
            ),
          )
          .toList();
  bool get inlineCredentials => ((provider?['actions'] as List?) ?? [])
      .cast<Json>()
      .any((action) => credentialFields(action).isNotEmpty);
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
    writable = p['writable'] != false;
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
    if (widget.initialAction != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        final action = ((provider?['actions'] as List?) ?? [])
            .cast<Json>()
            .where((a) => a['id'] == widget.initialAction)
            .firstOrNull;
        if (mounted && action != null) providerAction(action);
      });
    }
  }

  void saveProfile([bool connect = false]) {
    if (!form.currentState!.validate()) return;
    Navigator.pop(context, {
      if (connect ||
          (widget.original == null &&
              kind == 'peer' &&
              fields['invitation_file']?.text.isNotEmpty == true))
        '_connect': true,
      'id': widget.original?['id'] ?? '',
      'kind': kind,
      'network_route': route,
      'writable': writable,
      'name': fields['name']!.text.trim(),
      'mount_path': fields['mount_path']!.text.trim(),
      for (final f in providerFields.where(
        (f) => f['omit_from_profile'] != true,
      ))
        f['key'] as String: f['type'] == 'boolean'
            ? (flags[f['key']] ?? false)
            : (f['secret'] == true
                  ? fields[f['key']]!.text
                  : fields[f['key']]!.text.trim()),
    });
  }

  Future<void> saveOrLogin() async {
    final action = ((provider?['actions'] as List?) ?? [])
        .cast<Json>()
        .where((a) => credentialFields(a).isNotEmpty)
        .firstOrNull;
    if (!authenticated &&
        action != null &&
        credentialFields(action).any((key) => fields[key]!.text.isNotEmpty)) {
      await providerAction(action, saveOnSuccess: true);
    } else {
      saveProfile(authenticated);
    }
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
      key: ValueKey('profile-$key'),
      controller: fields[key],
      obscureText: secret,
      autocorrect: !secret,
      enableSuggestions: !secret,
      onChanged:
          providerFields.any(
            (f) => f['key'] == key && f['omit_from_profile'] == true,
          )
          ? (_) => setState(() => authenticated = false)
          : null,
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

  Future<Json> requestProviderAction(Json arguments) async {
    final id = widget.client!.reserveRequestId();
    if (mounted) setState(() => activeActionRequest = id);
    try {
      return await widget.client!.request('provider_action', arguments, id);
    } finally {
      if (mounted) setState(() => activeActionRequest = null);
    }
  }

  Future<void> cancelProviderAction() async {
    final id = activeActionRequest;
    if (id == null) return;
    try {
      await widget.client!.request('cancel', {'id': id});
    } catch (_) {
      if (mounted) setState(() => actionMessage = '取消未完成，请稍后重试。');
    }
  }

  Future<void> providerAction(Json action, {bool saveOnSuccess = false}) async {
    final targetKind = action['provider'] as String? ?? kind;
    final credentials = targetKind == kind
        ? credentialFields(action)
        : <String>[];
    for (final key in credentials) {
      if (fields[key]!.text.isEmpty) {
        final label = providerFields.firstWhere(
          (f) => f['key'] == key,
        )['label'];
        setState(() => actionMessage = '请先填写$label。');
        return;
      }
    }
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
      Json result;
      try {
        result = await requestProviderAction({
          'kind': targetKind,
          'action': action['id'],
          'network_route': route,
          'file': ?file,
          for (final key in credentials) key: fields[key]!.text,
          for (final key
              in ((action['input_fields'] as List?) ?? []).cast<String>())
            if (targetKind == kind &&
                fields.containsKey(key) &&
                providerFields.any(
                  (f) => f['key'] == key && f['secret'] != true,
                ))
              key: fields[key]!.text,
          if (targetKind == kind && fields.containsKey('expected_device'))
            'expected_device': fields['expected_device']!.text,
        });
      } finally {
        for (final key in credentials) {
          if (mounted &&
              providerFields.any(
                (f) => f['key'] == key && f['secret'] == true,
              )) {
            fields[key]!.clear();
          }
        }
      }
      Json? pendingAuth;
      try {
        while (result['auth'] is Map && mounted) {
          final question = result['auth'] as Json;
          pendingAuth = question;
          if (!mounted) return;
          var answer = await askProviderAuth(context, question);
          if (!mounted) return;
          result = await requestProviderAction({
            'kind': targetKind,
            'action':
                question[answer == null ? 'cancel_action' : 'reply_action'],
            ...((question['context'] as Json?) ?? {}),
            'attempt': question['attempt'],
            'answer': ?answer,
          });
          answer = null;
          pendingAuth = null;
        }
      } finally {
        if (pendingAuth != null) {
          try {
            await requestProviderAction({
              'kind': targetKind,
              'action': pendingAuth['cancel_action'],
              ...((pendingAuth['context'] as Json?) ?? {}),
              'attempt': pendingAuth['attempt'],
            });
          } catch (_) {
            /* An expired attempt must not cancel a newer login. */
          }
        }
      }
      if (!mounted) return;
      setState(() {
        actionItems = ((result['items'] as List?) ?? []).cast<Json>();
        actionMessage = result['message'] as String?;
        authenticated = result['authenticated'] == true;
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
      if (authenticated && (widget.connectOnLogin || saveOnSuccess)) {
        saveProfile(true);
      }
    } catch (e) {
      if (mounted) {
        setState(() => actionMessage = e is StateError ? e.message : '$e');
      }
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
              if (inlineCredentials)
                for (final f in providerFields.where(
                  (f) => f['advanced'] != true,
                ))
                  providerField(f),
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
                (f) => !inlineCredentials && f['advanced'] != true,
              ))
                providerField(f),
              if (((provider?['actions'] as List?) ?? []).isNotEmpty)
                const SizedBox(height: 20),
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
                    const DropdownMenuItem(
                      value: 'system',
                      child: Text('使用系统路由和代理'),
                    ),
                ],
                onChanged: runningAction
                    ? null
                    : (value) => setState(() => route = value!),
              ),
              const SizedBox(height: 14),
              ExpansionTile(
                title: const Text('高级设置'),
                tilePadding: EdgeInsets.zero,
                children: [
                  for (final f in providerFields.where(
                    (f) => f['advanced'] == true,
                  ))
                    providerField(f),
                  if (Platform.isLinux || Platform.isWindows)
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
        onPressed: runningAction
            ? (activeActionRequest == null ? null : cancelProviderAction)
            : () => Navigator.pop(context),
        child: Text(runningAction ? '取消当前操作' : '取消'),
      ),
      FilledButton(
        onPressed: provider == null || runningAction ? null : saveOrLogin,
        child: Text(
          authenticated ||
                  (widget.original == null &&
                      kind == 'peer' &&
                      fields['invitation_file']?.text.isNotEmpty == true)
              ? '保存并连接'
              : '保存',
        ),
      ),
    ],
  );
}
