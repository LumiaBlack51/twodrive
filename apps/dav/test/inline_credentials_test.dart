import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:twodrive_dav/client.dart';
import 'package:twodrive_dav/profile_dialog.dart';

class InlineClient extends StorageClient {
  String? failure;
  Json? received;
  @override
  Future<Json> request(
    String op, [
    Json args = const {},
    int? requestId,
  ]) async {
    expect(op, 'provider_action');
    received = {...args};
    if (failure != null) throw StateError(failure!);
    return {
      'authenticated': true,
      'options': {'account_state': '/private/example', 'writable': false},
    };
  }
}

const inlineProviders = <Json>[
  {
    'id': 'example',
    'label': '示例网盘',
    'network_modes': ['system'],
    'fields': [
      {
        'key': 'username',
        'label': '账号',
        'transient': true,
        'omit_from_profile': true,
      },
      {
        'key': 'password',
        'label': '密码',
        'secret': true,
        'transient': true,
        'omit_from_profile': true,
      },
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
        'credential_fields': ['username', 'password'],
      },
    ],
  },
];

void main() {
  testWidgets(
    'inline credentials precede login, layout has a gap, save authenticates without persisting answers',
    (tester) async {
      tester.view.physicalSize = const Size(1100, 1100);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final client = InlineClient();
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
                      providers: inlineProviders,
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
      final account = find.byKey(const ValueKey('profile-username'));
      final password = find.byKey(const ValueKey('profile-password'));
      final login = find.widgetWithText(OutlinedButton, '登录账号');
      final route = find.byKey(const ValueKey('route-example'));
      expect(account, findsOneWidget);
      expect(password, findsOneWidget);
      expect(
        tester.getBottomLeft(password).dy,
        lessThan(tester.getTopLeft(login).dy),
      );
      expect(
        tester.getTopLeft(route).dy - tester.getBottomLeft(login).dy,
        greaterThanOrEqualTo(16),
      );
      final passwordInput = tester.widget<TextField>(
        find.descendant(of: password, matching: find.byType(TextField)),
      );
      expect(passwordInput.obscureText, true);
      await tester.enterText(account, 'user@example.invalid');
      await tester.enterText(password, 'synthetic-inline-secret');
      await tester.tap(find.text('保存'));
      await tester.pumpAndSettle();
      expect(client.received?['username'], 'user@example.invalid');
      expect(client.received?['password'], 'synthetic-inline-secret');
      expect(client.received?['network_route'], 'system');
      expect(saved?['_connect'], true);
      expect(saved?['account_state'], '/private/example');
      expect(jsonEncode(saved), isNot(contains('synthetic-inline-secret')));
      expect(saved?.containsKey('username'), false);
      expect(saved?.containsKey('password'), false);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );

  testWidgets(
    'failed login shows actionable message and clears only the password',
    (tester) async {
      tester.view.physicalSize = const Size(1100, 1100);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final client = InlineClient()..failure = '请检查网络路由（AUTH_NETWORK）';
      await tester.pumpWidget(
        MaterialApp(
          home: ProfileDialog(client: client, providers: inlineProviders),
        ),
      );
      await tester.enterText(
        find.byKey(const ValueKey('profile-username')),
        'user@example.invalid',
      );
      await tester.enterText(
        find.byKey(const ValueKey('profile-password')),
        'synthetic-test-password',
      );
      await tester.tap(find.text('登录账号'));
      await tester.pumpAndSettle();
      expect(find.text('请检查网络路由（AUTH_NETWORK）'), findsOneWidget);
      expect(find.textContaining('Bad state:'), findsNothing);
      final inputs = tester.widgetList<TextField>(find.byType(TextField));
      expect(
        inputs.any((f) => f.controller?.text == 'user@example.invalid'),
        true,
      );
      expect(
        inputs.where((f) => f.obscureText).single.controller?.text,
        isEmpty,
      );
      expect(client.connected, false);
      await tester.pumpWidget(const SizedBox());
      client.dispose();
    },
  );
}
