import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/foundation.dart';
import 'updates.dart';
import 'desktop_preferences.dart';
import 'file_manager.dart';

typedef Json = Map<String, dynamic>;

class Transfer {
  Transfer(this.id, this.operation, this.args);
  final int id;
  final String operation;
  final Json args;
  int done = 0;
  int total = 0;
  String state = '传输中';
  String? error;
  String get name => (args['path'] as String).split('/').last;
}

class StorageClient extends ChangeNotifier {
  StorageClient() {
    updates.addListener(notifyListeners);
  }
  final updates = UpdateChecker();
  final preferences = DesktopPreferences();
  bool get autoMountOnConnect => preferences.autoMountOnConnect;

  Future<void> setAutoMountOnConnect(bool enabled) async {
    await preferences.setAutoMountOnConnect(enabled);
    notifyListeners();
    if (enabled && connected && canMount && !mounted && !busy) {
      await mountToggle();
    }
  }

  Process? _process;
  final Map<int, Completer<Json>> _pending = {};
  final List<Transfer> transfers = [];
  List<Json> profiles = [];
  List<Json> providers = [];
  List<Json> entries = [];
  final Map<String, Json> quotas = {};
  final Map<String, DateTime> _quotaChecked = {};
  final Map<String, dynamic> _quotaVersions = {};
  final Map<String, Future<void>> _quotaRequests = {};
  final Map<String, Json> _views = {};
  Json status = {};
  String? selected;
  String path = '/';
  String? _error;
  String? _pollError;
  String? get error => _error ?? _pollError;
  set error(String? value) {
    _error = value;
    if (value == null) _pollError = null;
  }

  String activity = '';
  bool _busy = false;
  final Set<int> _engineOperations = {};
  bool get busy => _busy || _engineOperations.isNotEmpty;
  set busy(bool value) => _busy = value;
  Future<void>? _statusRefresh;
  Future<void>? _polling;
  Future<bool>? _shutdown;
  Future<void>? _startupRestore;
  bool _restoringStartup = false;
  bool _disposed = false;
  DateTime? _directoryChecked;
  bool _directoryStale = true;
  bool available = false;
  bool quitting = false;
  int _id = 0;
  int _browseGeneration = 0;
  int _selectionEpoch = 0;
  String? _selectingProfile;
  int? _directoryRequest;
  int? activeId;
  Timer? _poll;
  bool get connected => status['connected'] == true;
  bool isProfileConnected(String id) => status['multi_connection'] == true
      ? ((status['connections'] as List?) ?? []).any((p) => p['id'] == id)
      : connected && status['profile'] == id;
  List<Json> get connectedProfiles =>
      profiles.where((p) => isProfileConnected(p['id'] as String)).toList();
  Iterable<Transfer> get runningTransfers =>
      transfers.where((t) => t.state == '传输中');
  Json get sharing => (status['sharing'] as Json?) ?? {};
  bool get hosting => sharing['running'] == true;
  bool get syncError =>
      status['overall']?['error'] == true ||
      status['backup_failed'] == true ||
      status['backup']?['state'] == 'error' ||
      (status['cache']?['deletes_rejected'] as num? ?? 0) > 0;
  bool get paused => status['paused'] == true;
  List<Json> get syncActivity => [
    if (status['overall'] is Map)
      ...((status['overall']?['active'] as List?) ?? [])
    else ...[
      ...((status['backup']?['active'] as List?) ?? []),
      ...((status['mount_activity']?['active'] as List?) ?? []),
    ],
  ].map((v) => Map<String, dynamic>.from(v as Map)).toList();
  bool get syncing =>
      status['overall']?['syncing'] == true ||
      busy ||
      runningTransfers.isNotEmpty ||
      syncActivity.isNotEmpty ||
      (status['backup']?['state'] == 'scanning' ||
          status['backup']?['state'] == 'starting') ||
      (status['cache']?['pending'] as num? ?? 0) > 0;
  Future<void> pauseToggle() => action(paused ? '恢复同步' : '暂停同步', () async {
    status = await request('pause', {'paused': !paused});
  });
  Future<void> signOut() => action('退出登录', () async {
    await request('sign_out', {'id': selected});
    entries = [];
    status = {};
    await loadProfiles();
    await refreshStatus();
  });
  bool get canMount => status['capabilities']?['mount'] == true;
  bool get mountSupported => status['mount_support']?['supported'] == true;
  bool get readOnlyMount => status['mount_support']?['readonly'] == true;
  bool get mountDriverMissing =>
      status['mount_support']?['driver_installed'] == false;
  Json get nativeItemCapabilities => Map<String, dynamic>.from(
    status['local_files']?['item_status'] as Map? ?? {},
  );
  bool get canInspectNativeItems =>
      nativeItemCapabilities['schema_version'] == 1 &&
      nativeItemCapabilities['supported'] == true &&
      nativeItemCapabilities['scope'] == 'generated_fixture_only';
  Future<Json> nativeItems(String op, [String? id, bool? offline]) async {
    if (!canInspectNativeItems ||
        !const [
          'local_items',
          'local_read',
          'local_dehydrate',
          'local_pin',
          'local_unpin',
          'local_offline',
          'local_reconnect',
        ].contains(op) ||
        (const ['local_pin', 'local_unpin'].contains(op) &&
            nativeItemCapabilities['pin'] != true) ||
        (op == 'local_offline' &&
            (nativeItemCapabilities['offline_test'] != true ||
                offline == null)) ||
        (op == 'local_reconnect' &&
            nativeItemCapabilities['reconnect'] != true) ||
        (op == 'local_read' && nativeItemCapabilities['read'] != true) ||
        (op == 'local_dehydrate' &&
            nativeItemCapabilities['native_copy_dehydrate'] != true)) {
      throw StateError('本机文件能力不可用');
    }
    final result = await request(op, {
      'schema_version': 1,
      'id': ?id,
      'offline': ?offline,
    });
    if (result['schema_version'] != 1 ||
        result['scope'] != 'generated_fixture_only' ||
        result['items'] is! List) {
      throw StateError('本机文件状态版本不兼容');
    }
    return result;
  }

