import 'language.dart';
import 'package:flutter/material.dart';

import 'client.dart';

String cloudTargetLabel(Json profile) {
  final name =
      profile['name'] as String? ??
      profile['provider_label'] as String? ??
      '网盘';
  final account = profile['account_name'] as String?;
  return account == null || account.isEmpty || account == name
      ? name
      : '$name · $account';
}

class CloudTargetPicker extends StatelessWidget {
  const CloudTargetPicker({
    super.key,
    required this.fieldKey,
    required this.label,
    required this.client,
    required this.onSelect,
    this.backupOnly = false,
  });

  final Key fieldKey;
  final String label;
  final StorageClient client;
  final ValueChanged<Json>? onSelect;
  final bool backupOnly;

  bool supportsTarget(Json profile) =>
      !backupOnly || profile['capabilities']?['backup'] == true;

  Widget option(BuildContext context, Json profile, {bool menu = false}) {
    final enabled = supportsTarget(profile);
    final kind = profile['kind'] as String?;
    final name = cloudTargetLabel(profile);
    final text = menu && !enabled ? '$name（暂不支持自动备份）' : name;
    return Row(
      children: [
        Icon(
          switch (kind) {
            'icloud' => Icons.apple,
            'google' => Icons.add_to_drive,
            'baidu' => Icons.pets_rounded,
            _ => Icons.cloud_outlined,
          },
          size: 20,
          color: enabled
              ? Theme.of(context).colorScheme.primary
              : Theme.of(context).disabledColor,
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Tooltip(
            message: text,
            child: Text(
              text,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(
                color: enabled
                    ? Theme.of(context).colorScheme.onSurface
                    : Theme.of(context).disabledColor,
              ),
            ),
          ),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final profiles = client.profiles;
    return InputDecorator(
      decoration: InputDecoration(labelText: label),
      child: DropdownButtonHideUnderline(
        child: DropdownButton<String>(
          key: fieldKey,
          isExpanded: true,
          isDense: true,

          borderRadius: BorderRadius.circular(14),
          menuMaxHeight: 360,
          value: profiles.any((p) => p['id'] == client.selected)
              ? client.selected
              : null,
          hint: Text(tr('选择网盘')),
          selectedItemBuilder: (_) => [
            for (final profile in profiles) option(context, profile),
          ],
          items: [
            for (final profile in profiles)
              DropdownMenuItem(
                value: profile['id'] as String,
                enabled: supportsTarget(profile),
                child: option(context, profile, menu: true),
              ),
          ],
          onChanged: onSelect == null || profiles.isEmpty
              ? null
              : (id) {
                  final profile = client.profiles
                      .where((p) => p['id'] == id)
                      .firstOrNull;
                  if (profile != null && supportsTarget(profile)) {
                    onSelect!(profile);
                  }
                },
        ),
      ),
    );
  }
}
