import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/profile_dialog.dart';

class BrowserClient extends StorageClient {
  final calls = <Json>[];
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    expect(op, 'provider_action');
    calls.add({...args});
    if (args['action'] == 'configure') {
      return {
        'auth': {
          'title': '官方网页授权',
          'confirmation': true,
          'help': '在官方网页完成授权后继续。',
          'display_code': 'ABCD-EFGH',
          'verification_url': 'https://openapi.baidu.com/device',
          'submit_label': '我已授权，继续',
          'attempt': 'current-attempt',
          'reply_action': 'auth_reply',
          'cancel_action': 'auth_cancel',
          'context': {'private_state': '/private/example'},
        },
      };
    }
    expect(args['attempt'], 'current-attempt');
    return {
      'authenticated': args['action'] == 'auth_reply',
      'options': {'private_state': '/private/example'},
      'message': args['action'] == 'auth_reply' ? '授权成功' : '已取消',
    };
  }
}

const browserProviders = <Json>[
  {
    'id': 'browser-example',
    'label': '网页授权网盘',
    'network_modes': ['system'],
    'fields': [
      {
        'key': 'app_key',
        'label': 'AppKey',
        'transient': true,
        'omit_from_profile': true,
      },
      {
        'key': 'secret_key',
        'label': 'SecretKey',
        'secret': true,
        'transient': true,
        'omit_from_profile': true,
      },
      {
        'key': 'private_state',
        'label': '私有目录',
        'advanced': true,
        'read_only': true,
      },
    ],
    'actions': [
      {
        'id': 'configure',
        'label': '官方授权',
        'credential_fields': ['app_key', 'secret_key'],
        'input_fields': ['private_state'],
      },
    ],
  },
];

void main() {
  for (final cancel in [false, true]) {
    testWidgets(
      'browser authorization confirms or cancels without another credential input: $cancel',
      (tester) async {
        tester.view.physicalSize = const Size(780, 660);
        tester.view.devicePixelRatio = 1;
        addTearDown(tester.view.resetPhysicalSize);
        addTearDown(tester.view.resetDevicePixelRatio);
        final client = BrowserClient();
        Json? saved;
        await tester.pumpWidget(
          MaterialApp(
            home: Builder(
              builder: (context) => Scaffold(
                body: TextButton(
                  onPressed: () async {
                    saved = await showDialog<Json>(
                      context: context,
                      builder: (_) => ProfileDialog(
                        providers: browserProviders,
                        client: client,
                      ),
                    );
                  },
                  child: const Text('打开'),
                ),
              ),
            ),
          ),
        );
        await tester.tap(find.text('打开'));
        await tester.pumpAndSettle();
        await tester.enterText(
          find.byKey(const Key('profile-app_key')),
          'synthetic-app',
        );
        await tester.enterText(
          find.byKey(const Key('profile-secret_key')),
          'synthetic-secret',
        );
        await tester.ensureVisible(find.text('官方授权'));
        await tester.tap(find.text('官方授权'));
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 500));
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 500));
        expect(find.text('ABCD-EFGH'), findsOneWidget);
        expect(find.text('https://openapi.baidu.com/device'), findsOneWidget);
        expect(find.byKey(const Key('provider-auth-answer')), findsNothing);
        expect(tester.takeException(), isNull);
        await tester.tap(find.text(cancel ? '取消登录' : '我已授权，继续'));
        await tester.pumpAndSettle();
        expect(client.calls.first['secret_key'], 'synthetic-secret');
        expect(
          client.calls.last['action'],
          cancel ? 'auth_cancel' : 'auth_reply',
        );
        expect(client.calls.last.containsKey('secret_key'), false);
        expect(client.calls.last.containsKey('app_key'), false);
        expect(find.text('synthetic-secret'), findsNothing);
        await tester.tap(find.text(cancel ? '保存' : '保存并连接'));
        await tester.pumpAndSettle();
        final encoded = jsonEncode(saved);
        expect(encoded, isNot(contains('synthetic-app')));
        expect(encoded, isNot(contains('synthetic-secret')));
        expect(saved?['_connect'] == true, !cancel);
        await tester.pumpWidget(const SizedBox());
        client.dispose();
      },
    );
  }
}
