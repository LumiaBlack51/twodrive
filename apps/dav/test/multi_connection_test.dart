import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/connection_card.dart';
import 'package:twodrive_dav/storage_usage.dart';
import 'package:twodrive_dav/tray_panel.dart';

class MultiClient extends StorageClient {
  MultiClient() {
    available = true;
    selected = 'a';
    profiles = [
      {
        'id': 'a',
        'revision': 'ra',
        'name': 'iCloud Drive',
        'provider_label': 'iCloud Drive',
        'kind': 'icloud',
      },
      {
        'id': 'b',
        'revision': 'rb',
        'name': 'OneDrive',
        'provider_label': 'OneDrive',
        'kind': 'onedrive',
      },
    ];
    status = engineStatus();
  }
  String active = 'a';
  Completer<Json>? download;
  Completer<Json>? quota;
  bool failQuota = false;
  final calls = <Json>[];
  Json engineStatus() => {
    'connected': true,
    'profile': active,
    'multi_connection': true,
    'connections': [
      {'id': 'a'},
      {'id': 'b'},
    ],
  };
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add({'op': op, ...args, 'request_id': requestId});
    if (op == 'connect') {
      active = args['id'] as String;
      return {'retained': true};
    }
    if (op == 'status') return engineStatus();
    if (op == 'profiles') {
      return {'profiles': profiles, 'providers': [], 'selected': active};
    }
    if (op == 'list') return {'path': args['path'], 'entries': []};
    if (op == 'download') return download!.future;
    if (op == 'quota') {
      if (failQuota) throw StateError('synthetic quota failure');
      return quota?.future ??
          Future.value({
            'state': 'ready',
            'usage': {
              'total': 1000,
              'used': 200,
              'free': 800,
              'scope': 'account',
            },
          });
    }
    return {};
  }
}

Future<void> snapshot(WidgetTester tester, String name) async {
  final dir = Platform.environment['TWODRIVE_UI_SNAPSHOTS'];
  if (dir == null) return;
  final boundary = tester.renderObject<RenderRepaintBoundary>(
    find.byKey(const ValueKey('preview')),
  );
  await tester.runAsync(() async {
    final image = await boundary.toImage(pixelRatio: 1);
    final data = await image.toByteData(format: ui.ImageByteFormat.png);
    await File('$dir/$name.png').writeAsBytes(data!.buffer.asUint8List());
    image.dispose();
  });
}

