import 'dart:io';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/file_manager.dart';

void main() {
  test(
    'Windows downloaded copy is marked before opening a supported document',
    () async {
      final temp = await Directory.systemTemp.createTemp('dav-view-');
      addTearDown(() => temp.delete(recursive: true));
      final document = File('${temp.path}/report.PDF');
      await document.writeAsString('fixture');
      final calls = <List<String>>[];
      Future<void> record(String exe, List<String> args) async {
        expect(exe, 'explorer.exe');
        if (args.length == 1) {
          expect(
            await File('${document.path}:Zone.Identifier').readAsString(),
            contains('ZoneId=3'),
          );
        }
        calls.add(args);
      }

      await openLocalFile(document.path, platform: 'windows', launch: record);
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
    'Windows mounted originals open without creating or replacing a zone stream',
    () async {
      final temp = await Directory.systemTemp.createTemp('dav-mounted-view-');
      addTearDown(() => temp.delete(recursive: true));
      final unmarked = File('${temp.path}/原始文档 #1.PDF');
      final marked = File('${temp.path}/原始文档 #2.PDF');
      await unmarked.writeAsString('first fixture');
      await marked.writeAsString('second fixture');
      final firstZone = File('${unmarked.path}:Zone.Identifier');
      final secondZone = File('${marked.path}:Zone.Identifier');
      const existingZone = '[ZoneTransfer]\r\nZoneId=4\r\n';
      await secondZone.writeAsString(existingZone);
      final calls = <List<String>>[];
      Future<void> record(String exe, List<String> args) async {
        expect(exe, 'explorer.exe');
        calls.add(args);
      }

      await openMountedFile(unmarked.path, platform: 'windows', launch: record);
      await openMountedFile(marked.path, platform: 'windows', launch: record);
      final executable = '${temp.path}/document.pdf.exe';
      await openMountedFile(executable, platform: 'windows', launch: record);

      expect(await firstZone.exists(), isFalse);
      expect(await secondZone.readAsString(), existingZone);
      expect(await unmarked.readAsString(), 'first fixture');
      expect(await marked.readAsString(), 'second fixture');
      expect(calls, [
        [unmarked.path],
        [marked.path],
        ['/select,', executable],
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
