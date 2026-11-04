import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/profile_dialog.dart';

class AuthClient extends StorageClient {
  final calls = <Json>[];
  int stage = 0;
  Json question(String label, bool secret) => {
    'options': {'account_state': '/private/example', 'writable': false},
    'auth': {
      'title': '测试账号登录',
      'label': label,
      'secret': secret,
      'required': true,
      'attempt': 'step-$stage',
      'reply_action': 'auth_reply',
      'cancel_action': 'auth_cancel',
      'context': {'account_state': '/private/example'},
    },
  };
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    expect(op, 'provider_action');
    calls.add({...args});
    if (args['action'] == 'configure') return question('账号', false);
    if (args['action'] == 'auth_cancel') return {'message': '已取消登录'};
    expect(args['attempt'], 'step-$stage');
    stage++;
    if (stage == 1) return question('密码', true);
    if (stage == 2) return question('验证码', true);
    return {
      'authenticated': true,
      'message': '登录成功',
      'options': {'account_state': '/private/example', 'writable': false},
    };
  }
}

const providers = <Json>[
  {
    'id': 'example',
    'label': '示例网盘',
    'network_modes': ['system'],
    'fields': [
      {
        'key': 'account_state',
        'label': '私有目录',
        'advanced': true,
        'read_only': true,
      },
    ],
    'actions': [
      {
        'id': 'configure',
        'label': '登录账号',
        'input_fields': ['account_state'],
      },
    ],
  },
];

void main() {
  Future<void> showQuestion(WidgetTester tester) async {
    // The parent action deliberately shows a spinner while awaiting this form.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
  }

  testWidgets(
    'generic GUI asks one question at a time and never saves answers as profile fields',
    (tester) async {
      final client = AuthClient();
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
                      client: client,
                      providers: providers,
                      original: const {
                        'id': 'p',
                        'kind': 'example',
                        'name': 'Test',
                      },
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
      await tester.tap(find.text('登录账号'));
      await showQuestion(tester);
      for (final step in [
        ('账号', 'test@example.invalid', false),
        ('密码', 'synthetic-password', true),
        ('验证码', '123456', true),
      ]) {
        final field = find.byKey(const Key('provider-auth-answer'));
        expect(field, findsOneWidget);
        final text = tester.widget<TextField>(
          find.descendant(of: field, matching: find.byType(TextField)),
        );
        expect(text.obscureText, step.$3);
        expect(text.controller!.text, isEmpty);
        expect(find.text(step.$1), findsWidgets);
        await tester.enterText(field, step.$2);
        await tester.tap(find.text('下一步'));
        await showQuestion(tester);
      }
      expect(find.byKey(const Key('provider-auth-answer')), findsNothing);
      expect(find.text('登录成功'), findsOneWidget);
      await tester.tap(find.text('保存并连接'));
      await tester.pumpAndSettle();
      expect(saved?['_connect'], true);
      expect(saved?['account_state'], '/private/example');
      final serialized = jsonEncode(saved);
      expect(serialized, isNot(contains('synthetic-password')));
      expect(serialized, isNot(contains('123456')));
      expect(serialized, isNot(contains('test@example.invalid')));
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets(
    'cancel sends only the current attempt and does not submit a secret',
    (tester) async {
      final client = AuthClient();
      await tester.pumpWidget(
        MaterialApp(
          home: ProfileDialog(
            client: client,
            providers: providers,
            original: const {'id': 'p', 'kind': 'example', 'name': 'Test'},
          ),
        ),
      );
      await tester.tap(find.text('登录账号'));
      await showQuestion(tester);
      await tester.enterText(
        find.byKey(const Key('provider-auth-answer')),
        'do-not-submit',
      );
      await tester.tap(find.text('取消登录'));
      await tester.pumpAndSettle();
      expect(client.calls.last['action'], 'auth_cancel');
      expect(client.calls.last['attempt'], 'step-0');
      expect(client.calls.last.containsKey('answer'), false);
      expect(find.text('已取消登录'), findsOneWidget);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );
}
