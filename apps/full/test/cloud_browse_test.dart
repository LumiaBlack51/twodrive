import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_full/engine_client.dart';

Map<String, dynamic> response(
  Map<String, dynamic> request,
  String? query, {
  String status = 'complete',
  int page = 1,
  String name = '中文 # %.pdf',
  String auth = 'signed_in',
}) => {
  'version': 1,
  'id': request['id'],
  'ok': true,
  'snapshot': {
    'auth_status': auth,
    'capabilities': ['cloud_browse'],
    'directory': {
      'query_id': query,
      'status': status,
      'page_number': page,
      'page': {
        'account_id': 'account',
        'drive_id': 'drive',
        'item_id': 'root',
        'has_more': status == 'partial',
        'items': [
          {
            'id': '$page',
            'name': name,
            'kind': 'file',
            'size': 42,
            'modified': '2026-09-16T00:00:00Z',
          },
        ],
      },
    },
  },
};

void main() {
  test(
    'rapid directory change ignores late old query and queues latest navigation',
    () async {
      final pending = Completer<Map<String, dynamic>>();
      Map<String, dynamic>? old;
      final client = EngineClient(
        '',
        '',
        transport: (r) async {
          if (old == null) {
            old = r;
            return pending.future;
          }
          return response(r, (r['command'] as Map)['query_id'], name: 'new');
        },
      );
      client.browse();
      final oldId = client.queryId;
      client.browse(driveId: 'drive', itemId: 'child', name: '目录');
      expect(client.cloudItems, isEmpty);
      expect(client.directory!['status'], 'loading');
      pending.complete(response(old!, oldId, name: 'old'));
      await Future<void>.delayed(Duration.zero);
      await Future<void>.delayed(Duration.zero);
      expect(client.cloudItems.single['name'], 'new');
      expect(client.breadcrumbs.single['id'], 'child');
      client.dispose();
    },
  );

  test(
    'pagination accumulates once and does not mistake a pending page for empty',
    () async {
      int number = 1;
      String status = 'partial';
      late EngineClient client;
      client = EngineClient(
        '',
        '',
        transport: (r) async =>
            response(r, client.queryId, status: status, page: number),
      );
      client.browse();
      await Future<void>.delayed(Duration.zero);
      expect(client.cloudItems.single['name'], '中文 # %.pdf');
      await client.send({'type': 'snapshot'});
      expect(client.cloudItems.length, 1);
      status = 'loading';
      await client.send({'type': 'snapshot'});
      expect(client.cloudItems.length, 1);
      number = 2;
      status = 'complete';
      await client.send({'type': 'snapshot'});
      expect(client.cloudItems.length, 2);
      client.dispose();
    },
  );

  test(
    'logout and cancellation cannot restore a late directory response',
    () async {
      for (final logout in [false, true]) {
        final pending = Completer<Map<String, dynamic>>();
        Map<String, dynamic>? old;
        final client = EngineClient(
          '',
          '',
          transport: (r) async {
            if (old == null) {
              old = r;
              return pending.future;
            }
            return response(r, null, auth: logout ? 'signed_out' : 'signed_in');
          },
        );
        client.browse();
        final oldId = client.queryId;
        if (logout) {
          await client.logout();
        } else {
          await client.cancelBrowse();
        }
        pending.complete(response(old!, oldId));
        await Future<void>.delayed(Duration.zero);
        await Future<void>.delayed(Duration.zero);
        expect(client.cloudItems, isEmpty);
        expect(client.queryId, isNull);
        client.dispose();
      }
    },
  );
}
