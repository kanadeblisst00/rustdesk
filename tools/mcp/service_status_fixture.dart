import 'dart:convert';
// @STATUS_IMPORT@

const kOptionVerificationMethod = 'verification-method';
const kOptionApproveMode = 'approve-mode';
const kOptionAllowNumericOneTimePassword = 'numeric';
const kOptionStopService = 'stop-service';
const kUsePermanentPassword = 'use-permanent-password';
String translate(String key) => key;
Future<bool> mainGetBoolOption(String key) async => bind.options[key] == 'Y';
final bind = FakeBind();

class FakeBind {
  String password = '';
  final options = <String, String>{};
  Future<String> mainGetTemporaryPassword() async => password;
  Future<String> mainGetOption({required String key}) async => options[key] ?? '';
  void mainUpdateTemporaryPassword() {}
}

class TextController {
  String text = 'Generating ...';
}

class Model {
  LocalServiceStatus _localServiceStatus = LocalServiceStatus.connected;
  final _serverPasswd = TextController();
  String _verificationMethod = '';
  String _temporaryPasswordLength = '';
  String _approveMode = '';
  bool _allowNumericOneTimePassword = false;
  int notifications = 0;
  void notifyListeners() => notifications++;
  // @PASSWORD_METHOD@
}

void expect(bool condition, String message) {
  if (!condition) throw StateError(message);
}

Future<void> main() async {
  final cases = <Map<String, String>>[];
  Future<void> check(String name, Future<void> Function() body) async {
    try {
      await body();
      cases.add({'name': name, 'status': 'passed'});
    } catch (error) {
      cases.add({'name': name, 'status': 'failed', 'message': '$error'});
    }
  }

  final model = Model();
  await check('initial IPC wait does not claim password generation', () async {
    model._localServiceStatus = LocalServiceStatus.fromConnectionStatus(
        {'status_num': 0, 'ipc_status': 'connecting'});
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == 'Connecting...', 'wrong startup text');
    expect(model._localServiceStatus.messageKey == 'Connecting to local service...',
        'local and network connection states were conflated');
  });
  await check('initial IPC failure clears placeholder', () async {
    model._localServiceStatus = LocalServiceStatus.disconnected;
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == '-', 'still generating');
    expect(model._localServiceStatus.messageKey == 'Local service unavailable',
        'incorrectly reports network failure');
  });
  await check('network failure with live IPC retains valid password', () async {
    model._localServiceStatus = LocalServiceStatus.fromConnectionStatus(
        {'status_num': -1, 'ipc_status': 'connected'});
    bind.password = 'test-password-one';
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == bind.password, 'network failure hid password');
    expect(model._localServiceStatus.messageKey == null, 'overrode network status');
  });
  await check('IPC disconnect hides cached password and notifies UI', () async {
    model._localServiceStatus = LocalServiceStatus.disconnected;
    final before = model.notifications;
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == '-', 'stale password remains visible');
    expect(model.notifications > before, 'UI was not notified');
  });
  await check('reconnect displays newly synchronized password', () async {
    bind.password = 'test-password-two';
    model._localServiceStatus = LocalServiceStatus.connected;
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == bind.password, 'fresh password not displayed');
  });
  for (final option in [kOptionStopService, kOptionVerificationMethod, kOptionApproveMode]) {
    await check('disabled password path: $option', () async {
      bind.options.clear();
      bind.options[option] = option == kOptionStopService ? 'Y' :
          option == kOptionVerificationMethod ? kUsePermanentPassword : 'click';
      await model.updatePasswordModel();
      expect(model._serverPasswd.text == '-', 'disabled password became visible');
    });
  }
  await check('mobile and older bridge preserve existing behavior', () async {
    bind.options.clear();
    model._localServiceStatus = LocalServiceStatus.fromConnectionStatus({'status_num': -1});
    await model.updatePasswordModel();
    expect(model._serverPasswd.text == bind.password, 'missing IPC field changed mobile behavior');
    expect(model._localServiceStatus.messageKey == null, 'mobile got desktop diagnostic');
  });
  print(jsonEncode({
    'total': cases.length,
    'passed': cases.where((c) => c['status'] == 'passed').length,
    'failed': cases.where((c) => c['status'] == 'failed').length,
    'skipped': 0,
    'cases': cases,
  }));
  if (cases.any((c) => c['status'] == 'failed')) throw StateError('regression failed');
}
