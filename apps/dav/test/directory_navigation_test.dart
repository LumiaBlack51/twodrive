import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/workspace.dart';

class DirectoryClient extends StorageClient {
  DirectoryClient() {
    available = true;
    selected = 'a';
    profiles = [
      {'id': 'a', 'revision': 'ra', 'name': 'First account', 'kind': 'dav'},
      {'id': 'b', 'revision': 'rb', 'name': 'Second account', 'kind': 'dav'},
    ];
    status = snapshot('a');
  }

  final lists = <({Json args, int id, Completer<Json> reply})>[];
  final calls = <String>[];
  Completer<Json>? heldStatus;
  List<Json>? nextProfiles;

  Json snapshot(String id) => {
    'connected': true,
    'profile': id,
    'multi_connection': true,
    'connections': [
      {'id': 'a'},
      {'id': 'b'},
    ],
    'capabilities': {'delete': true},
  };

  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add(op);
    switch (op) {
      case 'status':
        return heldStatus?.future ?? status;
      case 'profiles':
        return {'profiles': nextProfiles ?? profiles, 'selected': selected};
      case 'select':
        return {'status': snapshot(args['id'] as String)};
      case 'list':
        final reply = Completer<Json>();
        lists.add((args: args, id: requestId!, reply: reply));
        return reply.future;
      default:
        return {};
    }
  }

  void finishList(int index, {String? name}) {
    final list = lists[index];
    list.reply.complete({
      'entries': [
        if (name != null)
          {
            'path': list.args['path'] == '/'
                ? '/$name'
                : '${list.args['path']}/$name',
            'name': name,
            'directory': false,
            'size': 1,
            'etag': 'version',
          },
      ],
    });
  }
}

Future<void> openFiles(WidgetTester tester, DirectoryClient client) async {
  await tester.pumpWidget(DavApp(client: client));
  await tester.tap(find.text('文件').first);
  await tester.pump();
}

