import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/language.dart';

const profile = {'id': 'account', 'revision': 'one', 'name': 'Example Drive'};
const identity = {'profile': 'account', 'revision': 'one'};

class HistoryClient extends StorageClient {
  final calls = <String>[];
  final opened = <String>[];
  final openedOriginals = <String>[];
  String mountedFile = '';
  final uploadStarted = Completer<void>();
  Completer<Json>? heldUpload;
  String revision = 'one';

  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    calls.add(op);
    switch (op) {
      case 'profiles':
        return {
          'profiles': [
            {...profile, 'revision': revision},
          ],
          'selected': profile['id'],
        };
      case 'status':
        return {'profile': profile['id'], 'connected': true};
      case 'upload':
        if (!uploadStarted.isCompleted) uploadStarted.complete();
        if (heldUpload case final hold?) return hold.future;
        return {'size': 7};
      case 'mounted_file':
        return {'path': mountedFile};
      case 'download':
        await File(args['output'] as String).writeAsString('fixture');
        return {'bytes': 7};
      case 'list':
        throw StateError('directory unavailable after successful upload');
      default:
        return {};
    }
  }

  @override
  Future<void> openViewedFile(String path) async {
    expect(await File(path).readAsString(), 'fixture');
    opened.add(path);
  }

  @override
  Future<void> openMountedOriginal(String path) async {
    openedOriginals.add(path);
  }
}

Future<File> stubEngine(Directory directory) async {
  final engine = File('${directory.path}/engine');
  await engine.writeAsString('#!/bin/sh\nexec cat >/dev/null\n');
  expect((await Process.run('chmod', ['700', engine.path])).exitCode, 0);
  return engine;
}

