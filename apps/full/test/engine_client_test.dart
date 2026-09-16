import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_full/engine_client.dart';

Map<String,dynamic> reply(Map<String,dynamic> request, {bool paused=false}) =>
  {'version':1,'id':request['id'],'ok':true,'error':null,
    'snapshot':{'paused':paused,'capabilities':['pause'],'status':paused?'paused':'mock_idle'}};
void main() {
  test('disconnect discards stale success; reconnect clears transport error', () async {
    bool fail = false;
    final client = EngineClient('', '', transport: (request) async {
      if (fail) throw Exception('pipe closed');
      return reply(request);
    });
    await client.send({'type':'snapshot'});
    expect(client.can('pause'), isTrue);
    fail = true;
    await client.send({'type':'snapshot'});
    expect(client.snapshot, isNull);
    expect(client.can('pause'), isFalse);
    expect(client.error, EngineClient.disconnected);
    fail = false;
    await client.send({'type':'snapshot'});
    expect(client.error, isNull);
    expect(client.can('pause'), isTrue);
    client.dispose();
  });
  test('pause remains pending until matching backend acknowledgement', () async {
    final completion = Completer<Map<String,dynamic>>();
    Map<String,dynamic>? mutation;
    final client = EngineClient('', '', transport: (request) async {
      if ((request['command'] as Map)['type'] == 'snapshot') return reply(request);
      mutation = request;
      return completion.future;
    });
    await client.send({'type':'snapshot'});
    final pending = client.send({'type':'set_paused','paused':true});
    expect(client.pendingMutation, isTrue);
    expect(client.snapshot!['paused'], isFalse);
    completion.complete(reply(mutation!, paused:true));
    await pending;
    expect(client.pendingMutation, isFalse);
    expect(client.snapshot!['paused'], isTrue);
    client.dispose();
  });
  test('mismatched protocol or response id cannot supply UI state', () async {
    final client = EngineClient('', '', transport: (request) async =>
      {...reply(request), 'id':'another-request'});
    await client.send({'type':'snapshot'});
    expect(client.snapshot, isNull);
    expect(client.can('pause'), isFalse);
    client.dispose();
  });
}
