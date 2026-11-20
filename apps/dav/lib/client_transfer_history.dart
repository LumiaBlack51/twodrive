part of 'client.dart';

class _TransferHistory {
  static const limit = 100;
  File? _file;
  bool get ready => _file != null;
  Future<void>? flushed;

  Future<List<Transfer>> load(File file) async {
    final type = await FileSystemEntity.type(file.path, followLinks: false);
    if (type == FileSystemEntityType.notFound) {
      _file = file;
      return [];
    }
    if (type != FileSystemEntityType.file) {
      throw const FormatException('Invalid transfer history file');
    }
    final data = jsonDecode(await file.readAsString());
    if (data is! Json || data['version'] != 1 || data['transfers'] is! List) {
      throw const FormatException('Invalid transfer history schema');
    }
    final records = <Transfer>[];
    final ids = <int>{};
    for (final row in data['transfers'] as List) {
      final record = _decode(row);
      if (!ids.add(record.id)) {
        throw const FormatException('Duplicate transfer history record');
      }
      records.add(record);
    }
    _file = file;
    return records;
  }

  Future<void> save(Iterable<Transfer> transfers) {
    final file = _file!;
    final contents = jsonEncode({
      'version': 1,
      'transfers': transfers.map(_encode).toList(),
    });
    final write = (flushed ?? Future<void>.value()).then((_) async {
      final folder = await file.parent.createTemp('.transfer-history-');
      try {
        final temporary = File('${folder.path}/history.json');
        await temporary.writeAsString(contents, flush: true);
        await temporary.rename(file.path);
      } finally {
        await folder.delete(recursive: true);
      }
    });
    // A failed write is reported to its caller. Later writes can still retry.
    flushed = write.then<void>((_) {}, onError: (Object _, StackTrace _) {});
    return write;
  }

  static Json _encode(Transfer task) => {
    'id': task.id,
    'operation': task.operation,
    'profile': task.args['profile'],
    'revision': task.args['revision'],
    'path': task.args['path'],
    'local_path': task.args[task.upload ? 'source' : 'output'],
    if (task.args['etag'] case final String etag) 'etag': etag,
    if (task.account != null) 'account': task.account,
    'done': task.done,
    'total': task.total,
    'result': switch (task.state) {
      '已完成' => 'complete',
      '未完成' => 'failed',
      _ => 'unconfirmed',
    },
  };

  static Transfer _decode(dynamic row) {
    if (row is! Json ||
        row['id'] is! int ||
        (row['id'] as int) < 1 ||
        !const ['upload', 'download', 'view'].contains(row['operation']) ||
        row['profile'] is! String ||
        row['revision'] is! String ||
        row['path'] is! String ||
        row['local_path'] is! String ||
        row['done'] is! int ||
        row['total'] is! int ||
        !const ['complete', 'failed', 'unconfirmed'].contains(row['result'])) {
      throw const FormatException('Invalid transfer history record');
    }
    final operation = row['operation'] as String;
    return Transfer(
        row['id'] as int,
        operation,
        {
          'profile': row['profile'],
          'revision': row['revision'],
          'path': row['path'],
          operation == 'upload' ? 'source' : 'output': row['local_path'],
          if (row['etag'] case final String etag) 'etag': etag,
          if (operation != 'upload') 'size': row['total'],
        },
        account: row['account'] is String ? row['account'] as String : null,
      )
      ..done = row['done'] as int
      ..total = row['total'] as int
      ..state = switch (row['result']) {
        'complete' => '已完成',
        'failed' => '未完成',
        _ => '结果未确认',
      };
  }
}
