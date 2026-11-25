import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'package:flutter/foundation.dart';
import 'updates.dart';
import 'desktop_preferences.dart';
import 'language.dart';
import 'file_manager.dart';
import 'mounted_directory.dart';
part 'client_transfer_history.dart';

typedef Json = Map<String, dynamic>;

class AuthorizationExpired extends StateError {
  AuthorizationExpired(super.message);
}

enum TransferPhase { running, completed, failed, unconfirmed }

class Transfer {
  Transfer(this.id, this.operation, this.args, {this.account});
  final int id;
  final String operation;
  final Json args;
  final String? account;
  int done = 0;
  int total = 0;
  TransferPhase phase = TransferPhase.running;
  String get state => tr(stateKey);
  String get stateKey => switch (phase) {
    TransferPhase.running => '传输中',
    TransferPhase.completed => '已完成',
    TransferPhase.failed => '未完成',
    TransferPhase.unconfirmed => '结果未确认',
  };
  String? error;
  String get name => (args['path'] as String).split('/').last;
  bool get upload => operation == 'upload';
  bool get view => operation == 'view';
}

typedef TransferRow = ({Json info, Transfer? manual});
typedef _DirectoryOwner = ({String? profile, Object revision});
typedef _DirectoryKey = ({_DirectoryOwner owner, String path});
typedef _DirectorySnapshot = ({List<Json> entries, DateTime checked});

class StorageClient extends ChangeNotifier {
  StorageClient() {
    updates.addListener(notifyListeners);
  }
  final updates = UpdateChecker();
  final preferences = DesktopPreferences();
  bool get autoMountOnConnect => preferences.autoMountOnConnect;
  bool get restoreConnectionsOnStartup =>
      preferences.restoreConnectionsOnStartup;

  Future<void> setRestoreConnectionsOnStartup(bool enabled) async {
    error = null;
    try {
      await preferences.setRestoreConnectionsOnStartup(enabled);
    } catch (e) {
      error = tr('无法保存桌面设置：{arg0}', [e]);
    }
    notifyListeners();
  }

  Future<void> setLanguage(String value) async {
    try {
      await preferences.setLanguage(value);
      desktopLocale.value = value;
    } catch (e) {
      error = tr('无法保存桌面设置：{arg0}', [e]);
    }
    notifyListeners();
  }

  Future<void> setAutoMountOnConnect(bool enabled) async {
    error = null;
    try {
      await preferences.setAutoMountOnConnect(enabled);
    } catch (e) {
      error = tr('无法保存桌面设置：{arg0}', [e]);
      notifyListeners();
      return;
    }
    notifyListeners();
    if (enabled && connected && canMount && !mounted && !busy) {
      await mountToggle();
    }
  }

  Process? _process;
  final Map<int, Completer<Json>> _pending = {};
  final List<Transfer> transfers = [];
  final _TransferHistory _transferHistory = _TransferHistory();
  Directory? _stateDirectory;
  String? transferHistoryError;
  List<Json> profiles = [];
  List<Json> providers = [];
  List<Json> entries = [];
  final Map<String, Json> quotas = {};
  final Map<String, DateTime> _quotaChecked = {};
  final Map<String, dynamic> _quotaVersions = {};
  final Map<String, Future<void>> _quotaRequests = {};
  final Map<String, ({_DirectoryOwner owner, String path, bool stale})> _views =
      {};
  final Map<_DirectoryKey, _DirectorySnapshot> _directories = {};
  Json status = {};
  String? selected;
  String path = '/';
  final Map<String, String> connectionErrors = {};
  final Set<String> needsAuthorization = {};

  void _recordConnectionError(String id, Object error) {
    connectionErrors[id] = error.toString().replaceFirst('Bad state: ', '');
    if (error is AuthorizationExpired) {
      needsAuthorization.add(id);
    } else {
      needsAuthorization.remove(id);
    }
  }

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
  bool _directoryFromMount = false;
  bool get directoryFromMount => _directoryFromMount;
  String? _directoryError;
  String? get directoryError => _directoryError;
  bool get directoryLoaded => _directoryChecked != null || entries.isNotEmpty;
  bool get directoryLoading => _directoryRead?.generation == _browseGeneration;
  bool get directoryReady =>
      directoryLoaded &&
      !directoryFromMount &&
      !directoryLoading &&
      _directoryError == null &&
      (_directoryChecked == null || !_directoryStale);
  bool get needsDirectoryRead =>
      connected &&
      available &&
      !busy &&
      !quitting &&
      !_disposed &&
      _selectingProfile == null &&
      !directoryLoading &&
      _directoryError == null &&
      _directoryChecked == null &&
      (entries.isEmpty || directoryFromMount);
  bool available = false;
  bool quitting = false;
  int _id = 0;
  int _browseGeneration = 0;
  int _selectionEpoch = 0;
  String? _selectingProfile;
  ({int id, int generation, Future<void> future})? _directoryRead;
  int? activeId;
  bool _showFiles = false;
  int _filesViewEpoch = 0;
  bool _showQuota = false;
  bool _viewVisible = false;
  bool get viewVisible => _viewVisible;
  int _visibilityEpoch = 0;
  int get visibilityEpoch => _visibilityEpoch;
  Timer? _transferRefresh;
  int _statusVersion = 0;

