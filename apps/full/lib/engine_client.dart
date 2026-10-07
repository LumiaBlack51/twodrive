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
  final Map<String, int> downloadSpeed = {};
  final Map<String, int> _downloadBytes = {};
  DateTime? _downloadSample;
  void _sampleDownloads() {
    final now = DateTime.now();
    final elapsed = _downloadSample == null
        ? 0
        : now.difference(_downloadSample!).inMilliseconds;
    final tasks = snapshot?['cloud']?['tasks'] as List? ?? [];
    downloadSpeed.clear();
    for (final task in tasks) {
      final id = task['item']['id'] as String;
      final done = task['done'] as int;
      final previous = _downloadBytes[id];
      downloadSpeed[id] =
          elapsed > 0 &&
              previous != null &&
              done >= previous &&
              task['state'] == 'downloading'
          ? ((done - previous) * 1000 / elapsed).round()
          : 0;
      _downloadBytes[id] = done;
    }
    _downloadSample = now;
  }

  bool busy = false;
  bool pendingMutation = false;
  int _sequence = 0;
  bool _disposed = false;
  Timer? timer;
  String? queryId;
  Map<String, dynamic>? directory;
  final List<Map<String, dynamic>> cloudItems = [];
  final List<Map<String, dynamic>> breadcrumbs = [];
  int _pageNumber = 0;
  Map<String, dynamic>? _queuedBrowse;

  void browse({String? driveId, String? itemId, String? name, int? ancestor}) {
    queryId =
        'browse-$pid-${DateTime.now().microsecondsSinceEpoch}-${_sequence++}';
    directory = {'status': 'loading'};
    cloudItems.clear();
    _pageNumber = 0;
    if (ancestor != null) {
      breadcrumbs.removeRange(ancestor + 1, breadcrumbs.length);
    } else if (itemId == null) {
      breadcrumbs.clear();
    } else if (name != null) {
      breadcrumbs.add({'id': itemId, 'drive_id': driveId, 'name': name});
    }
    _queuedBrowse = {
      'type': 'browse',
      'query_id': queryId,
      'drive_id': driveId,
      'item_id': itemId,
    };
    notifyListeners();
    if (!busy) {
      final command = _queuedBrowse!;
      _queuedBrowse = null;
      send(command);
    }
  }

  void refreshDirectory() {
    final current = breadcrumbs.isEmpty ? null : breadcrumbs.last;
    browse(
      driveId: current?['drive_id'],
      itemId: current?['id'],
      ancestor: breadcrumbs.isEmpty ? null : breadcrumbs.length - 1,
    );
  }

  Future<void> cancelBrowse() async {
    final old = queryId;
    _clearDirectory();
    directory = {'status': 'cancelled'};
    notifyListeners();
    if (old != null) {
      final command = {'type': 'cancel_browse', 'query_id': old};
      if (busy) {
        _queuedBrowse = command;
      } else {
        await send(command);
      }
    }
  }

  Future<void> logout() async {
    _clearDirectory();
    notifyListeners();
    if (busy) {
      _queuedBrowse = {'type': 'logout'};
    } else {
      await send({'type': 'logout'});
    }
  }

  void _clearDirectory() {
    queryId = null;
    directory = null;
    _queuedBrowse = null;
    cloudItems.clear();
    breadcrumbs.clear();
    _pageNumber = 0;
  }

  void _acceptDirectory() {
    if (snapshot?['auth_status'] != 'signed_in') {
      _clearDirectory();
      return;
    }
    final incoming = snapshot?['directory'] as Map<String, dynamic>?;
    if (queryId != null &&
        !pendingMutation &&
        _queuedBrowse == null &&
        (incoming == null || incoming['query_id'] != queryId)) {
      directory = {...?directory, 'status': 'stale', 'error': 'query_replaced'};
      return;
    }
    if (queryId == null ||
        incoming == null ||
        incoming['query_id'] != queryId) {
      return;
    }
    directory = incoming;
    final page = incoming['page'] as Map<String, dynamic>?;
    final number = incoming['page_number'] as int? ?? 0;
    if (page != null && number > _pageNumber) {
      for (final raw in page['items'] as List) {
        final item = Map<String, dynamic>.from(raw as Map);
        final index = cloudItems.indexWhere((old) => old['id'] == item['id']);
        if (index < 0) {
          cloudItems.add(item);
        } else {
          cloudItems[index] = item;
        }
      }
      _pageNumber = number;
    }
  }

  static const disconnected = '后台连接中断 · 无法确认同步状态';

  void start() {
    send({'type': 'snapshot'});
    timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (!busy) send({'type': 'snapshot'});
    });
  }

  bool can(String capability) =>
      snapshot != null &&
      !busy &&
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
      final id =
          'ui-$pid-${DateTime.now().microsecondsSinceEpoch}-${_sequence++}';
      final request = {'version': 1, 'id': id, 'command': command};
      final reply = await (transport ?? _invoke)(request);
      if (reply['version'] != 1 || reply['id'] != id) {
        throw const FormatException('incompatible protocol');
      }
      snapshot = reply['snapshot'] as Map<String, dynamic>;
      _sampleDownloads();
      _acceptDirectory();
      if (reply['ok'] != true) {
        error = reply['error'] as String? ?? '后台拒绝操作';
      } else if (pendingMutation || error == disconnected) {
        error = null;
      }
    } catch (_) {
      snapshot = null;
      _clearDirectory();
      error = disconnected;
    } finally {
      busy = false;
      pendingMutation = false;
      if (!_disposed) notifyListeners();
      if (!_disposed && _queuedBrowse != null) {
        final queued = _queuedBrowse!;
        _queuedBrowse = null;
        send(queued);
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    timer?.cancel();
    super.dispose();
  }
}
