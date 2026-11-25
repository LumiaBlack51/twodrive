import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/overview_page.dart';

void main() {
  for (final scale in [1.0, 1.5, 2.0]) {
    for (final busySide in ['backup', 'transfers']) {
      testWidgets('overview previews share both edges: $busySide at $scale', (
        tester,
      ) async {
        tester.view.physicalSize = const Size(1200, 2400);
        tester.view.devicePixelRatio = 1;
        tester.platformDispatcher.textScaleFactorTestValue = scale;
        addTearDown(tester.view.resetPhysicalSize);
        addTearDown(tester.view.resetDevicePixelRatio);
        addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
        final client = StorageClient()..available = true;
        client.status = {
          if (busySide == 'backup')
            'accounts': [
              for (var i = 0; i < 3; i++)
                {
                  'name': 'Example backup $i',
                  'backup': {'folders': 1, 'state': 'scanning'},
                },
            ],
        };
        if (busySide == 'transfers') {
          for (var i = 0; i < 3; i++) {
            client.transfers.add(
              Transfer(i + 1, 'upload', {'path': '/example-$i.txt'}),
            );
          }
        }
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: OverviewPage(
                client: client,
                onNavigate: (_) {},
                onOpenMount: (_) async {},
              ),
            ),
          ),
        );
        Rect bounds(String title) => tester.getRect(
          find
              .ancestor(of: find.text(title), matching: find.byType(Container))
              .first,
        );
        final transfers = bounds('传输概览');
        final backup = bounds('备份概览');
        expect(transfers.top, closeTo(backup.top, .01));
        expect(transfers.bottom, closeTo(backup.bottom, .01));
        expect(tester.takeException(), isNull);
        await tester.pumpWidget(const SizedBox());
        client.dispose();
      });
    }
  }
}
