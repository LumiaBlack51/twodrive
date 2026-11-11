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
  @override
  Future<Json> request(String op, [Json args = const {}, int? requestId]) {
    events.add('engine:$op');
    if (op == 'quit') {
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
}
