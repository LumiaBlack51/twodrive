import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/profile_dialog.dart';

class ActionClient extends StorageClient {
  Json? received;
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    expect(op, 'provider_action');
    received = args;
    return {
      'message': '登录助手已打开，尚未连接',
      'options': {'icloud_state': '/private/icloud', 'writable': false},
    };
  }
}

void main() {
  testWidgets('schema action forwards only declared non-secret fields', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1100, 1100);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final client = ActionClient();
    await tester.pumpWidget(
      MaterialApp(
        home: ProfileDialog(
          client: client,
          original: const {
            'kind': 'icloud',
            'network_route': 'direct',
            'name': 'iCloud test',
            'icloud_state': '/private/existing',
            'private_password': 'synthetic-secret',
          },
          providers: const [
            {
              'id': 'icloud',
              'label': 'iCloud Drive',
              'network_modes': ['system', 'direct'],
              'fields': [
                {
                  'key': 'icloud_state',
                  'label': '登录目录',
                  'advanced': true,
                  'read_only': true,
                },
                {
                  'key': 'private_password',
                  'label': '私有测试字段',
                  'secret': true,
                  'advanced': true,
                },
              ],
              'actions': [
                {
                  'id': 'configure',
                  'label': '登录 iCloud',
                  'input_fields': [
                    'icloud_state',
                    'private_password',
                    'unknown',
                  ],
                },
              ],
            },
          ],
        ),
      ),
    );
    expect(find.text('iCloud Drive'), findsOneWidget);
    await tester.tap(find.text('登录 iCloud'));
    await tester.pumpAndSettle();
    expect(client.received, {
      'kind': 'icloud',
      'action': 'configure',
      'network_route': 'direct',
      'icloud_state': '/private/existing',
    });
    expect(find.text('登录助手已打开，尚未连接'), findsOneWidget);
    expect(client.connected, isFalse);
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });
}