  void setView({
    required bool visible,
    bool files = false,
    bool quota = false,
    bool transfers = false,
  }) {
    if (_viewVisible != visible) _visibilityEpoch++;
    _viewVisible = visible;
    final showFiles = visible && files;
    if (_showFiles != showFiles) _filesViewEpoch++;
    _showFiles = showFiles;
    _showQuota = visible && quota;
    if (visible && transfers) {
      _transferRefresh ??= Timer.periodic(
        const Duration(seconds: 3),
        (_) => unawaited(refreshView()),
      );
    } else {
      _transferRefresh?.cancel();
      _transferRefresh = null;
    }
  }

  Future<void> refreshView({
    bool forceDirectory = false,
    bool forceQuota = false,
  }) async {
    // The next status result selects eligible accounts and coalesces pending quota reads.
    if (forceQuota && _showQuota) _quotaChecked.clear();
    if (_showFiles) {
      await pollNow(forceDirectory: forceDirectory);
      return;
    }
    if (busy || !available || quitting || _disposed) return;
    try {
      await refreshStatus();
    } catch (_) {
      // refreshStatus records the display error and notifies the view.
    }
  }

  bool get connected => status['connected'] == true;
  bool isProfileConnected(String id) => status['multi_connection'] == true
      ? ((status['connections'] as List?) ?? []).any((p) => p['id'] == id)
      : connected && status['profile'] == id;
  List<Json> get connectedProfiles =>
      profiles.where((p) => isProfileConnected(p['id'] as String)).toList();
  Iterable<Transfer> get runningTransfers =>
      transfers.where((t) => t.phase == TransferPhase.running);
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
      for (final item
          in ((status['backup']?['active'] as List?) ?? []).whereType<Map>())
        {...item, 'source': 'backup', 'profile': selected},
      ...((status['mount_activity']?['active'] as List?) ?? []),
    ],
  ].whereType<Map>().map((v) => Map<String, dynamic>.from(v)).toList();
  Json transferInfo(Transfer task) => {
    'id': task.id,
    'profile': task.args['profile'],
    'revision': task.args['revision'],
    'name': task.name,
    'kind': task.upload ? 'upload' : 'download',
    'view': task.view,
    'source': 'manual',
    'state': task.stateKey,
    'path': task.args['path'],
    'local_path': task.args[task.upload ? 'source' : 'output'],
    'bytes_done': task.done,
    'bytes_total': task.total,
    'account':
        task.account ??
        profiles
            .where(
              (p) =>
                  p['id'] == task.args['profile'] &&
                  p['revision'] == task.args['revision'],
            )
            .firstOrNull?['name'] ??
        tr('原连接'),
  };
  List<TransferRow> get transferRows => [
    for (final task in transfers.where(
      (task) => task.phase == TransferPhase.running,
    ))
      (info: transferInfo(task), manual: task),
    for (final item in syncActivity) (info: item, manual: null),
    for (final task in transfers.where(
      (task) => task.phase != TransferPhase.running,
    ))
      (info: transferInfo(task), manual: task),
    for (final item
        in ((status['overall']?['recent'] as List?) ??
                (status['backup']?['recent'] as List?) ??
                [])
            .whereType<Map>())
      (
        info: {'source': 'backup', ...Map<String, dynamic>.from(item)},
        manual: null,
      ),
  ];

  String transferState(Json item) =>
      item['state']?.toString() ??
      (paused
          ? '已暂停'
          : item['source'] == 'backup'
          ? '正在备份'
          : '传输中');

  bool transferShowsProgress(Json item) {
    final state = transferState(item);
    final total = item['bytes_total'] as num? ?? 0;
    final done = item['bytes_done'] as num? ?? 0;
    return (state == '传输中' || state == '正在备份') && (total <= 0 || done < total);
  }

  String transferLabel(Json item) {
    final source = switch (item['source']) {
      'backup' => '备份',
      'manual' when item['view'] == true => '查看',
      'manual' => item['kind'] == 'download' ? '下载' : '上传',
      _ => '后台同步',
    };
    final account = item['account']?.toString();
    return account == null || account.isEmpty
        ? tr(source)
        : '${tr(source)} · $account';
  }

  bool get syncing =>
      status['overall']?['syncing'] == true ||
      busy ||
      runningTransfers.isNotEmpty ||
      syncActivity.isNotEmpty ||
      (status['backup']?['state'] == 'scanning' ||
          status['backup']?['state'] == 'starting') ||
      (status['cache']?['pending'] as num? ?? 0) > 0;
  Future<void> pauseToggle() =>
      action(paused ? tr('恢复同步') : tr('暂停同步'), () async {
        status = await request('pause', {'paused': !paused});
      });
  Future<void> signOut() => action(tr('退出登录'), () async {
    final id = selected;
    await request('sign_out', {'id': id});
    _invalidateDirectories(id);
    _showDirectory('/');
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
      throw StateError(tr('本机文件能力不可用'));
    }
    final result = await request(op, {
      'schema_version': 1,
      'id': ?id,
      'offline': ?offline,
    });
    if (result['schema_version'] != 1 ||
        result['scope'] != 'generated_fixture_only' ||
        result['items'] is! List) {
      throw StateError(tr('本机文件状态版本不兼容'));
    }
    return result;
  }

  bool get canPreflightLocalFile =>
      status['local_files']?['schema_version'] == 1 &&
      status['local_files']?['metadata_preflight'] == true;
  Future<Json> preflightLocalFile(String source, String kind) async {
    if (!canPreflightLocalFile) {
      throw StateError(tr('当前引擎不支持本地文件预检'));
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
      throw StateError(tr('本地预检响应不兼容，请更新引擎与界面'));
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
      tr(profile?['provider_label'] as String? ?? 'TwoDrive');
  String get accountLabel => tr(profile?['account_label'] as String? ?? '账号');
  String get account => connected
      ? (status['account_name'] as String? ??
            profile?['account_name'] as String? ??
            tr('账号名称暂不可用'))
      : tr('未登录');
  String get trayTitle {
    final connection = connected ? ' · $providerLabel · $account' : '';
    final host = hosting
        ? tr(' · 正在共享 {arg0}', [sharing['settings']?['name'] ?? ''])
        : '';
    return 'TwoDrive$connection$host${!connected && !hosting ? tr(' · 未连接') : ''}';
  }

  String get trayState => error != null || syncError
      ? 'error'
      : proxyFallback || status['overall']?['route_fallback'] == true
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
  _DirectoryOwner get _directoryOwner =>
      (profile: selected, revision: profile?['revision'] ?? '');
  bool get proxyFallback => connected && status['route_fallback'] == true;
  void requireProfile(Json args) {
    final id = args['profile'] as String? ?? selected;
    final owner = profiles.where((p) => p['id'] == id).firstOrNull;
    if (id == null || !isProfileConnected(id)) {
      throw StateError(tr('文件操作所属连接已改变，请刷新后重试'));
    }
    if (args['revision'] != null && args['revision'] != owner?['revision']) {
      throw StateError(tr('连接配置已更新，请刷新后重新选择文件'));
    }
  }

  Future<void> start(
    String engine,
    String state,
    String peer,
    String launcher, {
    bool server = false,
    String? nativeFixtureRoot,
  }) async {
    _stateDirectory = Directory(state);
    try {
      final saved = await _transferHistory.load(
        File('$state/transfer-history.json'),
      );
      if (_disposed) return;
      transfers.addAll(saved);
      for (final task in saved) {
        if (task.id > _id) _id = task.id;
      }
    } catch (_) {
      transferHistoryError = tr('无法读取本机传输记录');
    }
    if (_disposed) return;
    notifyListeners();
    try {
      await preferences.load(File('$state/desktop-settings.json'));
      desktopLocale.value = preferences.language;
      final p = await Process.start(engine, [
        if (server) '--tdlink-server',
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
          fail(tr('引擎返回无效数据'));
        }
      }, onError: (_) => fail(tr('引擎通信中断')));
      // Keep stderr drained without exposing private configuration in the UI.
      p.stderr.drain<void>();
      unawaited(
        p.exitCode.then((code) {
          if (!quitting) {
            fail(tr('本地引擎已退出（{arg0}）。请重新启动客户端。', [code]));
          }
        }),
      );
      available = true;
      if (server) {
        await refreshStatus();
        return;
      }
      // Operation notifications are opt-in. Older engines still support
      // the explicit reads made when a view opens or an action completes.
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
    } catch (e) {
      fail(tr('无法启动本地引擎：{arg0}', [e]));
    }
  }

  @visibleForTesting
  void accept(Json message) {
    if (_disposed) return;
    final id = message['id'] as int;
    if (id > _id) _id = id;
    if (message['event'] == 'operation') {
      if (message['op'] == 'upload' || message['op'] == 'download') {
        // Tracked uploads refresh on their reply. The completion event can
        // precede that reply, so starting both reads cancels useful work.
        if (message['op'] == 'upload' &&
            message['running'] != true &&
            !transfers.any((task) => task.id == id)) {
          final profile = message['profile'] as String?;
          _invalidateDirectories(profile);
          if (_showFiles && (profile == null || profile == selected)) {
            unawaited(refreshView());
          }
        }
        notifyListeners();
        return;
      }
      if (message['running'] == true) {
        _engineOperations.add(id);
        if (!_busy) {
          activeId = id;
          activity = tr('正在处理连接或文件操作');
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
        if (const ['mkdir', 'rename', 'delete'].contains(message['op'])) {
          _invalidateDirectories(message['profile'] as String?);
        }
        if (!quitting &&
            message['op'] != 'quit' &&
            (_showFiles || message['status'] is! Map)) {
          unawaited(refreshView());
        }
      }
      notifyListeners();
      return;
    }
    if (message['event'] == 'progress') {
      final task = transfers.where((t) => t.id == id).firstOrNull;
      if (task != null && task.phase == TransferPhase.running) {
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
      final text = message['error'] as String;
      reply.completeError(
        message['code'] == 'reauthorize'
            ? AuthorizationExpired(text)
            : StateError(text),
      );
    }
  }

  @visibleForTesting
  void fail(String message) {
    available = false;
    _engineOperations.clear();
    status = {};
    entries = [];
    _directoryFromMount = false;
    _views.clear();
    _directories.clear();
    _directoryChecked = null;
    _directoryError = null;
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
      return Future.error(StateError(tr('本地引擎未运行')));
    }
    if (_pending.length >= 32 && op != 'cancel') {
      return Future.error(StateError(tr('请求正在处理中，请稍后重试')));
    }
    final id = requestId ?? ++_id;
    if (_pending.containsKey(id)) {
      return Future.error(StateError(tr('请求编号重复')));
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
    final previous = _directoryOwner;
    profiles = (result['profiles'] as List).cast<Json>();
    providers = ((result['providers'] as List?) ?? []).cast<Json>();
    bool hasOwner(_DirectoryOwner owner) => profiles.any(
      (p) => p['id'] == owner.profile && p['revision'] == owner.revision,
    );
    _views.removeWhere((_, view) => !hasOwner(view.owner));
    _directories.removeWhere((key, _) => !hasOwner(key.owner));
    for (final id in quotas.keys.toList()) {
      if (!profiles.any(
        (p) => p['id'] == id && p['revision'] == _quotaVersions[id],
      )) {
        quotas.remove(id);
        _quotaChecked.remove(id);
        _quotaVersions.remove(id);
      }
    }
    connectionErrors.removeWhere(
      (id, _) => !profiles.any((p) => p['id'] == id),
    );
    selected = connected
        ? status['profile'] as String?
        : result['selected'] as String?;
    if (connected && previous != _directoryOwner) {
      _browseGeneration++;
      _showDirectory('/');
    }
  }

  Future<void> loadProfiles() async {
    _applyProfiles(await request('profiles'));
    notifyListeners();
  }

  void _applyStatus(Json next) {
    _statusVersion++;
    final previous = status['profile'];
    final wasConnected = connected;
    if (wasConnected && previous is String && previous != next['profile']) {
      _views[previous] = (
        owner: _directoryOwner,
        path: path,
        stale: _directoryStale,
      );
    }
    status = next;
    _views.removeWhere((id, _) => !isProfileConnected(id));
    _directories.removeWhere((key, _) {
      final id = key.owner.profile;
      return id == null || !isProfileConnected(id);
    });
    if (connected) selected = status['profile'] as String? ?? selected;
    if (!connected ||
        previous != status['profile'] ||
        wasConnected != connected) {
      _browseGeneration++;
      final view = connected ? _views[selected] : null;
      _showDirectory(view?.path ?? '/');
      if (view?.stale == true) _directoryStale = true;
    }
    _pollError = status['gateway_failed'] == true
        ? tr('配对连接已断开，请重连。')
        : status['mount_failed'] == true
        ? tr('挂载进程已退出；请卸载后重试。缓存已保留。')
        : null;
    if (_showQuota &&
        status['storage_usage'] == true &&
        available &&
        !quitting) {
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
    if (_showFiles) return _pollOnce(filesEpoch: _filesViewEpoch);
    return _polling ??= _pollOnce().whenComplete(() => _polling = null);
  }

  Future<void> _pollOnce({int? filesEpoch}) async {
    bool viewIsCurrent() =>
        filesEpoch == null || (_showFiles && filesEpoch == _filesViewEpoch);
    await Future<void>.value();
    try {
      if (busy || !available || quitting || _disposed || !viewIsCurrent()) {
        return;
      }
      final previous = selected;
      final owner = _directoryOwner;
      final epoch = _selectionEpoch;
      // Directory reads coalesce by the current path and generation. A
      // previous view's status or listing cannot own a reopened Files view.
      final listFirst = filesEpoch != null && connected;
      final listing = listFirst ? _refreshCurrentDirectory() : null;
      try {
        await refreshStatus();
        if (_disposed ||
            quitting ||
            !available ||
            !viewIsCurrent() ||
            epoch != _selectionEpoch) {
          return;
        }
        if (selected != previous) await loadProfiles();
        if (viewIsCurrent() && (!listFirst || owner != _directoryOwner)) {
          await _refreshCurrentDirectory();
        }
      } finally {
        await listing;
      }
    } catch (e) {
      if (!_disposed && !quitting) {
        _pollError = '$e'.replaceFirst('Bad state: ', '');
        notifyListeners();
      }
    }
  }

  Future<void> _refreshCurrentDirectory() async {
    if (_directoryStale ||
        _directoryChecked == null ||
        DateTime.now().difference(_directoryChecked!) >=
            const Duration(seconds: 15)) {
      await browse(path);
    }
  }

  Future<void> action(String label, Future<void> Function() run) async {
    if (busy) {
      return;
    }
    final statusVersion = _statusVersion;
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
      if (_showFiles || statusVersion == _statusVersion) {
        unawaited(refreshView());
      }
    }
  }

  // Optional maintenance restart: restore exactly the previously online IDs,
  // serially through this GUI-owned engine, then restore the selected account.
  Future<void> restoreConnectionsAtStartup(Iterable<String> ids) =>
      _restoreProfiles(ids.toSet());

  Future<void> connect([String? profileId]) {
    final id = profileId ?? selected;
    if (id != null && isProfileConnected(id)) {
      if (status['profile'] == id && _selectingProfile == null) {
        return Future.value();
      }
      return _selectConnected(id);
    }
    return action(tr('正在登录并连接'), () async {
      if (id == null) throw StateError(tr('请先添加连接'));
      if (connected && status['profile'] == id) return;
      _selectionEpoch++;
      _selectingProfile = null;
      _browseGeneration++;
      activeId = ++_id;
      try {
        late final Json result;
        try {
          result = await request('connect', {'id': id}, activeId);
          connectionErrors.remove(id);
          needsAuthorization.remove(id);
        } catch (e) {
          _recordConnectionError(id, e);
          return;
        }
        await refreshStatus();
        selected = id;
        if (result['entries'] is List) {
          path = '/';
          _saveDirectory(result['entries'] as List, target);
        }
        _directoryStale = result['entries'] is! List;
        if (!_directoryStale) _directoryChecked = DateTime.now();
        if (autoMountOnConnect && connected && canMount && !mounted) {
          activity = tr('已连接，正在自动挂载');
          activeId = ++_id;
          notifyListeners();
          await request('mount', {
            'profile': id,
            'revision': profile?['revision'],
          }, activeId);
          await refreshStatus();
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
    final oldRead = _directoryRead?.id;
    if (oldRead != null) {
      await _cancelDirectoryRead(oldRead);
    }
    try {
      final result = await request('select', {'id': id, 'revision': revision});
      if (_disposed || quitting || epoch != _selectionEpoch) return;
      if (!profiles.any((p) => p['id'] == id && p['revision'] == revision)) {
        throw StateError(tr('连接配置已更新，请刷新后重试'));
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
        if (_showFiles) unawaited(_refreshCurrentDirectory());
      }
    }
  }

  /// When enabled, restore every authenticated account once per app process.
  /// Disconnect is session-only; startup restoration is a separate preference.
  /// The engine refuses browser login here and validates revision and binding.
  Future<void> restoreOnStartup() => _startupRestore ??=
      restoreConnectionsOnStartup ? _restoreProfiles(null) : Future.value();

  Future<void> _restoreProfiles(Set<String>? requested) =>
      action(tr('正在恢复网盘连接和挂载'), () async {
        _restoringStartup = true;
        try {
          final preferred = selected;
          final candidates = profiles
              .where(
                (p) =>
                    p['startup_ready'] == true &&
                    (requested == null || requested.contains(p['id'])),
              )
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
              connectionErrors.remove(id);
              needsAuthorization.remove(id);
              if (quitting || _disposed) break;
              await refreshStatus();
            } catch (e) {
              if (!quitting) {
                _recordConnectionError(id, e);
              }
              continue;
            }
            if (quitting || _disposed) break;
            if (autoMountOnConnect && mountSupported && !isProfileMounted(id)) {
              try {
                activeId = reserveRequestId();
                await request('mount', {...owner, 'startup': true}, activeId);
                if (!quitting && !_disposed) await refreshStatus();
              } catch (e) {
                if (!quitting) {
                  failures.add(
                    tr('{arg0}：自动挂载失败，{arg1}', [
                      p['name'],
                      '$e'.replaceFirst('Bad state: ', ''),
                    ]),
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
      });
  Future<void> openWriteRecovery(String id) => action(tr('正在打开恢复目录'), () async {
    final result = await request('write_recovery', {'profile': id});
    final directories = result['directories'] as List;
    for (final directory in directories) {
      await openLocalDirectory(directory as String);
    }
  });
  Future<void> disconnect([String? profileId]) => action(tr('正在断开'), () async {
    final id = profileId ?? selected;
    _selectionEpoch++;
    _selectingProfile = null;
    await request('disconnect', {'id': ?id});
    _invalidateDirectories(id);
    if (id == selected) {
      _showDirectory('/');
    }
    await refreshStatus();
  });

  void _showDirectory(String directory) {
    path = directory;
    final key = (owner: _directoryOwner, path: directory);
    final cached = _directories.remove(key);
    if (cached != null) _directories[key] = cached;
    entries = cached?.entries ?? [];
    _directoryFromMount = false;
    _directoryChecked = cached?.checked;
    _directoryStale = cached == null;
    _directoryError = null;
  }

  void _saveDirectory(List listing, Json identity) {
    entries = listing.map((e) => {...e as Json, ...identity}).toList();
    _directoryFromMount = false;
    final checked = DateTime.now();
    _directoryChecked = checked;
    _directoryStale = false;
    _directoryError = null;
    final owner = _directoryOwner;
    final key = (owner: owner, path: path);
    _directories.remove(key);
    // Bound the memory used by revisited directories. The current view can
    // still display a listing too large to retain.
    if (owner.profile != null &&
        owner.revision != '' &&
        entries.length <= 2000) {
      _directories[key] = (entries: entries, checked: checked);
      if (_directories.length > 32) {
        _directories.remove(_directories.keys.first);
      }
    }
  }

  void _invalidateDirectories(String? id) {
    _directories.removeWhere((key, _) => id == null || key.owner.profile == id);
    if (id == null || id == selected) {
      _directoryStale = true;
      _browseGeneration++;
    }
  }

  Future<void> _cancelDirectoryRead(int id) async {
    try {
      await request('cancel', {'id': id});
    } catch (_) {
      // Only the current navigation may update the view.
    }
  }

  Future<void> browse(String directory) async {
    if (busy) return;
    try {
      await readDirectory(directory);
    } catch (_) {
      // readDirectory publishes failures for the current directory.
    }
  }

  Future<void> readDirectory(String directory) {
    if (!connected ||
        !available ||
        quitting ||
        _disposed ||
        _selectingProfile != null) {
      return Future.value();
    }
    final previous = _directoryRead;
    if (directory == path && directoryLoading) return previous!.future;
    if (directory != path) _showDirectory(directory);
    _directoryError = null;
    final generation = ++_browseGeneration;
    final requestId = ++_id;
    if (previous != null) unawaited(_cancelDirectoryRead(previous.id));
    final future = _readDirectory(directory, target, requestId, generation);
    _directoryRead = (id: requestId, generation: generation, future: future);
    notifyListeners();
    return future;
  }

  Future<void> _readDirectory(
    String directory,
    Json identity,
    int requestId,
    int generation,
  ) async {
    bool current() =>
        generation == _browseGeneration && available && !_disposed && !quitting;
    try {
      if (!directoryLoaded) {
        unawaited(() async {
          final preview = await loadMountedDirectory(directory);
          if (preview != null && current() && !directoryLoaded) {
            entries = preview.map((entry) => {...entry, ...identity}).toList();
            _directoryFromMount = true;
            notifyListeners();
          }
        }());
      }
      final result = await Future.sync(
        () => request('list', {'path': directory, ...identity}, requestId),
      );
      if (current()) _saveDirectory(result['entries'] as List, identity);
    } catch (e) {
      if (current()) {
        _directoryStale = true;
        _directoryError = '$e'.replaceFirst('Bad state: ', '');
        rethrow;
      }
    } finally {
      if (_directoryRead?.id == requestId) _directoryRead = null;
      if (current()) notifyListeners();
    }
  }

  @protected
  Future<List<Json>?> loadMountedDirectory(String directory) async {
    final id = selected;
    if (id == null || !isProfileConnected(id)) return null;
    final mount = mountForProfile(id);
    final root = profile?['mount_path'];
    final mountedPath = mount?['path'];
    if (root is! String || mountedPath is! String || mount?['failed'] == true) {
      return null;
    }
    return readMountedDirectory(
      configuredRoot: root,
      mountedPath: mountedPath,
      directory: directory,
    );
  }

  Future<void> mutate(String op, Json args) => action(tr('正在更新文件'), () async {
    requireProfile(args);
    final id = (args['profile'] ?? selected) as String?;
    await request(op, {...target, ...args});
    _invalidateDirectories(id);
    if (id == selected) await readDirectory(path);
  });
  Future<void> transfer(String op, Json args) async {
    if (busy || quitting || _disposed) return;
    try {
      requireProfile(args);
    } catch (e) {
      error = '$e'.replaceFirst('Bad state: ', '');
      notifyListeners();
      return;
    }
    final profile = args['profile'] ?? selected;
    final owner = profiles.where((p) => p['id'] == profile).firstOrNull;
    args = {
      ...args,
      'profile': profile,
      'revision': args['revision'] ?? owner?['revision'] ?? '',
    };
    final id = ++_id;
    final task = Transfer(id, op, args, account: owner?['name'] as String?);
    transfers.insert(0, task);
    _pruneTransfers(task.upload);
    final started = _saveTransferHistory();
    notifyListeners();
    await started;
    try {
      requireProfile(args);
      final result = await request(task.view ? 'download' : op, args, id);
      if ((result['size'] ?? result['bytes']) case final int size) {
        task.total = size;
      }
      if (task.view) await openViewedFile(args['output'] as String);
      task.phase = TransferPhase.completed;
      task.done = task.total;
      if (task.upload) _invalidateDirectories(profile);
    } catch (e) {
      task.phase = available ? TransferPhase.failed : TransferPhase.unconfirmed;
      task.error = '$e'.replaceFirst('Bad state: ', '');
      if (selected == args['profile']) error = task.error;
    } finally {
      _pruneTransfers(task.upload);
      await _saveTransferHistory();
      if (!_disposed) notifyListeners();
    }
    if (task.upload &&
        task.phase == TransferPhase.completed &&
        selected == args['profile'] &&
        connected &&
        !_disposed &&
        !quitting) {
      try {
        await readDirectory(path);
      } catch (_) {
        _directoryStale = true;
        if (!_disposed && !quitting && selected == args['profile']) {
          _pollError = tr('上传已完成，目录刷新失败');
          notifyListeners();
        }
      }
    }
  }

  void _pruneTransfers(bool upload) {
    var count = 0;
    transfers.removeWhere(
      (task) =>
          task.upload == upload &&
          ++count > _TransferHistory.limit &&
          task.phase != TransferPhase.running,
    );
  }

  Future<void> _saveTransferHistory() async {
    if (!_transferHistory.ready) return;
    try {
      await _transferHistory.save(transfers);
      transferHistoryError = null;
    } catch (_) {
      transferHistoryError = tr('无法保存本机传输记录');
    }
  }

  bool canRetryTransfer(Transfer task) {
    final profile = task.args['profile'];
    return !busy &&
        !quitting &&
        profile is String &&
        isProfileConnected(profile) &&
        profiles.any(
          (p) => p['id'] == profile && p['revision'] == task.args['revision'],
        );
  }

  Future<void> viewCloudFile(Json file) async {
    if (busy || quitting || _disposed) return;
    Directory? folder;
    try {
      requireProfile(file);
      final name = file['name'] as String;
      if (name.isEmpty ||
          name == '.' ||
          name == '..' ||
          name.contains(RegExp(r'[/\\\x00]')) ||
          (Platform.isWindows && name.contains(RegExp(r'[:*?"<>|]')))) {
        throw StateError(tr('该文件名无法用于本机查看'));
      }
      final state = _stateDirectory;
      if (state == null) throw StateError(tr('本机查看目录暂不可用'));
      final views = Directory('${state.absolute.path}/viewed-files');
      await views.create(recursive: true);
      folder = await views.createTemp('file-');
      await transfer('view', {
        'profile': file['profile'],
        'revision': file['revision'],
        'path': file['path'],
        'size': file['size'],
        'output': '${folder.path}${Platform.pathSeparator}$name',
      });
    } catch (e) {
      if (!_disposed) {
        error = '$e'.replaceFirst('Bad state: ', '');
        notifyListeners();
      }
    } finally {
      if (folder != null) {
        try {
          if (await folder.list().isEmpty) await folder.delete();
        } on FileSystemException {
          // The user may have moved the copy while it was open.
        }
      }
    }
  }

  @protected
  Future<void> openViewedFile(String path) => openLocalFile(path);

  Future<void> cancelTransfer(int id) =>
      request('cancel', {'id': id}).then((_) {});
  Future<void> cancel() async {
    final id = activeId ?? runningTransfers.firstOrNull?.id;
    if (id != null) {
      activity = tr('正在取消，等待当前网络读写返回');
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
    return action(op == 'unmount' ? tr('正在卸载') : tr('正在挂载'), () async {
      activeId = ++_id;
      await request(op, owner, activeId);
      await refreshStatus();
    });
  }

  Future<void> openMountFolder(
    String id, {
    Future<void> Function(String)? openDirectory,
  }) => action(tr('打开挂载'), () async {
    final mount = mountForProfile(id);
    if (mount == null || mount['failed'] == true || mount['path'] is! String) {
      throw StateError(tr('请先挂载此网盘；挂载异常时请卸载后重试'));
    }
    await (openDirectory ?? openLocalDirectory)(mount['path'] as String);
  });

  String? mountedTransferPath(Json info) {
    final profile = info['profile'];
    final revision = info['revision'];
    final remote = info['path'];
    if (profile is! String ||
        revision is! String ||
        remote is! String ||
        !remote.startsWith('/') ||
        remote.contains('\\') ||
        remote.contains('\u0000')) {
      return null;
    }
    final mount = mountForProfile(profile);
    final root = mount?['path'];
    if (root is! String ||
        root.isEmpty ||
        mount?['failed'] == true ||
        mount?['revision'] != revision) {
      return null;
    }
    final parts = remote.substring(1).split('/');
    if (parts.any((part) => part.isEmpty || part == '.' || part == '..')) {
      return null;
    }
    if (root.startsWith('dav://')) {
      final uri = Uri.tryParse(root);
      if (uri == null ||
          uri.host != '127.0.0.1' ||
          !uri.hasPort ||
          uri.path != '/' ||
          uri.hasQuery ||
          uri.hasFragment ||
          !RegExp(r'^iCloud-[a-zA-Z0-9_-]{8}$').hasMatch(uri.userInfo)) {
        return null;
      }
      return uri.replace(pathSegments: parts).toString();
    }
    try {
      return Directory(
        root,
      ).uri.resolveUri(Uri(pathSegments: parts)).toFilePath();
    } on UnsupportedError {
      return null;
    }
  }

  Future<bool> revealTransferFile(String path) async {
    if (Platform.isLinux && path.startsWith('dav://')) {
      return revealGnomeFile(Uri.parse(path));
    }
    final type = await FileSystemEntity.type(path, followLinks: false).timeout(
      const Duration(seconds: 5),
      onTimeout: () => FileSystemEntityType.notFound,
    );
    if (type == FileSystemEntityType.notFound) {
      throw StateError(tr('文件已移动或该位置暂不可用'));
    }
    return revealLocalFile(path);
  }

  Future<String?> save(Json profile) async {
    String? id;
    await action(tr('保存连接'), () async {
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
          ? tr('正在启动共享')
          : op == 'share_export'
          ? tr('正在导出连接文件')
          : tr('正在更新共享'),
      () async {
        activeId = ++_id;
        result = await request(op, args, activeId);
        await refreshStatus();
      },
    );
    return result;
  }

  Future<void> remove(String id) => action(tr('删除连接配置'), () async {
    await request('remove_profile', {'id': id});
    await loadProfiles();
  });
  Future<void> autostart(bool enabled) => action(tr('更新自启动设置'), () async {
    status = await request('autostart', {'enabled': enabled});
  });
  Future<void> retryDeletes() => action(tr('安排云端删除重试'), () async {
    await request('retry_deletes', {'id': status['profile'] ?? selected});
    await refreshStatus();
  });
  Future<void> clearCache() => action(tr('释放已同步缓存'), () async {
    await request('clear_cache', {'id': selected});
    await refreshStatus();
  });
  @protected
  Future<void> closeTransport() async => _process?.stdin.close();

  List<Json> quitBlockers = const [];

  Future<bool> shutdown({List<Json> closeMountProcesses = const []}) {
    final existing = _shutdown;
    if (existing != null) return existing;
    final attempt = _shutdownOnce(closeMountProcesses);
    _shutdown = attempt;
    unawaited(
      attempt.then((done) {
        if (!done) _shutdown = null;
      }),
    );
    return attempt;
  }

  Future<bool> _shutdownOnce(List<Json> closeMountProcesses) async {
    quitBlockers = const [];
    if ((busy && !_restoringStartup) || runningTransfers.isNotEmpty) {
      error = tr('请先完成或取消当前操作，再退出。');
      notifyListeners();
      return false;
    }
    quitting = true;
    activity = tr('正在退出，正在收尾连接…');
    notifyListeners();
    try {
      if (_transferHistory.flushed case final writes?) await writes;
      if (available) {
        if (_restoringStartup && activeId != null) {
          await request('cancel', {'id': activeId});
        }
        final reply = await request('quit', {
          'mount_confirmation': true,
          if (closeMountProcesses.isNotEmpty)
            'close_mount_processes': closeMountProcesses,
        });
        quitBlockers =
            (reply['quit_blockers'] as List?)?.cast<Json>() ?? const [];
        if (quitBlockers.isNotEmpty) {
          _resumeAfterQuitRefusal();
          return false;
        }
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
      _resumeAfterQuitRefusal('$e'.replaceFirst('Bad state: ', ''));
      return false;
    }
  }

  void _resumeAfterQuitRefusal([String? message]) {
    quitting = false;
    activity = '';
    error = message;
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _transferRefresh?.cancel();
    if (!quitting) updates.dispose();
    super.dispose();
  }
}
