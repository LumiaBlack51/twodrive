import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';

class RestoreClient extends StorageClient {
  RestoreClient() {
    available = true;
    selected = 'icloud';
    profiles = [
      for (final id in ['personal', 'icloud', 'baidu'])
        {'id': id, 'revision': id, 'startup_ready': true},
    ];
  }
  final restored = <String>[];
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    expect(op, 'resume');
    restored.add(args['id'] as String);
    if (args['id'] == 'personal') throw StateError('Unavailable');
    return {};
  }

  @override
  Future<void> refreshStatus() async {}
  @override
  Future<void> loadProfiles() async {}
}

void main() {
  test(
    'maintenance restart silently resumes only requested profiles, selected last',
    () async {
      final c = RestoreClient();
      await c.restoreConnectionsAtStartup([
        'icloud',
        'personal',
        'icloud',
        'unknown',
      ]);
      expect(c.restored, ['personal', 'icloud']);
      expect(c.connectionErrors, {'personal': 'Unavailable'});
      c.dispose();
    },
  );
}
