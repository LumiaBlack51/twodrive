import 'dart:convert';
import 'dart:io';

class DesktopPreferences {
  bool autoMountOnConnect = true;
  bool restoreConnectionsOnStartup = true;
  File? _file;
  Future<void> _saving = Future.value();

  Future<void> load(File file) async {
    _file = file;
    if (!await file.exists()) return;
    final saved = jsonDecode(await file.readAsString()) as Map<String, dynamic>;
    autoMountOnConnect = saved['auto_mount_on_connect'] != false;
    restoreConnectionsOnStartup =
        saved['restore_connections_on_startup'] != false;
  }

  Future<void> setAutoMountOnConnect(bool value) => _save(autoMount: value);

  Future<void> setRestoreConnectionsOnStartup(bool value) =>
      _save(restore: value);

  Future<void> _save({bool? autoMount, bool? restore}) {
    // Serialize switches so overlapping writes retain both choices and do not
    // race on the temporary file. A failed save must not change either value.
    final result = _saving.then((_) async {
      final nextAutoMount = autoMount ?? autoMountOnConnect;
      final nextRestore = restore ?? restoreConnectionsOnStartup;
      final file = _file;
      if (file != null) {
        await file.parent.create(recursive: true);
        final temporary = File('${file.path}.tmp');
        await temporary.writeAsString(
          jsonEncode({
            'auto_mount_on_connect': nextAutoMount,
            'restore_connections_on_startup': nextRestore,
          }),
          flush: true,
        );
        await temporary.rename(file.path);
      }
      autoMountOnConnect = nextAutoMount;
      restoreConnectionsOnStartup = nextRestore;
    });
    _saving = result.catchError((Object _) {});
    return result;
  }
}