  bool get canPreflightLocalFile =>
      status['local_files']?['schema_version'] == 1 &&
      status['local_files']?['metadata_preflight'] == true;
  Future<Json> preflightLocalFile(String source, String kind) async {
    if (!canPreflightLocalFile) {
      throw StateError('当前引擎不支持本地文件预检');
    }
    final result = await request('local_preflight', {
      'schema_version': 1,
      'source_path': source,
      'source_kind': kind,
    });
    if (result['schema_version'] != 1 ||
        result['approval_required'] != true ||
        result['content_read'] != false ||
        result['hydration_requested'] != false ||
        result['import_performed'] != false) {
      throw StateError('本地预检响应不兼容，请更新引擎与界面');
    }
    return result;
  }

  List<Json> get mounts {
    final rows = status['mounts'];
    if (rows is List) {
      return rows
          .whereType<Map>()
          .map((v) => Map<String, dynamic>.from(v))
          .where(
            (v) => profiles.any(
              (p) => p['id'] == v['profile'] && p['revision'] == v['revision'],
            ),
          )
          .toList();
    }
    final p = profiles.where((p) => p['id'] == status['profile']).firstOrNull;
    return status['mounted'] == true && p != null
        ? [
            {
              'profile': p['id'],
              'revision': p['revision'],
              'name': p['name'],
              'path': p['mount_path'],
              'failed': status['mount_failed'] == true,
            },
          ]
        : [];
  }

