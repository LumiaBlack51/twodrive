import 'package:flutter/material.dart';
import 'client.dart';

/// One transient provider-defined question. No provider-specific protocol lives here.
Future<String?> askProviderAuth(BuildContext context, Json question) async {
  return showDialog<String>(
    context: context,
    barrierDismissible: false,
    builder: (context) => _AuthQuestion(question: question),
  );
}

class _AuthQuestion extends StatefulWidget {
  const _AuthQuestion({required this.question});
  final Json question;
  @override
  State<_AuthQuestion> createState() => _AuthQuestionState();
}

class _AuthQuestionState extends State<_AuthQuestion> {
  final answer = TextEditingController();
  final form = GlobalKey<FormState>();
  @override
  void dispose() {
    answer.clear();
    answer.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final question = widget.question;
    return AlertDialog(
      title: Text(question['title'] as String? ?? '账号登录'),
      content: SizedBox(
        width: 400,
        child: Form(
          key: form,
          child: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (question['help'] is String) ...[
                  Text(question['help'] as String),
                  const SizedBox(height: 16),
                ],
                if ((question['message'] as String? ?? '').isNotEmpty) ...[
                  Text(
                    question['message'] as String,
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                  const SizedBox(height: 12),
                ],
                if (question['display_code'] is String) ...[
                  SelectableText(
                    question['display_code'] as String,
                    style: const TextStyle(
                      fontSize: 24,
                      fontWeight: FontWeight.bold,
                      letterSpacing: 2,
                    ),
                  ),
                  const SizedBox(height: 12),
                ],
                if (question['verification_url'] is String) ...[
                  SelectableText(question['verification_url'] as String),
                  const SizedBox(height: 12),
                ],
                if (question['confirmation'] != true)
                  TextFormField(
                    key: const Key('provider-auth-answer'),
                    controller: answer,
                    autofocus: true,
                    obscureText: question['secret'] == true,
                    autocorrect: false,
                    enableSuggestions: false,
                    maxLength: 4096,
                    decoration: InputDecoration(
                      labelText: question['label'] as String? ?? '输入信息',
                      counterText: '',
                    ),
                    validator: (value) =>
                        question['required'] == true &&
                            (value == null || value.isEmpty)
                        ? '请填写此项'
                        : null,
                    onFieldSubmitted: (_) {
                      if (form.currentState!.validate()) {
                        Navigator.pop(context, answer.text);
                      }
                    },
                  ),
                for (final alternative
                    in ((question['alternatives'] as List?) ?? []).cast<Json>())
                  TextButton(
                    onPressed: () =>
                        Navigator.pop(context, alternative['value'] as String),
                    child: Text(alternative['label'] as String),
                  ),
              ],
            ),
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('取消登录'),
        ),
        FilledButton(
          onPressed: () {
            if (form.currentState!.validate()) {
              Navigator.pop(context, answer.text);
            }
          },
          child: Text(question['submit_label'] as String? ?? '下一步'),
        ),
      ],
    );
  }
}
