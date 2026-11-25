import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:window_manager/window_manager.dart';
import 'package:twodrive_dav/server_window.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';

class Picker extends FileSelectorPlatform {
  String? folder;
  String? input;
  String? output;
  @override
  Future<String?> getDirectoryPathWithOptions(
    FileDialogOptions options,
  ) async => folder;
  @override
  Future<XFile?> openFile({
    List<XTypeGroup>? acceptedTypeGroups,
    String? initialDirectory,
    String? confirmButtonText,
  }) async => input == null ? null : XFile(input!);
  @override
  Future<FileSaveLocation?> getSaveLocation({
    List<XTypeGroup>? acceptedTypeGroups,
    SaveDialogOptions options = const SaveDialogOptions(),
  }) async => output == null ? null : FileSaveLocation(output!);
}

class SharingClient extends StorageClient {
  SharingClient() {
    available = true;
    providers = [
      {'id': 'dav', 'label': 'WebDAV', 'fields': <Json>[]},
      {
        'id': 'peer',
        'label': 'PC / NAS',
        'actions': [
          {'id': 'import', 'label': '导入连接文件', 'input': 'file'},
        ],
        'fields': [
          {'key': 'invitation_file', 'label': '连接文件', 'type': 'file'},
          {'key': 'expected_device', 'label': '设备', 'advanced': true},
          {'key': 'peer_state', 'label': '状态', 'advanced': true},
        ],
      },
    ];
    status = {
      'connected': false,
      'sharing': {
        'supported': true,
        'running': false,
        'settings': {'name': 'My PC', 'root': '', 'network': 'auto'},
      },
    };
  }
  final calls = <String>[];
  final arguments = <String, Json>{};
  Json? saved;
  String? newlyPaired;
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add(op);
    arguments[op] = Map.from(args);
    if (op == 'status') {
      if (newlyPaired != null) status['sharing']['peers'] = [newlyPaired];
      return Map.from(status);
    }
    if (op == 'share_revoke') {
      newlyPaired = null;
      status['sharing']['peers'] = [];
    }
    if (op == 'share_start') {
      status['sharing'] = {
        'supported': true,
        'running': true,
        'settings': args,
        'peers': <String>[],
      };
    }
    if (op == 'share_stop') status['sharing']['running'] = false;
    if (op == 'share_export') return {'expires': 2000000000, 'name': 'My PC'};
    if (op == 'provider_action') {
      return {
        'message': '连接信息已填好',
        'options': {
          'name': 'Remote laptop',
          'writable': true,
          'expected_device': 'public-device',
          'invitation_file': args['file'],
        },
      };
    }
    if (op == 'save_profile') {
      saved = {'id': 'new-pc', ...args, 'revision': 'v1'};
      saved!['id'] = 'new-pc';
      return {'id': 'new-pc'};
    }
    if (op == 'profiles') {
      return {
        'profiles': saved == null ? <Json>[] : [saved!],
        'providers': providers,
        'selected': saved?['id'],
      };
    }
    if (op == 'connect') {
      status = {...status, 'connected': true, 'profile': args['id']};
      return {'entries': <Json>[]};
    }
    if (op == 'list') return {'entries': <Json>[]};
    return {};
  }
}