  Json? mountForProfile(String id) =>
      mounts.where((m) => m['profile'] == id).firstOrNull;
  bool isProfileMounted(String id) => mountForProfile(id) != null;
  bool canMountProfile(String id) =>
      mountSupported && !mountDriverMissing && isProfileConnected(id);
  bool get mounted => status['mounted'] == true;
  Json? get profile => profiles.where((p) => p['id'] == selected).firstOrNull;
  bool capability(String key) =>
      connected && status['capabilities']?[key] == true;
  bool get canUpload =>
      capability('upload') &&
      (capability('conditional_upload') || capability('manual_upload'));
  bool get canMkdir => capability('mkdir');
  bool get canRename => capability('rename');
  bool get canDelete => capability('delete');
  bool get canBackup => connected
      ? capability('backup')
      : profile?['capabilities']?['backup'] == true;
  bool get writable => canUpload || canMkdir || canRename || canDelete;
  String get providerLabel =>
      profile?['provider_label'] as String? ?? 'TwoDrive';
  String get accountLabel => profile?['account_label'] as String? ?? '账号';
  String get account => connected
      ? (status['account_name'] as String? ??
            profile?['account_name'] as String? ??
            '账号名称暂不可用')
      : '未登录';
  String get trayTitle {
    final connection = connected ? ' · $providerLabel · $account' : '';
    final host = hosting ? ' · 正在共享 ${sharing['settings']?['name'] ?? ''}' : '';
    return 'TwoDrive$connection$host${!connected && !hosting ? ' · 未连接' : ''}';
  }

  String get trayState => error != null || syncError
      ? 'error'
      : connected &&
            (proxyFallback || status['overall']?['route_fallback'] == true)
      ? 'warning'
      : paused
      ? 'paused'
      : syncing
      ? 'syncing'
      : connected || hosting
      ? 'connected'
      : 'offline';
  Json get target => {
    'profile': selected,
    'revision': profile?['revision'] ?? '',
  };
  bool get proxyFallback => connected && status['route_fallback'] == true;
  void requireProfile(Json args) {
    final id = args['profile'] as String? ?? selected;
    final owner = profiles.where((p) => p['id'] == id).firstOrNull;
    if (id == null || !isProfileConnected(id)) {
      throw StateError('文件操作所属连接已改变，请刷新后重试');
    }
    if (args['revision'] != null && args['revision'] != owner?['revision']) {
      throw StateError('连接配置已更新，请刷新后重新选择文件');
    }
  }

  Future<void> start(
    String engine,
    String state,
    String peer,
    String launcher, {
    String? nativeFixtureRoot,
  }) async {
    try {
      await preferences.load(File('$state/desktop-settings.json'));
      final p = await Process.start(engine, [
        '--state',
        state,
        '--peer-bin',
        peer,
        '--launcher',
        launcher,
        if (nativeFixtureRoot != null) ...[
          '--native-local-fixture-root',
          nativeFixtureRoot,
        ],
      ]);
      _process = p;
      p.stdout.transform(utf8.decoder).transform(const LineSplitter()).listen((
        line,
      ) {
        try {
          accept(jsonDecode(line) as Json);
        } catch (_) {
          fail('引擎返回无效数据');
        }
      }, onError: (_) => fail('引擎通信中断'));
      // Keep stderr drained without exposing private configuration in the UI.
      p.stderr.drain<void>();
      unawaited(
        p.exitCode.then((code) {
          if (!quitting) {
            fail('本地引擎已退出（$code）。请重新启动客户端。');
          }
        }),
      );
      available = true;
      // Older engines keep working through single-flight polling. Operation
      // notifications are opt-in so older frontends never misread them as replies.
      try {
        await request('subscribe', {'operations': true});
      } on StateError {
        // No subscription operation in earlier engine versions.
      }
      await loadProfiles();
      await refreshStatus();
      final version = status['version'];
      if (version is String) {
        unawaited(
          updates.start(
            version,
            File('$state/update-settings.json'),
            managed: File(
              '${File(engine).parent.path}/distribution-msix',
            ).existsSync(),
          ),
        );
      }
      _poll = Timer.periodic(const Duration(seconds: 3), (_) {
        unawaited(pollNow());
      });
    } catch (e) {
      fail('无法启动本地引擎：$e');
    }
  }

