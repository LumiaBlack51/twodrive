import 'dart:async';
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'ui_preview.dart';

class BackupPicker extends FileSelectorPlatform {
  Completer<String?>? held;
  @override
  Future<String?> getDirectoryPathWithOptions(
    FileDialogOptions options,
  ) async => held?.future ?? '/local/Photos';
}

class BackupClient extends StorageClient {
  BackupClient() {
    available = true;
    selected = active;
    profiles = [
      for (final (id, name, supported) in [
        ('a', 'OneDrive', true),
        ('b', 'Google Drive', true),
        ('i', 'iCloud Drive', false),
        ('d', '百度网盘', false),
      ])
        {
          'id': id,
          'revision': 'rev-$id',
          'name': name,
          'account_name': 'test-$id@example.invalid',
          'capabilities': {'backup': supported},
        },
    ];
    status = snapshot();
  }
  String active = 'a';
  final calls = <Json>[];
  final saved = <String, List<Json>>{
    'a': [
      {'local': '/local/Alpha', 'remote': '/Backups/Alpha', 'enabled': true},
    ],
    'b': [
      {'local': '/local/Beta', 'remote': '/Backups/Beta', 'enabled': true},
    ],
  };
  bool failLoad = false;
  bool failSelect = false;
  Completer<Json>? heldRead;
  Json snapshot() => {
    'connected': true,
    'profile': active,
    'multi_connection': true,
    'connections': [
      for (final id in ['a', 'b', 'i', 'd']) {'id': id},
    ],
    'capabilities': {'backup': active == 'a' || active == 'b'},
  };
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add({'op': op, ...args});
    if (op == 'status') return snapshot();
    if (op == 'profiles') return {'profiles': profiles, 'selected': active};
    if (op == 'list') return {'entries': <Json>[]};
    if (op == 'select') {
      if (failSelect) throw StateError('目标网盘暂不可用');
      active = args['id'] as String;
      return {'status': snapshot()};
    }
    if (op == 'backups') {
      if (failLoad) throw StateError('读取任务失败');
      if (args['id'] == 'a' && heldRead != null) return heldRead!.future;
      return {'folders': saved[args['id']] ?? <Json>[]};
    }
    if (op == 'save_backups') {
      expect(
        args['revision'],
        profiles.firstWhere((p) => p['id'] == args['id'])['revision'],
      );
      saved[args['id'] as String] = (args['folders'] as List).cast<Json>();
      return {'folders': saved[args['id']]};
    }
    throw StateError('Unexpected backup request: $op');
  }

  void selectOutside(String id, {String? revision}) {
    active = id;
    if (revision != null) {
      profiles.firstWhere((p) => p['id'] == id)['revision'] = revision;
    }
    selected = id;
    status = snapshot();
    notifyListeners();
  }
}

