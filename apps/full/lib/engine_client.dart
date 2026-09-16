import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/foundation.dart';

typedef Transport = Future<Map<String, dynamic>> Function(Map<String, dynamic>);
class EngineClient extends ChangeNotifier {
  EngineClient(this.executable, this.root, {this.transport});
  final String executable;
  final String root;
  // Injection is used by tests only. Production always uses the IPC bridge.
  final Transport? transport;
  Map<String, dynamic>? snapshot;
  String? error;
  bool busy = false;
  bool pendingMutation = false;
  int _sequence = 0;
  bool _disposed = false;
  Timer? timer;
  static const disconnected = '后台连接中断 · 无法确认同步状态';

  void start() {
    send({'type': 'snapshot'});
    timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (!busy) send({'type': 'snapshot'});
    });
  }
  bool can(String capability) =>
      snapshot != null && !busy &&
      (snapshot!['capabilities'] as List).contains(capability);

  Future<Map<String, dynamic>> _invoke(Map<String, dynamic> request) async {
    Process? process;
    try {
      process = await Process.start(executable, ['ipc', '--state', root]);
      final out = process.stdout.transform(utf8.decoder).join();
      final err = process.stderr.transform(utf8.decoder).join();
      process.stdin.write(jsonEncode(request));
      await process.stdin.close();
      final code = await process.exitCode.timeout(const Duration(seconds: 6));
      final output = await out;
      await err;
      if (code != 0) throw const FormatException('backend disconnected');
      return jsonDecode(output) as Map<String, dynamic>;
    } finally {
      process?.kill();
    }
  }

  Future<void> send(Map<String, dynamic> command) async {
    if (busy || _disposed) return;
    busy = true;
    pendingMutation = command['type'] != 'snapshot';
    if (pendingMutation) notifyListeners();
    try {
      final id = 'ui-$pid-${DateTime.now().microsecondsSinceEpoch}-${_sequence++}';
      final request = {'version': 1, 'id': id, 'command': command};
      final reply = await (transport ?? _invoke)(request);
      if (reply['version'] != 1 || reply['id'] != id) {
        throw const FormatException('incompatible protocol');
      }
      snapshot = reply['snapshot'] as Map<String, dynamic>;
      if (reply['ok'] != true) {
        error = reply['error'] as String? ?? '后台拒绝操作';
      } else if (pendingMutation || error == disconnected) {
        error = null;
      }
    } catch (_) {
      snapshot = null;
      error = disconnected;
    } finally {
      busy = false;
      pendingMutation = false;
      if (!_disposed) notifyListeners();
    }
  }
  @override
  void dispose() {
    _disposed = true;
    timer?.cancel();
    super.dispose();
  }
}