  @visibleForTesting
  void accept(Json message) {
    if (_disposed) return;
    final id = message['id'] as int;
    if (id > _id) _id = id;
    if (message['event'] == 'operation') {
      if (message['op'] == 'upload' || message['op'] == 'download') {
        if (message['running'] != true &&
            (message['profile'] == null || message['profile'] == selected)) {
          _directoryStale = true;
        }
        notifyListeners();
        return;
      }
      if (message['running'] == true) {
        _engineOperations.add(id);
        if (!_busy) {
          activeId = id;
          activity = '正在处理连接或文件操作';
        }
      } else {
        _engineOperations.remove(id);
        if (!_busy && activeId == id) {
          activeId = null;
          activity = '';
        }
        if (message['configuration'] is Map) {
          _applyProfiles(
            Map<String, dynamic>.from(message['configuration'] as Map),
          );
        }
        if (message['status'] is Map) {
          _applyStatus(Map<String, dynamic>.from(message['status'] as Map));
        }
        _directoryStale = true;
        _browseGeneration++;
        if (!quitting && message['op'] != 'quit') unawaited(pollNow());
      }
      notifyListeners();
      return;
    }
    if (message['event'] == 'progress') {
      final task = transfers.where((t) => t.id == id).firstOrNull;
      if (task != null) {
        task.done = message['done'] as int;
        task.total = message['total'] as int;
        notifyListeners();
      }
      return;
    }
    if (message.containsKey('event')) return;
    final reply = _pending.remove(id);
    if (reply == null) {
      return;
    }
    if (message['ok'] == true) {
      reply.complete(Map<String, dynamic>.from(message['result'] as Map));
    } else {
      reply.completeError(StateError(message['error'] as String));
    }
  }

  @visibleForTesting
  void fail(String message) {
    available = false;
    _engineOperations.clear();
    status = {};
    entries = [];
    _views.clear();
    _selectingProfile = null;
    _selectionEpoch++;
    _browseGeneration++;
    error = message;
    for (final pending in _pending.values) {
      pending.completeError(StateError(message));
    }
    _pending.clear();
    notifyListeners();
  }

  Future<Json> request(String op, [Json args = const {}, int? requestId]) {
    if (!available || _process == null) {
      return Future.error(StateError('本地引擎未运行'));
    }
    if (_pending.length >= 32 && op != 'cancel') {
      return Future.error(StateError('请求正在处理中，请稍后重试'));
    }
    final id = requestId ?? ++_id;
    if (_pending.containsKey(id)) {
      return Future.error(StateError('请求编号重复'));
    }
    final completer = Completer<Json>();
    _pending[id] = completer;
    try {
      _process!.stdin.writeln(jsonEncode({'id': id, 'op': op, 'args': args}));
    } catch (e) {
      _pending.remove(id);
      completer.completeError(e);
    }
    return completer.future;
  }

  int reserveRequestId() => ++_id;

  void _applyProfiles(Json result) {
    profiles = (result['profiles'] as List).cast<Json>();
    providers = ((result['providers'] as List?) ?? []).cast<Json>();
    _views.removeWhere(
      (id, view) => !profiles.any(
        (p) => p['id'] == id && p['revision'] == view['revision'],
      ),
    );
    for (final id in quotas.keys.toList()) {
      if (!profiles.any(
        (p) => p['id'] == id && p['revision'] == _quotaVersions[id],
      )) {
        quotas.remove(id);
        _quotaChecked.remove(id);
        _quotaVersions.remove(id);
      }
    }
    selected = connected
        ? status['profile'] as String?
        : result['selected'] as String?;
  }

  Future<void> loadProfiles() async {
    _applyProfiles(await request('profiles'));
    notifyListeners();
  }

  void _applyStatus(Json next) {
    final previous = status['profile'];
    final wasConnected = connected;
    if (wasConnected && previous is String && previous != next['profile']) {
      _views[previous] = {
        'path': path,
        'entries': entries.map((e) => Map<String, dynamic>.from(e)).toList(),
        'revision': profiles
            .where((p) => p['id'] == previous)
            .firstOrNull?['revision'],
        'checked': _directoryChecked,
        'stale': _directoryStale,
      };
    }
    status = next;
    _views.removeWhere(
      (id, view) =>
          !isProfileConnected(id) ||
          !profiles.any(
            (p) => p['id'] == id && p['revision'] == view['revision'],
          ),
    );
    if (connected) selected = status['profile'] as String? ?? selected;
    if (!connected ||
        previous != status['profile'] ||
        wasConnected != connected) {
      final view = connected ? _views[status['profile']] : null;
      entries = ((view?['entries'] as List?) ?? []).cast<Json>();
      path = view?['path'] as String? ?? '/';
      _directoryChecked = view?['checked'] as DateTime?;
      _browseGeneration++;
      _directoryStale = view?['stale'] as bool? ?? true;
    }
    _pollError = status['gateway_failed'] == true
        ? '配对连接已断开，请重连。'
        : status['mount_failed'] == true
        ? '挂载进程已退出；请卸载后重试。缓存已保留。'
        : null;
    if (status['storage_usage'] == true && available && !quitting) {
      for (final profile in connectedProfiles) {
        unawaited(refreshQuota(profile['id'] as String));
      }
    }
  }

