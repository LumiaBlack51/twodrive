import 'dart:async';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';

class QuitClient extends StorageClient {
  final acknowledgement = Completer<Json>();
  bool reject = false;
  int quitRequests = 0;
  int closed = 0;
  @override
  Future<Json> request(String op, [Json args = const {}, int? requestId]) {
    expect(op, 'quit');
    expect(quitting, true, reason: 'Polling must stop before sending quit');
    quitRequests++;
    if (reject) return Future.error(StateError('待上传内容需要保留'));
    return acknowledgement.future;
  }

  @override
  Future<void> closeTransport() async {
    closed++;
    throw StateError('StreamSink is closed');
  }
}

class SlowTransportClient extends QuitClient {
  @override
  Future<void> closeTransport() {
    closed++;
    return Completer<void>().future;
  }
}

void main() {
  testWidgets(
    'confirmed quit does not wait indefinitely on transport cleanup',
    (tester) async {
      final client = SlowTransportClient()..available = true;
      bool? done;
      client.shutdown().then((v) => done = v);
      client.acknowledgement.complete({});
      await tester.pump();
      expect(client.quitting, true);
      expect(client.activity, contains('正在退出'));
      expect(client.closed, 1);
      expect(done, isNull);
      await tester.pump(const Duration(seconds: 3));
      expect(done, true);
      expect(client.error, isNull);
      client.dispose();
    },
  );
  test(
    'acknowledged quit tolerates closed pipe and coalesces repeated clicks',
    () async {
      final client = QuitClient()..available = true;
      final first = client.shutdown();
      final second = client.shutdown();
      expect(identical(first, second), true);
      await client.pollNow();
      expect(client.quitRequests, 1);
      client.acknowledgement.complete({});
      expect(await first, true);
      expect(client.closed, 1);
      expect(client.available, false);
      expect(client.error, isNull);
      client.dispose();
    },
  );
  test(
    'backend refusal restores normal operation and permits a later retry',
    () async {
      final client = QuitClient()
        ..available = true
        ..reject = true;
      expect(await client.shutdown(), false);
      await Future<void>.value();
      expect(client.quitting, false);
      expect(client.available, true);
      expect(client.error, contains('待上传内容'));
      expect(client.closed, 0);
      client.reject = false;
      final retry = client.shutdown();
      client.acknowledgement.complete({});
      expect(await retry, true);
      expect(client.quitRequests, 2);
      client.dispose();
    },
  );
}
