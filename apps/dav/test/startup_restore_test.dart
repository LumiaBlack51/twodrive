import 'dart:async';
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
    if (op == 'resume') {
      final id = args['id'];
      expect(args['revision'], 'rev-$id');
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
      expect(args['startup'], true);
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