  Future<void> refreshQuota(String id, {bool force = false}) {
    if (!profiles.any((p) => p['id'] == id)) return Future.value();
    if (!available || quitting || !isProfileConnected(id)) {
      return Future.value();
    }
    final checked = _quotaChecked[id];
    if (!force &&
        checked != null &&
        DateTime.now().difference(checked) < const Duration(minutes: 5)) {
      return Future.value();
    }
    return _quotaRequests[id] ??= _readQuota(id);
  }

  Future<void> _readQuota(String id) async {
    final p = profiles.where((p) => p['id'] == id).firstOrNull;
    if (p == null) return;
    final revision = p['revision'];
    _quotaVersions[id] = revision;
    final old = quotas[id];
    quotas[id] = {...?old, 'loading': true};
    try {
      final result = await request('quota', {
        'profile': id,
        'revision': revision,
      });
      if (!_disposed &&
          profiles.any((p) => p['id'] == id && p['revision'] == revision)) {
        quotas[id] = result;
        _quotaChecked[id] = DateTime.now();
      }
    } catch (_) {
      if (!_disposed &&
          profiles.any((p) => p['id'] == id && p['revision'] == revision)) {
        quotas[id] = old?['usage'] is Map
            ? {...old!, 'loading': false, 'stale': true}
            : {'state': 'unavailable'};
        _quotaChecked[id] = DateTime.now();
      }
    } finally {
      _quotaRequests.remove(id);
      if (!_disposed) notifyListeners();
    }
  }

  // Keep the in-flight request until its reply arrives, including time spent
  // behind a long engine operation. A timer tick must never enqueue another.
  Future<void> refreshStatus() => _statusRefresh ??= _readStatus();
  Future<void> _readStatus() async {
    final epoch = _selectionEpoch;
    try {
      final next = await request('status');
      if (!_disposed && epoch == _selectionEpoch) _applyStatus(next);
    } catch (e) {
      if (epoch != _selectionEpoch) return;
      if (!_disposed) _pollError = '$e'.replaceFirst('Bad state: ', '');
      rethrow;
    } finally {
      _statusRefresh = null;
      if (!_disposed) notifyListeners();
    }
  }

  Future<void> pollNow({bool forceDirectory = false}) {
    if (forceDirectory) _directoryStale = true;
    return _polling ??= _pollOnce();
  }

  Future<void> _pollOnce() async {
    await Future<void>.value();
    try {
      if (busy || !available || quitting || _disposed) return;
      final previous = selected;
      final epoch = _selectionEpoch;
      await refreshStatus();
      if (_disposed || quitting || !available || epoch != _selectionEpoch) {
        return;
      }
      if (selected != previous) await loadProfiles();
      if (!busy &&
          connected &&
          available &&
          !quitting &&
          (_directoryStale ||
              _directoryChecked == null ||
              DateTime.now().difference(_directoryChecked!) >=
                  const Duration(seconds: 15))) {
        await readDirectory(path);
      }
    } catch (e) {
      if (!_disposed && !quitting) {
        _pollError = '$e'.replaceFirst('Bad state: ', '');
        notifyListeners();
      }
    } finally {
      _polling = null;
    }
  }

