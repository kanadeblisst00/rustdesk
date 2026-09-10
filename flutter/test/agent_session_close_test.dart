import 'package:flutter/material.dart';
import 'package:flutter_hbb/desktop/widgets/tabbar_widget.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:get/get.dart';
import 'package:uuid/uuid.dart';

final _sessionId = UuidValue('00000000-0000-4000-8000-000000000001');

class _FakeFFI implements FFI {
  @override
  UuidValue get sessionId => _sessionId;

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _TestTabController extends DesktopTabController {
  _TestTabController(DesktopTabType type) : super(tabType: type);

  // Keep the real closeBy/remove path without native window selection calls.
  @override
  bool jumpTo(int index, {bool callOnSelected = true}) {
    state.value.selected = index;
    return true;
  }
}

void main() {
  late _FakeFFI ffi;
  late FfiModel model;

  setUp(() {
    ffi = _FakeFFI();
    model = FfiModel(WeakReference(ffi));
  });

  tearDown(() {
    model.clear();
    Get.reset();
  });

  for (final type in [
    DesktopTabType.fileTransfer,
    DesktopTabType.remoteScreen
  ]) {
    test('$type closes the requested background tab and the final tab',
        () async {
      final controller = _TestTabController(type);
      Get.put<DesktopTabController>(controller);
      final removed = <String>[];
      var emptyWindow = false;
      controller.onRemoved = (_, id) {
        removed.add(id);
        emptyWindow = controller.state.value.tabs.isEmpty;
      };
      controller.state.value.tabs.addAll([
        TabInfo(key: 'target-peer', label: '', page: const SizedBox()),
        TabInfo(key: 'other-peer', label: '', page: const SizedBox()),
      ]);
      controller.state.value.selected = 1;

      final targetListener =
          model.startEventListener(_sessionId, 'target-peer');
      final event = {
        'name': 'agent_close_session',
        'session': _sessionId.toString()
      };
      await targetListener(event);
      expect(removed, ['target-peer']);
      expect(controller.state.value.tabs.single.key, 'other-peer');
      expect(emptyWindow, isFalse);

      await targetListener(event);
      expect(removed, ['target-peer']);
      expect(controller.state.value.tabs.single.key, 'other-peer');

      await model.startEventListener(_sessionId, 'other-peer')(event);
      expect(removed, ['target-peer', 'other-peer']);
      expect(emptyWindow, isTrue);
    });
  }

  test('an event for another session does not close a same-peer desktop tab',
      () async {
    final controller = _TestTabController(DesktopTabType.remoteScreen);
    Get.put<DesktopTabController>(controller);
    controller.onRemoved = (_, __) => fail('Unexpected tab removal');
    controller.state.value.tabs.add(
      TabInfo(key: 'target-peer', label: '', page: const SizedBox()),
    );

    await model.startEventListener(_sessionId, 'target-peer')({
      'name': 'agent_close_session',
      'session': '00000000-0000-4000-8000-000000000002',
    });
    expect(controller.state.value.tabs.single.key, 'target-peer');
  });

  test('terminal session closure uses the existing whole-window callback',
      () async {
    final controller = _TestTabController(DesktopTabType.terminal);
    Get.put<DesktopTabController>(controller);
    var closed = false;
    controller.onCloseWindow = () async => closed = true;

    await model.startEventListener(_sessionId, 'target-peer')({
      'name': 'agent_close_session',
      'session': _sessionId.toString(),
    });
    expect(closed, isTrue);
  });
}
