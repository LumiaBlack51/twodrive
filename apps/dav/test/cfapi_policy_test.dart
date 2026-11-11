import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/cfapi_page.dart';
import 'package:twodrive_dav/sync_policy_card.dart';

class Client extends StorageClient {
  final calls = <(String, Json)>[];
  Client() {
    available = true;
    profiles = [
      {'id': 'one', 'revision': 'r1', 'kind': 'onedrive', 'name': 'OneDrive'},
    ];
    status = {
      'cfapi': {'supported': true},
      'sync_policy': {
        'exclude_name_regexes': <String>[],
        'exclude_directory_names': ['node_modules'],
      },
    };
  }
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add((op, args));
    if (op == 'native_prepare') {
      status['cfapi'] = {
        'supported': true,
        'configured': true,
        'running': false,
        'profile': 'one',
        'root': args['root'],
      };
    }
    if (op == 'native_start') {
      status['cfapi']['running'] = true;
    }
    if (op == 'native_stop') {
      status['cfapi']['running'] = false;
    }
    return {};
  }

  @override
  Future<void> refreshStatus() async {
    notifyListeners();
  }
}

class ProgressClient extends Client {
  int refreshes = 0;
  @override
  Future<void> refreshStatus() async {
    refreshes++;
    status['cfapi'] = {
      'supported': true,
      'configured': true,
      'running': true,
      'profile': 'one',
      'root': r'C:\Native',
      'progress': {
        'phase': 'sync',
        'summary': {
          'catalogue_files': 12,
          'catalogue_directories': 3,
          'durable_work_items': 0,
        },
      },
    };
    notifyListeners();
  }
}

void main() {
  for (final retry in [true, false]) {
    testWidgets(
      'interrupted native setup exposes ${retry ? 'retry before ownership' : 'resume sealed ownership'}',
      (tester) async {
        tester.view.physicalSize = const Size(1200, 900);
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.resetPhysicalSize);
        addTearDown(tester.view.resetDevicePixelRatio);
        final c = Client();
        c.status['cfapi'] = {
          'supported': true,
          'configured': false,
          'preparing': true,
          'retry_prepare': retry,
          'profile': 'one',
          'root': r'C:\Retained-Native',
        };
        await tester.pumpWidget(DavApp(client: c));
        await tester.tap(find.text('原生同步'));
        await tester.pumpAndSettle();
        final button = find.text(retry ? '重试准备 CFAPI 原生目录' : '继续准备现有原生目录');
        await tester.ensureVisible(button);
        await tester.tap(button);
        await tester.pumpAndSettle();
        expect(c.calls.single.$1, retry ? 'native_prepare' : 'native_start');
        if (retry) expect(c.calls.single.$2['root'], r'C:\Retained-Native');
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox());
        c.dispose();
      },
    );
  }
  testWidgets(
    'visible native page refreshes progress without directory IO and stops when hidden',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 850);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = ProgressClient()..setView(visible: true);
      c.status['cfapi'] = {
        'supported': true,
        'configured': true,
        'running': true,
        'profile': 'one',
        'root': r'C:\Native',
      };
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('原生同步'));
      await tester.pumpAndSettle();
      await tester.pump(const Duration(seconds: 10));
      await tester.pump();
      expect(c.refreshes, 1);
      expect(find.text('文件 12 · 目录 3 · 待处理 0'), findsOneWidget);
      expect(c.calls, isEmpty);
      c.setView(visible: false);
      await tester.pump(const Duration(seconds: 20));
      expect(c.refreshes, 1);
      c.setView(visible: true);
      c.busy = true;
      await tester.pump(const Duration(seconds: 10));
      expect(c.refreshes, 1);
      c.busy = false;
      await tester.tap(find.text('设置'));
      await tester.pumpAndSettle();
      await tester.pump(const Duration(seconds: 20));
      expect(c.refreshes, 1);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'ordinary CFAPI scope shows excluded service objects explicitly',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 850);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = Client();
      c.status['cfapi'] = {
        'supported': true,
        'configured': true,
        'profile': 'one',
        'root': r'D:\TwoDrive\OneDrive-Native',
        'exclusions': {'ordinary_only': true, 'excluded_objects': 1},
      };
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(body: CfapiPage(client: c)),
        ),
      );
      expect(find.textContaining('1 项特殊对象'), findsOneWidget);
      expect(find.textContaining('原挂载'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets(
    'native folder text survives background engine busy transitions',
    (tester) async {
      tester.view.physicalSize = const Size(1200, 850);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = Client()..selected = 'other';
      c.profiles.add({
        'id': 'other',
        'kind': 'baidu',
        'name': 'Baidu',
        'revision': 'other-revision',
      });
      c.status.addAll({'connected': true, 'profile': 'other'});
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('原生同步'));
      await tester.pumpAndSettle();
      expect(find.text('断开'), findsNothing);
      expect(find.text('OneDrive · Windows CFAPI'), findsOneWidget);
      await tester.enterText(
        find.byType(TextField),
        r'D:\TwoDrive\OneDrive-Native',
      );
      c.busy = true;
      c.notifyListeners();
      await tester.pump();
      c.busy = false;
      c.notifyListeners();
      await tester.pumpAndSettle();
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        r'D:\TwoDrive\OneDrive-Native',
      );
      await tester.tap(find.text('准备 CFAPI 原生目录'));
      await tester.pumpAndSettle();
      expect(c.calls.last.$2['root'], r'D:\TwoDrive\OneDrive-Native');
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('CFAPI root lifecycle binds selected account and revision', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1100, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final c = Client();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(body: CfapiPage(client: c)),
      ),
    );
    await tester.enterText(
      find.byType(TextField),
      r'D:\TwoDrive\OneDrive-Native',
    );
    await tester.tap(find.text('准备 CFAPI 原生目录'));
    await tester.pumpAndSettle();
    expect(c.calls.single.$1, 'native_prepare');
    expect(c.calls.single.$2, {
      'profile': 'one',
      'revision': 'r1',
      'root': r'D:\TwoDrive\OneDrive-Native',
    });
    await tester.tap(find.text('启动原生同步'));
    await tester.pumpAndSettle();
    expect(c.calls.last.$1, 'native_start');
    expect(find.text('安全停止原生同步'), findsOneWidget);
    await tester.tap(find.text('安全停止原生同步'));
    await tester.pumpAndSettle();
    expect(c.calls.last.$1, 'native_stop');
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });
  testWidgets(
    'exclusion settings send validated engine configuration without altering profile grants',
    (tester) async {
      tester.view.physicalSize = const Size(1100, 800);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = Client();
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: SingleChildScrollView(child: SyncPolicyCard(client: c)),
          ),
        ),
      );
      await tester.enterText(find.byType(TextField).first, r'(?i)\.bak$');
      await tester.tap(find.text('保存排除规则'));
      await tester.pumpAndSettle();
      expect(c.calls.single.$1, 'save_sync_policy');
      expect(c.calls.single.$2, {
        'exclude_name_regexes': [r'(?i)\.bak$'],
        'exclude_directory_names': ['node_modules'],
      });
      expect(c.profiles.single['revision'], 'r1');
      expect(find.text('已保存，重新连接账户后生效。'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
}