  Future<void> action(String label, Future<void> Function() run) async {
    if (busy) {
      return;
    }
    busy = true;
    activity = label;
    error = null;
    notifyListeners();
    try {
      await run();
    } catch (e) {
      error = e.toString().replaceFirst('Bad state: ', '');
    } finally {
      busy = false;
      activity = '';
      activeId = null;
      notifyListeners();
      unawaited(pollNow());
    }
  }

  // Optional maintenance restart: restore exactly the previously online IDs,
  // serially through this GUI-owned engine, then restore the selected account.
  Future<void> restoreConnectionsAtStartup(Iterable<String> ids) async {
    final original = selected;
    final known = profiles.map((p) => p['id'] as String).toSet();
    final order = ids.where(known.contains).toSet()..remove(original);
    if (original != null && known.contains(original)) order.add(original);
    final failed = <String>[];
    for (final id in order) {
      await connect(id);
      if (error != null) failed.add(id);
    }
    if (failed.isNotEmpty) {
      error = '部分连接未能恢复，请在连接页重试。';
      notifyListeners();
    }
  }

  Future<void> connect([String? profileId]) {
    final id = profileId ?? selected;
    if (id != null && isProfileConnected(id)) {
      if (status['profile'] == id && _selectingProfile == null) {
        return Future.value();
      }
      return _selectConnected(id);
    }
    return action('正在登录并连接', () async {
      if (id == null) throw StateError('请先添加连接');
      if (connected && status['profile'] == id) return;
      _selectionEpoch++;
      _selectingProfile = null;
      _browseGeneration++;
      activeId = ++_id;
      try {
        final result = await request('connect', {'id': id}, activeId);
        await refreshStatus();
        selected = id;
        if (result['entries'] is List) {
          path = '/';
          entries = (result['entries'] as List)
              .map((e) => {...e as Json, ...target})
              .toList();
        }
        _directoryStale = result['entries'] is! List;
        if (!_directoryStale) _directoryChecked = DateTime.now();
        if (Platform.isLinux &&
            autoMountOnConnect &&
            connected &&
            canMount &&
            !mounted) {
          activity = '已连接，正在自动挂载';
          activeId = ++_id;
          notifyListeners();
          await request('mount', {
            'profile': id,
            'revision': profile?['revision'],
          }, activeId);
          await refreshStatus();
        }
        if (result['entries'] is List) {
          _directoryChecked = DateTime.now();
          _directoryStale = false;
        }
      } finally {
        await refreshStatus();
        if (connected) selected = status['profile'] as String;
        notifyListeners();
      }
    });
  }

  Future<void> _selectConnected(String id) async {
    final revision = profiles
        .where((p) => p['id'] == id)
        .firstOrNull?['revision'];
    if (revision == null || !available || quitting || _disposed) return;
    final epoch = ++_selectionEpoch;
    _selectingProfile = id;
    _browseGeneration++; // Invalidate the previous display before cancelling it.
    error = null;
    final oldRead = _directoryRequest;
    if (oldRead != null) {
      try {
        await request('cancel', {'id': oldRead});
      } catch (_) {
        /* selection reports errors */
      }
    }
    try {
      final result = await request('select', {'id': id, 'revision': revision});
      if (_disposed || quitting || epoch != _selectionEpoch) return;
      if (!profiles.any((p) => p['id'] == id && p['revision'] == revision)) {
        throw StateError('连接配置已更新，请刷新后重试');
      }
      _applyStatus(Map<String, dynamic>.from(result['status'] as Map));
    } catch (e) {
      if (!_disposed && epoch == _selectionEpoch) {
        error = '$e'.replaceFirst('Bad state: ', '');
      }
    } finally {
      if (!_disposed && epoch == _selectionEpoch) {
        _selectingProfile = null;
        notifyListeners();
        unawaited(pollNow());
      }
    }
  }

