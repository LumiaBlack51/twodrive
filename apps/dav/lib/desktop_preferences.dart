import 'dart:convert';
import 'dart:io';

class DesktopPreferences {
  bool autoMountOnConnect = true;
  File? _file;

  Future<void> load(File file) async {
    _file = file;
    if (!await file.exists()) return;
    final saved = jsonDecode(await file.readAsString()) as Map<String, dynamic>;
    autoMountOnConnect = saved['auto_mount_on_connect'] != false;
  }

  Future<void> setAutoMountOnConnect(bool value) async {
    final file = _file;
    if (file != null) {
      await file.parent.create(recursive: true);
      final temporary = File('${file.path}.tmp');
      await temporary.writeAsString(
        jsonEncode({'auto_mount_on_connect': value}),
        flush: true,
      );
      await temporary.rename(file.path);
    }
    autoMountOnConnect = value;
  }
}
