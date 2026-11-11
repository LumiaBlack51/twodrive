import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/status_tray.dart';
import 'package:twodrive_dav/workspace.dart';

class RestoredClient extends StorageClient {
  final calls = <String>[];
  RestoredClient() {
    available = true;
    selected = 'one';
    profiles = [
      {
        'id': 'one',
        'revision': 'r',
        'kind': 'onedrive',
        'name': 'Restored drive',
      },
    ];
    status = {
      'connected': true,
      'profile': 'one',
      'multi_connection': true,
      'connections': [
        {'id': 'one'},
      ],
    };
    setView(visible: true);
  }
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add(op);
    if (op == 'status') return status;
    if (op == 'profiles') {
      return {'profiles': profiles, 'providers': [], 'selected': 'one'};
    }
    if (op == 'list') {
      return {
        'path': '/',
        'entries': [
          {
            'name': 'restored.txt',
            'path': '/restored.txt',
            'directory': false,
            'size': 4,
            'etag': 'v1',
          },
        ],
      };
    }
    throw StateError('Unexpected request: $op');
  }
}

void main() {
  testWidgets(
    'view files on the restored active connection loads without refocus or manual refresh',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      const window = MethodChannel('window_manager');
      messenger.setMockMethodCallHandler(window, (_) async => null);
      messenger.setMockMethodCallHandler(StatusTray.channel, (_) async => null);
      final c = RestoredClient();
      await tester.pumpWidget(DavApp(client: c, desktop: true));
      await tester.pumpAndSettle();
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      expect(c.entries, isEmpty);
      expect(c.calls.where((v) => v == 'list'), isEmpty);
      await tester.tap(find.text('查看文件'));
      await tester.pumpAndSettle();
      expect(find.text('restored.txt'), findsOneWidget);
      expect(c.calls.where((v) => v == 'list').length, 1);
      expect(c.calls, isNot(contains('connect')));
      expect(c.selected, 'one');
      await tester.pumpWidget(const SizedBox());
      c.dispose();
      trayReady = false;
      messenger.setMockMethodCallHandler(window, null);
      messenger.setMockMethodCallHandler(StatusTray.channel, null);
    },
  );
}