void main() {
  late FileSelectorPlatform original;
  late BackupPicker picker;
  setUp(() {
    original = FileSelectorPlatform.instance;
    picker = BackupPicker();
    FileSelectorPlatform.instance = picker;
  });
  tearDown(() => FileSelectorPlatform.instance = original);
  setUpAll(() async {
    await loadPreviewFonts();
    await (FontLoader('Noto Sans CJK SC')
          ..addFont(rootBundle.load('assets/fonts/NotoSansCJKSC-Regular.otf')))
        .load();
  });

  Future<void> open(WidgetTester tester, BackupClient c) async {
    tester.view.physicalSize = const Size(1100, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      RepaintBoundary(
        key: const ValueKey('appearance-preview'),
        child: DavApp(client: c),
      ),
    );
    await tester.tap(find.text('备份').first);
    await tester.pumpAndSettle();
    addTearDown(c.dispose);
  }

  Future<void> choose(WidgetTester tester, String name) async {
    await tester.tap(find.byKey(const ValueKey('backup-target')));
    await tester.pump(const Duration(milliseconds: 500));
    await tester.tap(find.text(name).last);
    await tester.pumpAndSettle();
  }

  FilledButton addButton(WidgetTester tester) => tester.widget<FilledButton>(
    find.ancestor(
      of: find.text('选择备份文件夹'),
      matching: find.byWidgetPredicate((w) => w is FilledButton),
    ),
  );

  testWidgets(
    'chooses a cloud in backup page and edits only that cloud tasks',
    (tester) async {
      final c = BackupClient();
      await open(tester, c);
      expect(find.text('Alpha'), findsOneWidget);
      await choose(tester, 'Google Drive · test-b@example.invalid');
      expect(c.selected, 'b');
      expect(find.text('Beta'), findsOneWidget);
      expect(find.text('Alpha'), findsNothing);
      expect(c.calls.where((r) => r['op'] == 'save_backups'), isEmpty);
      await tester.tap(find.byType(Switch));
      await tester.pumpAndSettle();
      expect(c.saved['b']!.single['enabled'], false);
      expect(c.saved['a']!.single['enabled'], true);
      await tester.tap(find.text('选择备份文件夹'));
      await tester.pumpAndSettle();
      expect(
        find.text('备份到：Google Drive · test-b@example.invalid'),
        findsOneWidget,
      );
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        '/Backups/Photos',
      );
      await appearanceSnapshot(tester, 'backup-target-dialog');
      await tester.tap(find.text('开始备份'));
      await tester.pumpAndSettle();
      expect(c.saved['b']!.last['local'], '/local/Photos');
      expect(c.saved['a'], hasLength(1));
      await tester.tap(find.byTooltip('停止备份，保留两端文件').last);
      await tester.pumpAndSettle();
      expect(c.saved['b'], hasLength(1));
      await choose(tester, 'OneDrive · test-a@example.invalid');
      expect(find.text('Alpha'), findsOneWidget);
      expect(
        c.calls.where(
          (r) => ['connect', 'disconnect', 'mount'].contains(r['op']),
        ),
        isEmpty,
      );
      await appearanceSnapshot(tester, 'backup-target-page');
    },
  );

  testWidgets(
    'shows unsupported clouds disabled and allows switching to a supported target',
    (tester) async {
      final c = BackupClient()..selectOutside('i');
      await open(tester, c);
      expect(addButton(tester).onPressed, isNull);
      await tester.tap(find.byKey(const ValueKey('backup-target')));
      await tester.pumpAndSettle();
      final dropdown = tester.widget<DropdownButton<String>>(
        find.byKey(const ValueKey('backup-target')),
      );
      expect(dropdown.items!.where((p) => !p.enabled).map((p) => p.value), [
        'i',
        'd',
      ]);
      expect(find.textContaining('暂不支持自动备份'), findsWidgets);
      await appearanceSnapshot(tester, 'backup-target-menu');
      await tester.tap(find.text('OneDrive · test-a@example.invalid').last);
      await tester.pumpAndSettle();
      expect(c.selected, 'a');
      expect(find.text('Alpha'), findsOneWidget);
      expect(tester.takeException(), isNull);
    },
  );

  testWidgets(
    'load failure disables mutations until existing tasks reload successfully',
    (tester) async {
      final c = BackupClient()..failLoad = true;
      await open(tester, c);
      expect(find.textContaining('读取任务失败'), findsOneWidget);
      expect(addButton(tester).onPressed, isNull);
      c.failLoad = false;
      await tester.tap(find.text('重新加载'));
      await tester.pumpAndSettle();
      expect(find.text('Alpha'), findsOneWidget);
      expect(c.calls.where((r) => r['op'] == 'save_backups'), isEmpty);
    },
  );

  testWidgets(
    'failed target selection keeps the displayed target and its tasks',
    (tester) async {
      final c = BackupClient()..failSelect = true;
      await open(tester, c);
      await choose(tester, 'Google Drive · test-b@example.invalid');
      expect(c.selected, 'a');
      expect(find.text('Alpha'), findsOneWidget);
      expect(
        tester
            .widget<DropdownButton<String>>(
              find.byKey(const ValueKey('backup-target')),
            )
            .value,
        'a',
      );
      expect(find.textContaining('目标网盘暂不可用'), findsOneWidget);
      expect(c.calls.where((r) => r['op'] == 'save_backups'), isEmpty);
    },
  );

  testWidgets(
    'a late task read cannot overwrite the newly selected cloud list',
    (tester) async {
      final c = BackupClient()..heldRead = Completer<Json>();
      // Do not settle while the initial progress indicator is active.
      tester.view.physicalSize = const Size(1100, 900);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('备份').first);
      await tester.pump();
      await choose(tester, 'Google Drive · test-b@example.invalid');
      c.heldRead!.complete({'folders': c.saved['a']});
      await tester.pumpAndSettle();
      expect(find.text('Beta'), findsOneWidget);
      expect(find.text('Alpha'), findsNothing);
      expect(tester.takeException(), isNull);
      c.dispose();
    },
  );

  for (final change in ['account', 'revision']) {
    testWidgets(
      'folder picker result is abandoned after target $change changes',
      (tester) async {
        final c = BackupClient();
        await open(tester, c);
        picker.held = Completer<String?>();
        await tester.tap(find.text('选择备份文件夹'));
        await tester.pump();
        c.selectOutside(
          change == 'account' ? 'b' : 'a',
          revision: change == 'revision' ? 'new-revision' : null,
        );
        await tester.pumpAndSettle();
        picker.held!.complete('/local/Photos');
        await tester.pumpAndSettle();
        expect(find.text('开始备份'), findsNothing);
        expect(c.calls.where((r) => r['op'] == 'save_backups'), isEmpty);
        expect(tester.takeException(), isNull);
      },
    );
  }

  testWidgets(
    'confirmation for an old target cannot save to the newly displayed cloud',
    (tester) async {
      final c = BackupClient();
      await open(tester, c);
      await tester.tap(find.text('选择备份文件夹'));
      await tester.pumpAndSettle();
      expect(
        find.text('备份到：OneDrive · test-a@example.invalid'),
        findsOneWidget,
      );
      c.selectOutside('b');
      await tester.pumpAndSettle();
      await tester.tap(find.text('开始备份'));
      await tester.pumpAndSettle();
      expect(c.calls.where((r) => r['op'] == 'save_backups'), isEmpty);
      expect(c.saved['a'], hasLength(1));
      expect(c.saved['b'], hasLength(1));
      expect(find.text('Beta'), findsOneWidget);
    },
  );
}