void main() {
  testWidgets(
    'engine snapshots render before refresh and obsolete snapshot events are ignored',
    (tester) async {
      final c = DirectoryClient();
      await openFiles(tester, c);
      await tester.pump();
      final oldId = c.lists.single.id;
      c.accept({
        'id': oldId,
        'event': 'directory_snapshot',
        'profile': 'a',
        'revision': 'ra',
        'path': '/',
        'entries': [
          {
            'path': '/cached.txt',
            'name': 'cached.txt',
            'directory': false,
            'size': 5,
            'etag': 'old',
          },
        ],
      });
      await tester.pump();
      expect(find.text('cached.txt'), findsOneWidget);
      expect(c.directoryReady, isFalse);
      final next = c.browse('/new');
      await tester.pump();
      c.accept({
        'id': oldId,
        'event': 'directory_snapshot',
        'profile': 'a',
        'revision': 'ra',
        'path': '/',
        'entries': [
          {'path': '/obsolete.txt', 'name': 'obsolete.txt', 'directory': false},
        ],
      });
      await tester.pump();
      expect(find.text('obsolete.txt'), findsNothing);
      c.finishList(0, name: 'late-cloud.txt');
      c.finishList(1, name: 'fresh.txt');
      await next;
      await tester.pump();
      expect(find.text('fresh.txt'), findsOneWidget);
      expect(find.text('late-cloud.txt'), findsNothing);
      expect(c.directoryReady, isTrue);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets(
    'switching targets from Files resets the filter and keeps each directory',
    (tester) async {
      final c = DirectoryClient();
      await openFiles(tester, c);
      await tester.pump();
      c.finishList(0, name: 'first.txt');
      await tester.pumpAndSettle();
      final browse = c.browse('/saved-folder');
      await tester.pump();
      c.finishList(1, name: 'saved.txt');
      await browse;
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextField), 'saved');
      expect(find.text('saved.txt'), findsOneWidget);

      Future<void> choose(String account) async {
        await tester.tap(find.byKey(const ValueKey('files-target')));
        await tester.pumpAndSettle();
        await tester.tap(find.text(account).last);
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));
      }

      await choose('Second account');
      expect(c.selected, 'b');
      expect(c.lists.last.args, containsPair('profile', 'b'));
      expect(c.lists.last.args, containsPair('path', '/'));
      expect(find.text('saved.txt'), findsNothing);
      c.finishList(2, name: 'second.txt');
      await tester.pumpAndSettle();
      expect(find.text('second.txt'), findsOneWidget);
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text,
        '',
      );

      await choose('First account');
      await tester.pumpAndSettle();
      expect(c.path, '/saved-folder');
      expect(find.text('saved.txt'), findsOneWidget);
      expect(find.text('second.txt'), findsNothing);
      expect(c.calls.where((op) => op == 'select'), hasLength(2));
      expect(
        c.calls.where((op) => ['connect', 'disconnect', 'mount'].contains(op)),
        isEmpty,
      );
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  testWidgets('showing Files automatically reads the selected root', (
    tester,
  ) async {
    final c = DirectoryClient();
    await openFiles(tester, c);
    await tester.pump();
    expect(find.text('读取目录'), findsNothing);
    expect(c.lists.single.args['path'], '/');
    expect(c.lists.single.args['profile'], 'a');
    c.finishList(0, name: 'auto-loaded.txt');
    await tester.pump();
    expect(find.text('auto-loaded.txt'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });

  testWidgets('View Files on a connection opens its directory immediately', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1440, 1000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final c = DirectoryClient();
    await openFiles(tester, c);
    c.finishList(0);
    await tester.pump();
    await tester.tap(find.text('连接').first);
    await tester.pumpAndSettle();
    final viewFiles = find.descendant(
      of: find.byKey(const ValueKey('connection-b')),
      matching: find.text('查看文件'),
    );
    await tester.ensureVisible(viewFiles);
    await tester.pumpAndSettle();
    await tester.tap(viewFiles);
    await tester.pump();
    expect(c.lists.last.args['profile'], 'b');
    expect(find.text('读取目录'), findsNothing);
    c.finishList(1, name: 'from-connected-account.txt');
    await tester.pump();
    expect(find.text('from-connected-account.txt'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });

  testWidgets('Files renders a cold list while aggregate status is pending', (
    tester,
  ) async {
    final c = DirectoryClient()..heldStatus = Completer<Json>();
    c.setView(visible: true, files: true);
    final refresh = c.refreshView();
    await openFiles(tester, c);
    expect(c.calls, ['list', 'status']);
    expect(find.text('正在加载目录'), findsOneWidget);
    expect(find.text('当前目录为空'), findsNothing);
    expect(find.text('0 项'), findsNothing);

    c.finishList(0, name: '云端文件.txt');
    await tester.pump();
    expect(find.text('云端文件.txt'), findsOneWidget);
    expect(c.heldStatus!.isCompleted, isFalse);
    c.heldStatus!.complete(c.snapshot('a'));
    await refresh;
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });

  testWidgets('back navigation shows the last successful list during refresh', (
    tester,
  ) async {
    final c = DirectoryClient();
    final root = c.browse('/');
    c.finishList(0, name: 'old.txt');
    await root;
    final child = c.browse('/folder');
    c.finishList(1);
    await child;
    await openFiles(tester, c);
    expect(find.text('当前目录为空'), findsOneWidget);

    await tester.tap(find.byTooltip('上一级'));
    await tester.pump();
    expect(find.text('old.txt'), findsOneWidget);
    expect(find.text('当前目录为空'), findsNothing);
    expect(
      tester
          .widget<PopupMenuButton<String>>(find.byType(PopupMenuButton<String>))
          .enabled,
      isFalse,
    );
    c.lists[2].reply.completeError(StateError('network unavailable'));
    await tester.pump();
    expect(find.text('old.txt'), findsOneWidget);

    await tester.tap(find.byTooltip('刷新'));
    await tester.pump();
    c.finishList(3);
    await tester.pump();
    expect(find.text('old.txt'), findsNothing);
    expect(find.text('当前目录为空'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    c.dispose();
  });

  test(
    'reopening Files after switching accounts starts the new root read',
    () async {
      final c = DirectoryClient()..heldStatus = Completer<Json>();
      c.setView(visible: true, files: true);
      final old = c.refreshView();
      await Future<void>.delayed(Duration.zero);
      expect(c.lists.single.args['profile'], 'a');

      c.setView(visible: false);
      await c.connect('b');
      c.setView(visible: true, files: true);
      final reopened = c.refreshView(forceDirectory: true);
      await Future<void>.delayed(Duration.zero);
      expect(c.lists.map((read) => read.args['profile']), ['a', 'b']);

      c.finishList(1, name: 'visible-now.txt');
      await Future<void>.delayed(Duration.zero);
      expect(c.entries.single['name'], 'visible-now.txt');
      expect(c.entries.single['profile'], 'b');
      c.finishList(0, name: 'old-account.txt');
      c.heldStatus!.complete(c.snapshot('a'));
      await Future.wait([old, reopened]);
      expect(c.entries.single['profile'], 'b');
      c.dispose();
    },
  );

  testWidgets(
    'visible Files still loads the selected account after window blur',
    (tester) async {
      final c = DirectoryClient()..setView(visible: true, files: true);
      await openFiles(tester, c);
      expect(find.text('读取目录'), findsNothing);
      final dynamic workspace = tester.state(find.byType(Workspace));
      workspace.onWindowBlur();
      await c.connect('b');
      await tester.pump();
      expect(c.lists.map((read) => read.args['profile']), ['a', 'b']);
      expect(find.text('正在加载目录'), findsOneWidget);
      c.finishList(0);
      c.finishList(1, name: 'selected-account.txt');
      await tester.pump();
      expect(find.text('selected-account.txt'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );

  test(
    'late directory response cannot replace a newer path or account',
    () async {
      final c = DirectoryClient();
      final old = c.browse('/old');
      final newer = c.browse('/new');
      expect(c.lists.map((read) => read.args['path']), ['/old', '/new']);
      c.finishList(1, name: 'new.txt');
      await newer;
      c.finishList(0, name: 'old.txt');
      await old;
      expect(c.entries.single['name'], 'new.txt');

      await c.connect('b');
      final other = c.browse('/new');
      expect(c.entries, isEmpty);
      expect(c.lists.last.args['profile'], 'b');
      c.finishList(2, name: 'other.txt');
      await other;
      expect(c.entries.single['profile'], 'b');
      c.dispose();
    },
  );

  test(
    'a new connection revision discards cached and in-flight listings',
    () async {
      final c = DirectoryClient();
      final root = c.browse('/');
      c.finishList(0, name: 'previous-account.txt');
      await root;
      final held = c.browse('/folder');
      c.nextProfiles = [
        {'id': 'a', 'revision': 'new-revision'},
        c.profiles[1],
      ];
      await c.loadProfiles();
      expect(c.path, '/');
      expect(c.entries, isEmpty);
      c.finishList(1, name: 'previous-folder.txt');
      await held;
      expect(c.entries, isEmpty);

      final next = c.browse('/');
      expect(c.lists.last.args['revision'], 'new-revision');
      c.finishList(2, name: 'current-account.txt');
      await next;
      expect(c.entries.single['name'], 'current-account.txt');
      c.dispose();
    },
  );

  test(
    'background upload completion refreshes only the visible account',
    () async {
      final c = DirectoryClient()..setView(visible: true, files: true);
      final initial = c.browse('/');
      c.finishList(0, name: 'original.txt');
      await initial;

      void completeUpload(int id, String profile) => c.accept({
        'id': id,
        'event': 'operation',
        'op': 'upload',
        'profile': profile,
        'running': false,
      });

      completeUpload(10, 'b');
      await Future<void>.delayed(Duration.zero);
      expect(c.lists, hasLength(1));
      expect(c.directoryReady, isTrue);

      completeUpload(11, 'a');
      await Future<void>.delayed(Duration.zero);
      expect(c.lists, hasLength(2));
      expect(c.lists.last.args['profile'], 'a');
      c.finishList(1, name: 'uploaded.txt');
      await Future<void>.delayed(Duration.zero);
      expect(c.entries.single['name'], 'uploaded.txt');
      expect(c.directoryReady, isTrue);
      c.dispose();
    },
  );

  test(
    'a rename invalidates the previous contents of affected folders',
    () async {
      final c = DirectoryClient();
      final root = c.browse('/');
      c.finishList(0);
      await root;
      final folder = c.browse('/folder');
      c.finishList(1, name: 'old.txt');
      await folder;
      final back = c.browse('/');
      c.finishList(2);
      await back;
      final rename = c.mutate('rename', {'path': '/folder', 'to': '/new'});
      await Future<void>.delayed(Duration.zero);
      c.finishList(3);
      await rename;

      final revisit = c.browse('/folder');
      expect(c.entries, isEmpty);
      expect(c.directoryLoaded, isFalse);
      c.lists[4].reply.completeError(StateError('directory not found'));
      await revisit;
      expect(c.directoryError, 'directory not found');
      expect(c.directoryLoaded, isFalse);
      c.dispose();
    },
  );
}