  /// Restore every previously authenticated account once per app process.
  /// The engine refuses browser login here and validates revision and binding.
  Future<void> restoreOnStartup() => _startupRestore ??= action(
    '正在恢复网盘连接和挂载',
    () async {
      _restoringStartup = true;
      try {
        final preferred = selected;
        final candidates = profiles
            .where((p) => p['startup_ready'] == true)
            .map((p) => Map<String, dynamic>.from(p))
            .toList();
        // Restore the previous browsing account last, retaining all others.
        final ordered = [
          ...candidates.where((p) => p['id'] != preferred),
          ...candidates.where((p) => p['id'] == preferred),
        ];
        final failures = <String>[];
        for (final p in ordered) {
          if (quitting || _disposed || !available) break;
          final id = p['id'] as String;
          final owner = {'profile': id, 'revision': p['revision']};
          try {
            activeId = reserveRequestId();
            await request('resume', {
              'id': id,
              'revision': p['revision'],
            }, activeId);
            if (quitting || _disposed) break;
            await refreshStatus();
          } catch (e) {
            if (!quitting) {
              failures.add(
                '${p['name']}：连接恢复失败，${'$e'.replaceFirst('Bad state: ', '')}',
              );
            }
            continue;
          }
          if (quitting || _disposed) break;
          if (mountSupported && !isProfileMounted(id)) {
            try {
              activeId = reserveRequestId();
              await request('mount', {...owner, 'startup': true}, activeId);
              if (!quitting && !_disposed) await refreshStatus();
            } catch (e) {
              if (!quitting) {
                failures.add(
                  '${p['name']}：自动挂载失败，${'$e'.replaceFirst('Bad state: ', '')}',
                );
              }
            }
          }
        }
        if (!quitting && !_disposed) {
          await loadProfiles();
          await refreshStatus();
          if (failures.isNotEmpty) error = failures.join('\n');
        }
      } finally {
        _restoringStartup = false;
      }
    },
  );
  Future<void> openWriteRecovery(String id) => action('正在打开恢复目录', () async {
    final result = await request('write_recovery', {'profile': id});
    final directories = result['directories'] as List;
    for (final directory in directories) {
      await openLocalDirectory(directory as String);
    }
  });
  Future<void> disconnect([String? profileId]) => action('正在断开', () async {
    final id = profileId ?? selected;
    _selectionEpoch++;
    _selectingProfile = null;
    await request('disconnect', {'id': ?id});
    if (id == selected) {
      _browseGeneration++;
      entries = [];
      path = '/';
    }
    await refreshStatus();
  });
  Future<void> browse(String target) =>
      action('读取目录', () => readDirectory(target));
  Future<void> readDirectory(String target) async {
    final generation = ++_browseGeneration;
    final identity = this.target;
    final requestId = ++_id;
    _directoryRequest = requestId;
    try {
      final result = await request('list', {
        'path': target,
        ...identity,
      }, requestId);
      if (generation != _browseGeneration ||
          !available ||
          _disposed ||
          quitting) {
        return;
      }
      _directoryChecked = DateTime.now();
      _directoryStale = false;
      path = target;
      entries = (result['entries'] as List)
          .map((e) => {...e as Json, ...identity})
          .toList();
      notifyListeners();
    } catch (_) {
      if (generation == _browseGeneration &&
          !_disposed &&
          available &&
          !quitting) {
        rethrow;
      }
    } finally {
      if (_directoryRequest == requestId) _directoryRequest = null;
    }
  }

  Future<void> mutate(String op, Json args) => action('正在更新文件', () async {
    requireProfile(args);
    await request(op, {...target, ...args});
    await readDirectory(path);
  });
  Future<void> transfer(String op, Json args) async {
    if (busy) return;
    try {
      requireProfile(args);
    } catch (e) {
      error = '$e'.replaceFirst('Bad state: ', '');
      notifyListeners();
      return;
    }
    args = {...target, ...args};
    final id = ++_id;
    final task = Transfer(id, op, args);
    transfers.insert(0, task);
    if (transfers.length > 100) {
      final old = transfers.last;
      if (old.state != '传输中') transfers.removeLast();
    }
    notifyListeners();
    try {
      await request(op, args, id);
      task.state = '已完成';
      task.done = task.total;
      if (op == 'upload' && selected == args['profile'] && connected) {
        await readDirectory(path);
      }
    } catch (e) {
      task.state = '未完成';
      task.error = '$e'.replaceFirst('Bad state: ', '');
      if (selected == args['profile']) error = task.error;
    } finally {
      if (!_disposed) notifyListeners();
    }
  }

