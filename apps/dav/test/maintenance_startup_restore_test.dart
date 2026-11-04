import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';

class RestoreClient extends StorageClient {
  RestoreClient() {
    selected = 'icloud';
    profiles = [
      for (final id in ['personal', 'icloud', 'baidu']) {'id': id},
    ];
  }
  final connectedIds = <String>[];
  bool rejectPersonal = false;
  @override
  Future<void> connect([String? id]) async {
    connectedIds.add(id!);
    selected = id;
    error = id == 'personal' && rejectPersonal ? 'Unavailable' : null;
  }
}

void main() {
  test(
    'maintenance restart restores known connections once and keeps selected account',
    () async {
      final c = RestoreClient();
      await c.restoreConnectionsAtStartup([
        'personal',
        'icloud',
        'baidu',
        'personal',
        'unknown',
      ]);
      expect(c.connectedIds, ['personal', 'baidu', 'icloud']);
      expect(c.selected, 'icloud');
      expect(c.error, isNull);
      c.dispose();
    },
  );
  test(
    'one failed restore does not lose original selection or hide the failure',
    () async {
      final c = RestoreClient()..rejectPersonal = true;
      await c.restoreConnectionsAtStartup(['personal', 'baidu']);
      expect(c.selected, 'icloud');
      expect(c.error, contains('部分连接未能恢复'));
      c.dispose();
    },
  );
}
