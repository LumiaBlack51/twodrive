import 'dart:async';
import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';

class StartupClient extends StorageClient {
  StartupClient() {
    available = true;
    selected = 'b';
    profiles = [
      for (final id in ['a', 'b', 'new'])
        {
          'id': id,
          'name': 'Drive $id',
          'revision': 'rev-$id',
          'startup_ready': id != 'new',
          'mount_path': 'D:\\TwoDrive\\$id',
        },
    ];
    status = {
      'multi_connection': true,
      'connections': <Json>[],
      'mounts': <Json>[],
      'mount_support': {'supported': true},
      'capabilities': {'mount': true},
    };
  }
  final calls = <Json>[];
  String? failResume;
  String? failMount;
  Completer<void>? gate;
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    if (!['status', 'profiles', 'list'].contains(op)) {
      calls.add({'op': op, ...args});
    }
    if (op == 'profiles') {
      return {'profiles': profiles, 'selected': status['profile'] ?? 'b'};
    }
    if (op == 'status') return status;
    if (op == 'list') return {'entries': <Json>[]};
    if (op == 'resume' || op == 'connect') {
      final id = args['id'];
      if (op == 'resume') expect(args['revision'], 'rev-$id');
      if (gate != null) await gate!.future;
      if (failResume == id) throw StateError('授权需要处理');
      status = {
        ...status,
        'connected': true,
        'profile': id,
        'connections': [
          ...status['connections'] as List,
          {'id': id},
        ],
      };
      return {};
    }
    if (op == 'mount') {
      final id = args['profile'];
      expect(args['revision'], 'rev-$id');
      final starting = calls.any((v) => v['op'] == 'resume' && v['id'] == id);
      expect(args['startup'], starting ? true : isNull);
      if (failMount == id) throw StateError('目录已被占用');
      status = {
        ...status,
        'mounts': [
          ...status['mounts'] as List,
          {'profile': id, 'revision': 'rev-$id', 'path': 'D:\\TwoDrive\\$id'},
        ],
      };
      return {};
    }
    if (op == 'disconnect') {
      final remaining = (status['connections'] as List)
          .where((p) => p['id'] != args['id'])
          .toList();
      status = {
        ...status,
        'connected': remaining.isNotEmpty,
        'profile': remaining.isEmpty ? null : remaining.first['id'],
        'connections': remaining,
        'mounts': (status['mounts'] as List)
            .where((m) => m['profile'] != args['id'])
            .toList(),
      };
      // Like the engine, disconnect does not remove saved authentication.
      return {};
    }
    if (op == 'cancel') {
      gate!.completeError(StateError('恢复已取消'));
      return {};
    }
    if (op == 'quit') return {};
    throw StateError('Unexpected startup request: $op');
  }

  @override
  Future<void> closeTransport() async {}
}