void main() {
  test(
    'English completed transfers keep their result after restart',
    () async {
      final temp = await Directory.systemTemp.createTemp(
        'twodrive-locale-history-',
      );
      addTearDown(() => temp.delete(recursive: true));
      addTearDown(() => desktopLocale.value = 'zh');
      final engine = await stubEngine(temp);
      final first = HistoryClient();
      await first.start(engine.path, temp.path, '', '');
      desktopLocale.value = 'en';
      await first.transfer('upload', {
        ...identity,
        'path': '/example.txt',
        'source': '${temp.path}/example.txt',
      });
      expect(first.transfers.single.phase, TransferPhase.completed);
      expect(first.transferInfo(first.transfers.single)['state'], '已完成');
      expect(await first.shutdown(), isTrue);
      first.dispose();
      final second = HistoryClient();
      addTearDown(() async {
        await second.shutdown();
        second.dispose();
      });
      await second.start(engine.path, temp.path, '', '');
      expect(second.transfers.single.phase, TransferPhase.completed);
      desktopLocale.value = 'zh';
      expect(second.transfers.single.state, '已完成');
      expect(second.calls, isNot(contains('upload')));
    },
    skip: Platform.isWindows,
  );

  test('file-manager opens stay outside actual transfer history', () {
    final c = StorageClient()
      ..status = {
        'opened_files': [
          {
            'name': 'viewed.docx',
            'source': 'nautilus',
            'kind': 'download',
            'state': '已打开',
          },
        ],
        'overall': {
          'recent': [
            {'name': 'backup.txt', 'kind': 'upload', 'state': '已备份'},
          ],
        },
      };
    addTearDown(c.dispose);
    final download = Transfer(10, 'download', {'path': '/saved.docx'})
      ..state = '已完成';
    c.transfers.add(download);
    expect(c.transferRows.map((row) => row.info['name']), [
      'saved.docx',
      'backup.txt',
    ]);
    expect(c.transferShowsProgress(c.transferInfo(download)), isFalse);
    download.state = '传输中';
    expect(c.transferShowsProgress(c.transferInfo(download)), isTrue);
  });

  test('mounted view opens only the path verified by the engine', () async {
    final mount = await Directory.systemTemp.createTemp('dav-mounted-route-');
    addTearDown(() => mount.delete(recursive: true));
    final verified = await File(
      '${mount.path}/verified.pdf',
    ).writeAsString('fixture');
    final client = HistoryClient()
      ..profiles = [profile]
      ..selected = 'account'
      ..status = {
        'profile': 'account',
        'connected': true,
        'mounts': [
          {...identity, 'path': mount.path},
        ],
      }
      ..mountedFile = verified.path;
    addTearDown(client.dispose);

    await client.viewCloudFile({...identity, 'path': '/document.pdf'});

    expect(client.error, isNull);
    expect(client.calls, ['mounted_file']);
    expect(client.openedOriginals, [verified.path]);
    expect(client.opened, isEmpty);
    expect(client.transfers, isEmpty);
  });

  test(
    'upload, download, and view survive restart with exact paths',
    () async {
      final temp = await Directory.systemTemp.createTemp('twodrive-history-');
      addTearDown(() => temp.delete(recursive: true));
      final engine = await stubEngine(temp);
      final first = HistoryClient();
      await first.start(engine.path, temp.path, '', '');
      final local = '${temp.path}/上传 #.txt';
      final output = '${temp.path}/下载 #.txt';
      await first.transfer('upload', {
        ...identity,
        'path': '/测试/上传.txt',
        'source': local,
        'etag': 'original-version',
        'unrelated': 'never persisted',
      });
      await first.transfer('download', {
        ...identity,
        'path': '/测试/下载.txt',
        'output': output,
      });
      first.mountedFile = output;
      first.status['mounts'] = [
        {'profile': 'account', 'revision': 'one', 'path': temp.path},
      ];
      await first.viewCloudFile({
        ...identity,
        'name': '查看 #.docx',
        'path': '/测试/查看 #.docx',
        'size': 7,
      });
      expect(first.opened, isEmpty);
      final viewedPath = first.openedOriginals.single;
      expect(viewedPath, output);
      expect(await first.shutdown(), isTrue);
      first.dispose();

      final second = HistoryClient()..revision = 'two';
      addTearDown(() async {
        await second.shutdown();
        second.dispose();
      });
      await second.start(engine.path, temp.path, '', '');
      final rows = second.transferRows;
      expect(rows.map((r) => r.manual!.operation), ['download', 'upload']);
      expect(rows.map((r) => r.info['local_path']), [output, local]);
      expect(rows.map((r) => r.info['state']), List.filled(2, '已完成'));
      expect(rows.map((r) => r.info['bytes_total']), [7, 7]);
      expect(rows.last.info['account'], 'Example Drive');
      expect(rows.last.manual!.args['etag'], 'original-version');
      expect(rows.last.manual!.args, isNot(contains('unrelated')));
      expect(second.canRetryTransfer(rows.last.manual!), isFalse);
      expect(second.opened, isEmpty);
      expect(second.openedOriginals, isEmpty);
      expect(second.calls, isNot(contains(anyOf('upload', 'download'))));
      expect(await File(viewedPath).exists(), isTrue);
    },
    skip: Platform.isWindows,
  );

  test(
    'a saved in-flight upload stays unconfirmed and is not replayed',
    () async {
      final temp = await Directory.systemTemp.createTemp('twodrive-history-');
      addTearDown(() => temp.delete(recursive: true));
      final engine = await stubEngine(temp);
      final original = await Directory('${temp.path}/original').create();
      final restarted = await Directory('${temp.path}/restart').create();
      final first = HistoryClient()..heldUpload = Completer<Json>();
      await first.start(engine.path, original.path, '', '');
      final pending = first.transfer('upload', {
        ...identity,
        'path': '/pending.txt',
        'source': '${temp.path}/pending.txt',
      });
      await first.uploadStarted.future;
      final originalHistory = File('${original.path}/transfer-history.json');
      await originalHistory.copy('${restarted.path}/transfer-history.json');
      final other = first.transfer('download', {
        ...identity,
        'path': '/other.txt',
        'output': '${temp.path}/other.txt',
      });
      first.heldUpload!.complete({'size': 9});
      await Future.wait([pending, other]);
      expect(await first.shutdown(), isTrue);
      first.dispose();
      final written = jsonDecode(await originalHistory.readAsString()) as Json;
      expect((written['transfers'] as List).map((row) => row['result']), [
        'complete',
        'complete',
      ]);

      final second = HistoryClient();
      addTearDown(() async {
        await second.shutdown();
        second.dispose();
      });
      await second.start(engine.path, restarted.path, '', '');
      expect(second.transfers.single.state, '结果未确认');
      expect(second.runningTransfers, isEmpty);
      expect(second.calls, isNot(contains('upload')));
      final recoveredId = second.transfers.single.id;
      await second.transfer('upload', {
        ...identity,
        'path': '/new.txt',
        'source': '${temp.path}/new.txt',
      });
      expect(second.transfers.first.id, greaterThan(recoveredId));
      expect(second.transfers.last.state, '结果未确认');
    },
    skip: Platform.isWindows,
  );

  test(
    'unreadable history survives new transfers without being overwritten',
    () async {
      final temp = await Directory.systemTemp.createTemp('twodrive-history-');
      addTearDown(() => temp.delete(recursive: true));
      final engine = await stubEngine(temp);
      final stored = File('${temp.path}/transfer-history.json');
      await stored.writeAsString('bad history');
      final client = HistoryClient();
      addTearDown(() async {
        await client.shutdown();
        client.dispose();
      });
      await client.start(engine.path, temp.path, '', '');
      await client.transfer('upload', {
        ...identity,
        'path': '/new.txt',
        'source': '${temp.path}/new.txt',
      });
      expect(client.transferHistoryError, '无法读取本机传输记录');
      expect(client.transfers.single.state, '已完成');
      expect(await stored.readAsString(), 'bad history');
    },
    skip: Platform.isWindows,
  );
}
