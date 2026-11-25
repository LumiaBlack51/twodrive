import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/file_manager.dart';

void main() {
  test(
    'Windows cloud view preserves origin and only opens supported documents',
    () async {
      final temp = await Directory.systemTemp.createTemp('dav-view-');
      addTearDown(() => temp.delete(recursive: true));
      final document = File('${temp.path}/report.PDF');
      await document.writeAsString('fixture');
      final calls = <List<String>>[];
      Future<void> record(String exe, List<String> args) async {
        expect(exe, 'explorer.exe');
        calls.add(args);
      }

      await openLocalFile(document.path, platform: 'windows', launch: record);
      expect(
        await File('${document.path}:Zone.Identifier').readAsString(),
        contains('ZoneId=3'),
      );
      final program = '${temp.path}/report.pdf.exe';
      await openLocalFile(program, platform: 'windows', launch: record);
      final unavailable = '${temp.path}/missing/report.pdf';
      await openLocalFile(unavailable, platform: 'windows', launch: record);
      expect(calls, [
        [document.path],
        ['/select,', program],
        ['/select,', unavailable],
      ]);
    },
  );

  test(
    'native file manager receives a single unsplit Unicode directory argument',
    () async {
      for (final target in {
        'windows': 'explorer.exe',
        'linux': 'xdg-open',
      }.entries) {
        await openLocalDirectory(
          r'C:\generated folder\中文',
          platform: target.key,
          launch: (exe, args) async {
            expect(exe, target.value);
            expect(args, [r'C:\generated folder\中文']);
          },
        );
      }
      await expectLater(
        openLocalDirectory(
          'generated',
          platform: 'windows',
          launch: (_, _) async => throw StateError('launch failed'),
        ),
        throwsStateError,
      );
    },
  );
}