void main() {
  late Picker picker;
  late FileSelectorPlatform original;
  setUp(() {
    original = FileSelectorPlatform.instance;
    picker = Picker();
    FileSelectorPlatform.instance = picker;
  });
  tearDown(() {
    FileSelectorPlatform.instance = original;
  });

  testWidgets(
    'host selects folder, starts, exports and stops without a storage connection',
    (tester) async {
      tester.view.physicalSize = const Size(840, 580);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final directory = Directory.systemTemp.createTempSync(
        'td-sharing-widget-',
      );
      addTearDown(() => directory.deleteSync(recursive: true));
      picker.folder = directory.path;
      picker.output = '${directory.path}/connection.json';
      final c = SharingClient();
      await tester.pumpWidget(DavApp(client: c, server: true));
      await tester.pumpAndSettle();
      await tester.tap(find.byTooltip('选择共享文件夹'));
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('启动共享'));
      await tester.tap(find.text('启动共享'));
      await tester.pumpAndSettle();
      expect(c.arguments['share_start']?['root'], directory.path);
      expect(c.arguments['share_start']?['writable'], false);
      expect(c.hosting, true);
      expect(c.connected, false);
      expect(c.trayState, 'connected');
      expect(c.trayTitle, contains('正在共享'));
      c.status['connected'] = true;
      c.status['account_name'] = 'Signed in account';
      expect(c.trayTitle, contains('Signed in account'));
      expect(c.trayTitle, contains('正在共享'));
      c.status['connected'] = false;
      await tester.ensureVisible(find.text('导出连接文件'));
      await tester.runAsync(() async {
        await tester.tap(find.text('导出连接文件'));
        final deadline = DateTime.now().add(const Duration(seconds: 3));
        while (!c.calls.contains('share_export')) {
          if (DateTime.now().isAfter(deadline)) {
            throw StateError('export callback not reached');
          }
          await Future<void>.delayed(const Duration(milliseconds: 10));
        }
      });
      await tester.pumpAndSettle();
      expect(c.arguments['share_export']?['output'], picker.output);
      expect(find.textContaining('已导出：'), findsOneWidget);
      c.newlyPaired = '0123456789abcdef0123456789abcdef';
      await tester.tap(find.byTooltip('刷新状态'));
      await tester.pumpAndSettle();
      final revoke = find.text('撤销访问');
      await tester.scrollUntilVisible(
        revoke,
        200,
        scrollable: find.byType(Scrollable).last,
      );
      await tester.tap(revoke);
      await tester.pumpAndSettle();
      await tester.tap(find.text('撤销'));
      await tester.pumpAndSettle();
      expect(
        c.arguments['share_revoke']?['device'],
        '0123456789abcdef0123456789abcdef',
      );
      await tester.ensureVisible(find.text('停止共享'));
      await tester.tap(find.text('停止共享'));
      await tester.pumpAndSettle();
      expect(c.hosting, false);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('closing an active server requires confirmation before quit', (
    tester,
  ) async {
    final client = SharingClient();
    client.status['sharing']['running'] = true;
    final methods = <String>[];
    const channel = MethodChannel('window_manager');
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(channel, (call) async {
      methods.add(call.method);
      return true;
    });
    addTearDown(() => messenger.setMockMethodCallHandler(channel, null));
    await tester.pumpWidget(DavApp(client: client, server: true));
    await tester.pumpAndSettle();
    final window = tester.state(find.byType(ServerWindow)) as WindowListener;
    window.onWindowClose();
    await tester.pumpAndSettle();
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(client.hosting, isTrue);
    expect(client.calls, isNot(contains('quit')));
    window.onWindowClose();
    await tester.pumpAndSettle();
    await tester.tap(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.text('停止共享'),
      ),
    );
    await tester.pumpAndSettle();
    expect(client.calls.where((op) => op == 'quit'), hasLength(1));
    expect(methods, contains('destroy'));
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });

  testWidgets(
    'add connection imports hints then saves and connects the newly created profile',
    (tester) async {
      tester.view.physicalSize = const Size(1100, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      picker.input = '/private/TwoDrive-connection.json';
      final c = SharingClient();
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('连接').first);
      await tester.pumpAndSettle();
      await tester.tap(find.text('添加连接'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('从 TwoDrive 连接文件添加'));
      await tester.pumpAndSettle();
      expect(c.arguments['provider_action']?['kind'], 'peer');
      expect(find.text('Remote laptop'), findsOneWidget);
      await tester.ensureVisible(find.text('保存并连接'));
      await tester.tap(find.text('保存并连接'));
      await tester.pumpAndSettle();
      expect(c.arguments['save_profile']?['name'], 'Remote laptop');
      expect(c.arguments['save_profile']?['kind'], 'peer');
      expect(c.arguments['save_profile']?.containsKey('_connect'), false);
      expect(c.arguments['connect']?['id'], 'new-pc');
      expect(
        c.calls.indexOf('connect'),
        greaterThan(c.calls.indexOf('save_profile')),
      );
      expect(c.connected, true);
      expect(c.selected, 'new-pc');
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('cancelled import leaves the current provider unchanged', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1100, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final c = SharingClient();
    await tester.pumpWidget(DavApp(client: c));
    await tester.tap(find.text('连接').first);
    await tester.pumpAndSettle();
    await tester.tap(find.text('添加连接'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('从 TwoDrive 连接文件添加'));
    await tester.pumpAndSettle();
    expect(c.calls, isNot(contains('provider_action')));
    expect(find.text('保存并连接'), findsNothing);
    expect(find.text('WebDAV'), findsWidgets);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });
}