void main() {
  for (final restoreEnabled in [true, false]) {
    for (final autoMount in [true, false]) {
      test(
        'disconnect then restart: restore=$restoreEnabled mount=$autoMount',
        () async {
          final root = await Directory.systemTemp.createTemp(
            'twodrive-disconnect-restart-',
          );
          final first = StartupClient();
          final restarted = StartupClient();
          try {
            final file = File('${root.path}/desktop-settings.json');
            await first.preferences.load(file);
            await first.setAutoMountOnConnect(autoMount);
            await first.setRestoreConnectionsOnStartup(restoreEnabled);
            await first.connect('b');
            expect(first.isProfileConnected('b'), isTrue);
            await first.disconnect('b');
            expect(first.isProfileConnected('b'), isFalse);
            expect(
              first.profiles.singleWhere(
                (p) => p['id'] == 'b',
              )['startup_ready'],
              isTrue,
            );
            restarted.profiles = first.profiles
                .map((p) => Map<String, dynamic>.from(p))
                .toList();
            await restarted.preferences.load(file);
            await restarted.restoreOnStartup();
            expect(
              restarted.calls
                  .where((v) => v['op'] == 'resume')
                  .map((v) => v['id']),
              restoreEnabled ? ['a', 'b'] : isEmpty,
            );
            expect(
              restarted.calls
                  .where((v) => v['op'] == 'mount')
                  .map((v) => v['profile']),
              restoreEnabled && autoMount ? ['a', 'b'] : isEmpty,
            );
            expect(restarted.isProfileConnected('b'), restoreEnabled);
            expect(
              restarted.isProfileMounted('b'),
              restoreEnabled && autoMount,
            );
            expect(restarted.selected, 'b');
            expect(restarted.error, isNull);
            if (!restoreEnabled) {
              await restarted.connect('b');
              expect(restarted.isProfileConnected('b'), isTrue);
              final manualMounts = restarted.calls
                  .where((v) => v['op'] == 'mount')
                  .toList();
              expect(manualMounts, hasLength(autoMount ? 1 : 0));
              if (autoMount) {
                expect(manualMounts.single['profile'], 'b');
                expect(manualMounts.single.containsKey('startup'), isFalse);
              }
            }
          } finally {
            first.dispose();
            restarted.dispose();
            await root.delete(recursive: true);
          }
        },
      );
    }
  }
  test(
    'enabling startup restoration takes effect on the next process only',
    () async {
      final c = StartupClient();
      await c.setRestoreConnectionsOnStartup(false);
      await c.restoreOnStartup();
      await c.setRestoreConnectionsOnStartup(true);
      await c.restoreOnStartup();
      expect(c.calls, isEmpty);
      c.dispose();
    },
  );

  test(
    'persisted disabled automatic mount restores connections without mounts',
    () async {
      final root = await Directory.systemTemp.createTemp(
        'twodrive-startup-pref-',
      );
      final first = StartupClient();
      final restarted = StartupClient();
      try {
        final file = File('${root.path}/desktop-settings.json');
        await first.preferences.load(file);
        await first.setAutoMountOnConnect(false);
        await restarted.preferences.load(file);
        await restarted.restoreOnStartup();
        expect(
          restarted.calls.where((v) => v['op'] == 'resume').map((v) => v['id']),
          ['a', 'b'],
        );
        expect(restarted.connectedProfiles, hasLength(2));
        expect(restarted.calls.where((v) => v['op'] == 'mount'), isEmpty);
        expect(restarted.mounts, isEmpty);
        expect(restarted.selected, 'b');
        expect(restarted.error, isNull);
      } finally {
        first.dispose();
        restarted.dispose();
        await root.delete(recursive: true);
      }
    },
  );

  test(
    'normal startup restores four saved clouds even with only two previously online',
    () async {
      final c = StartupClient()
        ..selected = 'onedrive'
        ..profiles = [
          for (final id in ['onedrive', 'icloud', 'baidu', 'google', 'new'])
            {
              'id': id,
              'name': 'Drive $id',
              'revision': 'rev-$id',
              'startup_ready': id != 'new',
            },
        ];
      // The normal path uses saved sessions, not the previous mount subset.
      c.status['connections'] = [
        {'id': 'baidu'},
        {'id': 'google'},
      ];
      await c.restoreOnStartup();
      expect(c.calls.where((v) => v['op'] == 'resume').map((v) => v['id']), [
        'icloud',
        'baidu',
        'google',
        'onedrive',
      ]);
      expect(
        c.calls.where((v) => v['op'] == 'mount').map((v) => v['profile']),
        ['icloud', 'baidu', 'google', 'onedrive'],
      );
      expect(c.mounts.length, 4);
      expect(c.selected, 'onedrive');
      expect(c.error, isNull);
      c.dispose();
    },
  );
  test(
    'startup resumes and mounts all saved accounts once, keeps preferred view and skips new login',
    () async {
      final c = StartupClient();
      final first = c.restoreOnStartup();
      final second = c.restoreOnStartup();
      expect(identical(first, second), true);
      await first;
      expect(c.calls.map((v) => v['op']).toList(), [
        'resume',
        'mount',
        'resume',
        'mount',
      ]);
      expect(c.calls.where((v) => v['op'] == 'resume').map((v) => v['id']), [
        'a',
        'b',
      ]);
      expect(c.connectedProfiles.length, 2);
      expect(c.mounts.map((v) => v['path']), [
        r'D:\TwoDrive\a',
        r'D:\TwoDrive\b',
      ]);
      expect(c.selected, 'b');
      expect(c.error, isNull);
      await c.restoreOnStartup();
      expect(c.calls.length, 4);
      c.dispose();
    },
  );
  test(
    'failed account cannot prevent a healthy account from restoring and mounting',
    () async {
      final c = StartupClient()..failResume = 'a';
      await c.restoreOnStartup();
      expect(c.mounts.single['profile'], 'b');
      expect(c.error, contains('Drive a：连接恢复失败'));
      expect(c.selected, 'b');
      c.dispose();
    },
  );
  test('failed mount retains both connections and reports its owner', () async {
    final c = StartupClient()..failMount = 'a';
    await c.restoreOnStartup();
    expect(c.connectedProfiles.length, 2);
    expect(c.mounts.single['profile'], 'b');
    expect(c.error, contains('Drive a：自动挂载失败'));
    c.dispose();
  });
  test(
    'unsupported mount platform restores connections without bogus mount errors',
    () async {
      final c = StartupClient();
      c.status['mount_support'] = {'supported': false};
      await c.restoreOnStartup();
      expect(c.connectedProfiles.length, 2);
      expect(c.calls.any((v) => v['op'] == 'mount'), false);
      expect(c.error, isNull);
      c.dispose();
    },
  );
  test(
    'quit cancels startup restoration and does not restore the next account',
    () async {
      final c = StartupClient()..gate = Completer<void>();
      final restore = c.restoreOnStartup();
      await Future<void>.value();
      expect(c.busy, true);
      expect(await c.shutdown(), true);
      await restore;
      expect(c.calls.map((v) => v['op']), ['resume', 'cancel', 'quit']);
      expect(c.available, false);
      expect(c.error, isNull);
      c.dispose();
    },
  );
}
