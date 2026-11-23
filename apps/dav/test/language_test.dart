import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/language.dart';
import 'package:twodrive_dav/main.dart';

void main() {
  test('language selection preserves active transfer tracking', () {
    final c = StorageClient();
    final task = Transfer(1, 'upload', {'path': '/test'});
    c.transfers.add(task);
    try {
      desktopLocale.value = 'en';
      expect(c.runningTransfers.single, task);
      expect(task.state, 'Transferring');
    } finally {
      desktopLocale.value = 'zh';
      c.dispose();
    }
  });
  testWidgets(
    'switches the current settings and keeps user-defined connection names',
    (tester) async {
      final c = StorageClient()
        ..profiles = [
          {'id': 'one', 'kind': 'peer', 'name': '草稿'},
        ];
      addTearDown(() => desktopLocale.value = 'zh');
      await tester.pumpWidget(DavApp(client: c));
      await tester.tap(find.text('设置').first);
      await tester.pumpAndSettle();
      await tester.tap(find.text('简体中文'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('English').last);
      await tester.pumpAndSettle();
      expect(find.text('Settings'), findsWidgets);
      expect(find.text('Language'), findsOneWidget);
      expect(find.text('设置'), findsNothing);
      await tester.tap(find.text('Connections').first);
      await tester.pumpAndSettle();
      expect(find.text('草稿'), findsOneWidget);
      expect(find.text('Sign in and connect'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      c.dispose();
    },
  );
}
