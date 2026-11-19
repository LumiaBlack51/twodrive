import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';
import 'package:twodrive_dav/workspace.dart';

class WindowQuitClient extends StorageClient {
  WindowQuitClient(this.events) {
    available = true;
    status = {'connected': true, 'profile': 'saved'};
  }
  final List<String> events;
  final acknowledgement = Completer<Json>();
  final transport = Completer<void>();
  bool reject = false;
  List<Json> quitResults = [];
  final quitArguments = <Json>[];
  @override
  Future<Json> request(String op, [Json args = const {}, int? requestId]) {
    events.add('engine:$op');
    if (op == 'quit') {
      quitArguments.add(args);
      if (quitResults.isNotEmpty) return Future.value(quitResults.removeAt(0));
      if (reject) return Future.error(StateError('待上传内容需要保留'));
      return acknowledgement.future;
    }
    if (op == 'status') return Future.value(status);
    return Future.value({});
  }

  @override
  Future<void> closeTransport() => transport.future;
}

void main() {
  final events = <String>[];
  setUp(() {
    events.clear();
    trayReady = false;
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(const MethodChannel('window_manager'), (
          call,
        ) async {
          events.add('window:${call.method}');
          if (call.method == 'isMinimized' || call.method == 'isMaximized') {
            return false;
          }
          if (call.method == 'getBounds') {
            return {'x': 0.0, 'y': 0.0, 'width': 1120.0, 'height': 760.0};
          }
          return null;
        });
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(const MethodChannel('twodrive/status_tray'), (
          call,
        ) async {
          events.add('tray:${call.method}');
          return null;
        });
  });
  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(const MethodChannel('window_manager'), null);
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('twodrive/status_tray'),
          null,
        );
    trayReady = false;
  });

  testWidgets(
    'quit hides the window before waiting for backend and pipe cleanup',
    (tester) async {
      final client = WindowQuitClient(events);
      await tester.pumpWidget(DavApp(client: client, desktop: true));
      await tester.pump();
      events.clear();
      final dynamic state = tester.state(find.byType(Workspace));
      final Future<void> done = state.quit();
      await tester.pump();
      expect(events.first, 'window:hide');
      expect(events, contains('engine:quit'));
      expect(events, isNot(contains('window:destroy')));
      client.acknowledgement.complete({});
      await tester.pump();
      expect(events, isNot(contains('window:destroy')));
      await tester.pump(const Duration(seconds: 3));
      await done;
      expect(events.where((v) => v == 'window:destroy'), hasLength(1));
      expect(client.quitting, true);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets(
    'refused quit restores the window and keeps the engine and mounts available',
    (tester) async {
      final client = WindowQuitClient(events)..reject = true;
      await tester.pumpWidget(DavApp(client: client, desktop: true));
      await tester.pump();
      events.clear();
      final dynamic state = tester.state(find.byType(Workspace));
      final Future<void> done = state.quit();
      await tester.pumpAndSettle();
      await done;
      expect(events, contains('window:show'));
      expect(events, isNot(contains('window:destroy')));
      expect(events, isNot(contains('tray:destroy')));
      expect(client.available, true);
      expect(client.connected, true);
      expect(client.quitting, false);
      expect(client.error, contains('待上传内容'));
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets('repeated quit actions share one window and tray teardown', (
    tester,
  ) async {
    final client = WindowQuitClient(events);
    client.transport.complete();
    await tester.pumpWidget(DavApp(client: client, desktop: true));
    await tester.pump();
    events.clear();
    final dynamic state = tester.state(find.byType(Workspace));
    final Future<void> first = state.quit();
    final Future<void> second = state.quit();
    client.acknowledgement.complete({});
    // The mocked destroy keeps the widget alive with its exiting spinner.
    // Wait for shutdown itself rather than waiting for that animation to stop.
    await tester.pump(const Duration(seconds: 3));
    await Future.wait([first, second]);
    expect(events.where((v) => v == 'engine:quit'), hasLength(1));
    expect(events.where((v) => v == 'tray:destroy'), hasLength(1));
    expect(events.where((v) => v == 'window:destroy'), hasLength(1));
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });

  for (final accept in [false, true]) {
    testWidgets('mounted programs require confirmation before exit ($accept)', (
      tester,
    ) async {
      final program = {'pid': 1234, 'start_time': 98765, 'name': 'writer'};
      final client = WindowQuitClient(events)
        ..quitResults = [
          {
            'quit_blockers': [
              {
                'name': 'OneDrive',
                'path': '/synthetic/mount',
                'programs': [program],
              },
            ],
          },
          {},
        ];
      client.transport.complete();
      await tester.pumpWidget(DavApp(client: client, desktop: true));
      await tester.pump();
      events.clear();
      final dynamic state = tester.state(find.byType(Workspace));
      final Future<void> done = state.quit();
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.text('挂载目录正在使用'), findsOneWidget);
      expect(find.textContaining('writer'), findsOneWidget);
      expect(client.error, isNull);
      expect(client.quitArguments, [
        {'mount_confirmation': true},
      ]);
      expect(events, isNot(contains('window:destroy')));

      await tester.tap(find.text(accept ? '关闭程序并退出' : '取消'));
      await tester.pump(const Duration(seconds: 1));
      await done;
      if (accept) {
        expect(client.quitArguments, [
          {'mount_confirmation': true},
          {
            'mount_confirmation': true,
            'close_mount_processes': [
              {'pid': 1234, 'start_time': 98765},
            ],
          },
        ]);
        expect(events, contains('window:destroy'));
      } else {
        expect(client.quitArguments, [
          {'mount_confirmation': true},
        ]);
        expect(client.available, isTrue);
        expect(events, isNot(contains('window:destroy')));
      }
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    });
  }

  testWidgets(
    'a protected mount holder is displayed and excluded from confirmation',
    (tester) async {
      final client = WindowQuitClient(events)
        ..quitResults = [
          {
            'quit_blockers': [
              {
                'name': 'OneDrive',
                'path': '/synthetic/mount',
                'programs': [
                  {'pid': 1234, 'start_time': 98765, 'name': 'writer'},
                  {
                    'pid': 5678,
                    'start_time': 43210,
                    'name': 'launch-shell',
                    'can_close': false,
                  },
                ],
              },
            ],
          },
          {},
        ];
      client.transport.complete();
      await tester.pumpWidget(DavApp(client: client, desktop: true));
      await tester.pump();
      final dynamic state = tester.state(find.byType(Workspace));
      final Future<void> done = state.quit();
      await tester.pumpAndSettle();
      expect(
        find.text('launch-shell（PID 5678） · 请手动关闭或离开挂载目录'),
        findsOneWidget,
      );
      await tester.tap(find.text('关闭程序并退出'));
      await tester.pump(const Duration(seconds: 1));
      await done;
      expect(client.quitArguments.last['close_mount_processes'], [
        {'pid': 1234, 'start_time': 98765},
      ]);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets(
    'an unidentified mount holder can be retried without closing apps',
    (tester) async {
      final client = WindowQuitClient(events)
        ..quitResults = [
          {
            'quit_blockers': [
              {'name': 'OneDrive', 'path': '/synthetic/mount', 'programs': []},
            ],
          },
          {},
        ];
      client.transport.complete();
      await tester.pumpWidget(DavApp(client: client, desktop: true));
      await tester.pump();
      final dynamic state = tester.state(find.byType(Workspace));
      final Future<void> done = state.quit();
      await tester.pumpAndSettle();
      expect(find.text('/synthetic/mount'), findsOneWidget);
      expect(find.text('关闭程序并退出'), findsNothing);
      await tester.tap(find.text('重试退出'));
      await tester.pump();
      await done;
      expect(client.quitArguments, [
        {'mount_confirmation': true},
        {'mount_confirmation': true},
      ]);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );
}
