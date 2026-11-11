import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/main.dart';

class MountClient extends StorageClient {
  MountClient({
    this.supported = true,
    this.connectFails = false,
    this.mountFails = false,
  }) {
    available = true;
    selected = 'personal';
    profiles = [
      {'id': 'personal', 'revision': 'test'},
    ];
  }
  final bool supported;
  final bool connectFails;
  final bool mountFails;
  final operations = <String>[];
  Json? mountOwner;
  Completer<void>? connection;
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    operations.add(op);
    switch (op) {
      case 'connect':
        if (connection != null) await connection!.future;
        if (connectFails) throw StateError('connection rejected');
        status = {
          'connected': true,
          'profile': 'personal',
          'mounted': false,
          'capabilities': {'mount': supported},
        };
        return {'entries': <Json>[]};
      case 'mount':
        mountOwner = Map<String, dynamic>.from(args);
        if (mountFails) throw StateError('mount unavailable');
        status = {...status, 'mounted': true};
        return Map<String, dynamic>.from(status);
      case 'status':
        return Map<String, dynamic>.from(status);
      case 'list':
        return {'entries': <Json>[]};
      default:
        return {};
    }
  }
}

void main() {
  test(
    'successful connection automatically mounts once on Linux only',
    () async {
      final client = MountClient();
      final expectedMounts = Platform.isLinux ? 1 : 0;
      await client.connect();
      expect(
        client.operations.where((op) => op == 'mount'),
        hasLength(expectedMounts),
      );
      expect(
        client.mountOwner,
        Platform.isLinux ? {'profile': 'personal', 'revision': 'test'} : null,
      );
      expect(client.connected, isTrue);
      expect(client.mounted, Platform.isLinux);
      await client.connect();
      expect(
        client.operations.where((op) => op == 'mount'),
        hasLength(expectedMounts),
      );
      client.dispose();
    },
  );
  test('unsupported providers and rejected connections never mount', () async {
    for (final client in [
      MountClient(supported: false),
      MountClient(connectFails: true),
    ]) {
      await client.connect();
      expect(client.operations, isNot(contains('mount')));
      client.dispose();
    }
  });
  test(
    'mount failure preserves connected account and reports error',
    () async {
      final client = MountClient(mountFails: true);
      await client.connect();
      expect(client.connected, isTrue);
      expect(client.mounted, isFalse);
      expect(client.error, contains('mount unavailable'));
      client.dispose();
    },
    skip: !Platform.isLinux,
  );
  testWidgets('settings navigation stays usable while connection is pending', (
    tester,
  ) async {
    final client = MountClient()..connection = Completer<void>();
    await tester.pumpWidget(DavApp(client: client));
    final connecting = client.connect();
    await tester.pump();
    await tester.tap(find.text('设置'));
    await tester.pump();
    expect(find.text('自动检查更新'), findsOneWidget);
    expect(tester.takeException(), isNull);
    client.connection!.complete();
    await tester.pumpAndSettle();
    await connecting;
    await tester.pumpWidget(const SizedBox());
    client.dispose();
  });
}
