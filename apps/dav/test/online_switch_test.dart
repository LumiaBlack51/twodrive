import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';

class OnlineClient extends StorageClient {
  OnlineClient() {
    available = true;
    selected = active;
    profiles = [
      {'id': 'a', 'revision': 'ra'},
      {'id': 'b', 'revision': 'rb'},
    ];
    status = snapshot();
  }
  String active = 'a';
  final calls = <Json>[];
  Completer<Json>? heldList;
  Completer<Json>? heldStatus;
  int? heldListId;
  Json snapshot() => {
    'connected': true,
    'multi_connection': true,
    'profile': active,
    'connections': [
      {'id': 'a'},
      {'id': 'b'},
    ],
    'mounts': [
      {'profile': 'a', 'revision': 'ra', 'path': '/synthetic/a'},
      {'profile': 'b', 'revision': 'rb', 'path': '/synthetic/b'},
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
    if (op == 'select') {
      active = args['id'] as String;
      return {'retained': true, 'status': snapshot()};
    }
    if (op == 'status') {
      final held = heldStatus;
      heldStatus = null;
      return held?.future ?? Future.value(snapshot());
    }
    if (op == 'profiles') return {'profiles': profiles, 'selected': active};
    if (op == 'list') {
      if (args['path'] == '/slow' && heldList != null) {
        heldListId = requestId;
        return heldList!.future;
      }
      return {
        'path': args['path'],
        'entries': [
          {
            'name': 'file-${args['profile']}',
            'path': '${args['path']}/file',
            'directory': false,
          },
        ],
      };
    }
    if (op == 'cancel') {
      if (args['id'] == heldListId && heldList?.isCompleted == false) {
        heldList!.completeError(StateError('display cancelled'));
      }
    }
    return {};
  }
}

void main() {
  test(
    'online switch uses only selection, preserves mounts and restores fresh folders without login or relisting',
    () async {
      final c = OnlineClient();
      await c.readDirectory('/folder-a');
      final activities = <String>[];
      final busySamples = <bool>[];
      c.addListener(() {
        activities.add(c.activity);
        busySamples.add(c.busy);
      });
      await c.connect('b');
      await Future<void>.delayed(Duration.zero);
      await c.readDirectory('/folder-b');
      final listings = c.calls.where((v) => v['op'] == 'list').length;
      await c.connect('a');
      await Future<void>.delayed(Duration.zero);
      expect(c.path, '/folder-a');
      expect(c.entries.single['name'], 'file-a');
      expect(c.entries.single['profile'], 'a');
      await c.connect('b');
      await Future<void>.delayed(Duration.zero);
      expect(c.path, '/folder-b');
      expect(c.entries.single['name'], 'file-b');
      expect(c.calls.where((v) => v['op'] == 'list').length, listings);
      expect(
        c.calls.where(
          (v) => ['connect', 'mount', 'disconnect'].contains(v['op']),
        ),
        isEmpty,
      );
      expect(c.calls.where((v) => v['op'] == 'select'), hasLength(3));
      expect(activities.any((v) => v.contains('登录')), isFalse);
      expect(busySamples, everyElement(false));
      expect(c.mounts, hasLength(2));
      c.dispose();
    },
  );
  test(
    'switch cancels an old display read and its late failure cannot clear the new account',
    () async {
      final c = OnlineClient()..heldList = Completer<Json>();
      final old = c.readDirectory('/slow');
      await Future<void>.delayed(Duration.zero);
      await c.connect('b');
      expect(c.calls.where((v) => v['op'] == 'cancel'), isNotEmpty);
      if (!c.heldList!.isCompleted) {
        c.heldList!.completeError(StateError('late failure'));
      }
      await old;
      expect(c.selected, 'b');
      expect(c.error, isNull);
      c.dispose();
    },
  );
  test(
    'status started before a selection cannot restore the old account afterwards',
    () async {
      final c = OnlineClient();
      final old = c.snapshot();
      final held = Completer<Json>();
      c.heldStatus = held;
      final pending = c.refreshStatus();
      await c.connect('b');
      held.complete(old);
      await pending;
      expect(c.selected, 'b');
      expect(c.status['profile'], 'b');
      c.dispose();
    },
  );
}