  Future<void> cancelTransfer(int id) =>
      request('cancel', {'id': id}).then((_) {});
  Future<void> cancel() async {
    final id = activeId ?? runningTransfers.firstOrNull?.id;
    if (id != null) {
      activity = '正在取消，等待当前网络读写返回';
      notifyListeners();
      await request('cancel', {'id': id});
    }
  }

  Future<void> mountToggle([String? profileId]) {
    final id = profileId ?? selected;
    final p = profiles.where((p) => p['id'] == id).firstOrNull;
    if (p == null) return Future.value();
    final op = isProfileMounted(id!) ? 'unmount' : 'mount';
    final owner = {'profile': id, 'revision': p['revision']};
    return action(op == 'unmount' ? '正在卸载' : '正在挂载', () async {
      activeId = ++_id;
      await request(op, owner, activeId);
      await refreshStatus();
    });
  }

  Future<void> openMountFolder(
    String id, {
    Future<void> Function(String)? openDirectory,
  }) => action('打开挂载', () async {
    final mount = mountForProfile(id);
    if (mount == null || mount['failed'] == true || mount['path'] is! String) {
      throw StateError('请先挂载此网盘；挂载异常时请卸载后重试');
    }
    await (openDirectory ?? openLocalDirectory)(mount['path'] as String);
  });

  Future<String?> save(Json profile) async {
    String? id;
    await action('保存连接', () async {
      final result = await request('save_profile', profile);
      id = result['id'] as String?;
      await loadProfiles();
    });
    return id;
  }

  Future<Json?> shareAction(String op, Json args) async {
    Json? result;
    await action(
      op == 'share_start'
          ? '正在启动共享'
          : op == 'share_export'
          ? '正在导出连接文件'
          : '正在更新共享',
      () async {
        activeId = ++_id;
        result = await request(op, args, activeId);
        await refreshStatus();
      },
    );
    return result;
  }

  Future<void> remove(String id) => action('删除连接配置', () async {
    await request('remove_profile', {'id': id});
    await loadProfiles();
  });
  Future<void> autostart(bool enabled) => action('更新自启动设置', () async {
    status = await request('autostart', {'enabled': enabled});
  });
  Future<void> retryDeletes() => action('安排云端删除重试', () async {
    await request('retry_deletes', {'id': status['profile'] ?? selected});
    await refreshStatus();
  });
  Future<void> clearCache() => action('释放已同步缓存', () async {
    await request('clear_cache', {'id': selected});
    await refreshStatus();
  });
  @protected
  Future<void> closeTransport() async => _process?.stdin.close();

  Future<bool> shutdown() {
    final existing = _shutdown;
    if (existing != null) return existing;
    final attempt = _shutdownOnce();
    _shutdown = attempt;
    unawaited(
      attempt.then((done) {
        if (!done) _shutdown = null;
      }),
    );
    return attempt;
  }

  Future<bool> _shutdownOnce() async {
    if ((busy && !_restoringStartup) || runningTransfers.isNotEmpty) {
      error = '请先完成或取消当前操作，再退出。';
      notifyListeners();
      return false;
    }
    quitting = true;
    _poll?.cancel();
    activity = '正在退出，正在收尾连接…';
    notifyListeners();
    try {
      if (available) {
        if (_restoringStartup && activeId != null) {
          await request('cancel', {'id': activeId});
        }
        await request('quit');
      }
      updates.dispose();
      available = false;
      // The engine may close its pipe immediately after acknowledging quit.
      // That transport cleanup failure does not revoke the confirmed shutdown.
      try {
        await closeTransport().timeout(const Duration(seconds: 2));
      } catch (_) {}
      return true;
    } catch (e) {
      quitting = false;
      activity = '';
      if (available && !_disposed) {
        _poll = Timer.periodic(
          const Duration(seconds: 3),
          (_) => unawaited(pollNow()),
        );
      }
      error = '$e';
      notifyListeners();
      return false;
    }
  }

  @override
  void dispose() {
    _disposed = true;
    if (!quitting) updates.dispose();
    _poll?.cancel();
    super.dispose();
  }
}