void main() {
  final font = Platform.environment['TWODRIVE_UI_FONT'];
  setUpAll(() async {
    if (font != null) {
      await (FontLoader('PreviewFont')..addFont(
            File(font).readAsBytes().then((b) => ByteData.sublistView(b)),
          ))
          .load();
    }
    final icons = Platform.environment['TWODRIVE_UI_ICONS'];
    if (icons != null) {
      await (FontLoader('MaterialIcons')..addFont(
            File(icons).readAsBytes().then((b) => ByteData.sublistView(b)),
          ))
          .load();
    }
  });
  test(
    'switching view during download keeps transfer bound to its original account and restores each folder',
    () async {
      final c = MultiClient()
        ..path = '/folder-a'
        ..download = Completer<Json>();
      final transfer = c.transfer('download', {
        'profile': 'a',
        'revision': 'ra',
        'path': '/same.txt',
        'output': '/synthetic.bin',
      });
      expect(c.busy, false);
      expect(c.runningTransfers.length, 1);
      await c.connect('b');
      expect(c.selected, 'b');
      expect(c.isProfileConnected('a'), true);
      c.path = '/folder-b';
      await c.connect('a');
      expect(c.path, '/folder-a');
      await c.connect('b');
      expect(c.path, '/folder-b');
      c.download!.complete({'bytes': 12});
      await transfer;
      expect(c.transfers.single.args['profile'], 'a');
      expect(c.transfers.single.state, '已完成');
      expect(c.selected, 'b');
      c.dispose();
    },
  );
  test(
    'quota queries coalesce, keep per-account results and discard data after revision changes',
    () async {
      final c = MultiClient()..quota = Completer<Json>();
      final a = c.refreshQuota('a');
      final same = c.refreshQuota('a', force: true);
      expect(identical(a, same), true);
      expect(c.calls.where((v) => v['op'] == 'quota').length, 1);
      await c.connect('b');
      c.quota!.complete({
        'state': 'ready',
        'usage': {'total': 1000, 'used': 400, 'free': 600},
      });
      await a;
      expect(c.quotas['a']?['usage']['used'], 400);
      expect(c.quotas['b'], null);
      expect(c.selected, 'b');
      c.failQuota = true;
      await c.refreshQuota('a', force: true);
      expect(c.quotas['a']?['stale'], true);
      expect(c.error, null);
      c.profiles[0] = {...c.profiles[0], 'revision': 'new-account-generation'};
      await c.loadProfiles();
      expect(c.quotas['a'], null);
      c.dispose();
    },
  );
  testWidgets(
    'capacity distinguishes unknown, full and zero and cards fit narrow windows at larger text scale',
    (tester) async {
      tester.view.physicalSize = const Size(420, 700);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(
        MaterialApp(
          home: MediaQuery(
            data: const MediaQueryData(textScaler: TextScaler.linear(1.4)),
            child: Scaffold(
              body: SingleChildScrollView(
                child: Column(
                  children: [
                    ConnectionCard(
                      profile: const {
                        'id': 'a',
                        'name': 'iCloud Drive',
                        'kind': 'icloud',
                      },
                      online: true,
                      selected: true,
                      connectLabel: '查看文件',
                      usage: const {
                        'state': 'ready',
                        'usage': {
                          'total': 10737418240,
                          'used': 4294967296,
                          'free': 6442450944,
                        },
                      },
                      onRefresh: () {},
                    ),
                    const StorageUsageView(
                      value: {'state': 'unsupported'},
                      connected: true,
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.textContaining('4.0 GiB'), findsOneWidget);
      expect(find.text('此网盘暂不提供容量信息'), findsOneWidget);
      expect(tester.takeException(), isNull);
      expect(capacityBytes(null), '未提供');
      expect(capacityBytes(0), '0 B');
    },
  );
  testWidgets(
    'bold tray heading has an arrow and switches between retained accounts',
    (tester) async {
      tester.view.physicalSize = const Size(390, 600);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = MultiClient();
      c.quotas['a'] = {
        'state': 'ready',
        'usage': {'total': 10737418240, 'used': 4294967296, 'free': 6442450944},
      };
      await tester.pumpWidget(
        MaterialApp(
          theme: ThemeData(fontFamily: font == null ? null : 'PreviewFont'),
          home: RepaintBoundary(
            key: const ValueKey('preview'),
            child: TrayPanel(
              client: c,
              onManage: ([int page = 0]) async {},
              onOpenFolder: () async {},
              onQuit: () async {},
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.byIcon(Icons.expand_more), findsOneWidget);
      expect(find.textContaining('4.0 GiB'), findsOneWidget);
      expect(find.text('40%'), findsOneWidget);
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
      await snapshot(tester, 'tray-capacity-switcher');
      await tester.tap(find.byKey(const ValueKey('cloud-switcher')));
      await tester.pumpAndSettle();
      expect(find.text('当前浏览'), findsOneWidget);
      expect(find.text('已连接'), findsOneWidget);
      await tester.tap(find.text('OneDrive'));
      await tester.pumpAndSettle();
      expect(c.selected, 'b');
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
  testWidgets(
    'connection overview shows independent capacity and connection status without overflow',
    (tester) async {
      tester.view.physicalSize = const Size(1000, 600);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final c = MultiClient();
      await tester.pumpWidget(
        MaterialApp(
          theme: ThemeData(fontFamily: font == null ? null : 'PreviewFont'),
          home: RepaintBoundary(
            key: const ValueKey('preview'),
            child: Scaffold(
              body: Padding(
                padding: const EdgeInsets.all(20),
                child: Column(
                  children: [
                    const Align(
                      alignment: Alignment.centerLeft,
                      child: Text(
                        '连接',
                        style: TextStyle(
                          fontSize: 28,
                          fontWeight: FontWeight.bold,
                        ),
                      ),
                    ),
                    const SizedBox(height: 12),
                    for (final p in c.profiles)
                      ConnectionCard(
                        profile: p,
                        online: true,
                        selected: p['id'] == 'a',
                        connectLabel: '查看文件',
                        onSelect: () {},
                        onDisconnect: () {},
                        onRefresh: () {},
                        usage: {
                          'state': 'ready',
                          'usage': {
                            'total': 10737418240,
                            'used': p['id'] == 'a' ? 4294967296 : 1073741824,
                            'free': p['id'] == 'a' ? 6442450944 : 9663676416,
                          },
                        },
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.textContaining('4.0 GiB'), findsOneWidget);
      expect(find.textContaining('1.0 GiB'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await snapshot(tester, 'connections-capacity');
      c.dispose();
    },
  );
}
