import 'dart:async';

import 'package:flutter_hbb/models/first_image_watchdog.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late FirstImageWatchdog watchdog;
  late int refreshes;
  late int timeouts;

  void start() => watchdog.start(
        onRefresh: () => refreshes++,
        onTimeout: () => timeouts++,
      );

  setUp(() {
    watchdog = FirstImageWatchdog();
    refreshes = 0;
    timeouts = 0;
  });

  tearDown(() => watchdog.cancel());

  testWidgets(
      'refreshes once at 10s and times out at the original 30s deadline',
      (tester) async {
    start();
    await tester.pump(const Duration(seconds: 9));
    expect(refreshes, 0);
    expect(timeouts, 0);
    await tester.pump(const Duration(seconds: 1));
    expect(refreshes, 1);
    await tester.pump(const Duration(seconds: 19));
    expect(timeouts, 0);
    await tester.pump(const Duration(seconds: 1));
    expect(timeouts, 1);
    expect(watchdog.timedOut, isTrue);
    await tester.pump(const Duration(minutes: 5));
    expect(refreshes, 1);
    expect(timeouts, 1);
  });

  testWidgets('duplicate waiting events do not extend the deadline',
      (tester) async {
    start();
    for (var i = 0; i < 30; i++) {
      await tester.pump(const Duration(seconds: 1));
      start();
    }
    expect(refreshes, 1);
    expect(timeouts, 1);
    expect(watchdog.timedOut, isTrue);
    start();
    await tester.pump(const Duration(minutes: 1));
    expect(refreshes, 1);
    expect(timeouts, 1);
  });

  testWidgets('a stalled asynchronous refresh cannot postpone timeout',
      (tester) async {
    final refresh = Completer<void>();
    watchdog.start(
      onRefresh: () async {
        refreshes++;
        await refresh.future;
      },
      onTimeout: () => timeouts++,
    );
    await tester.pump(const Duration(seconds: 30));
    expect(refreshes, 1);
    expect(timeouts, 1);
    refresh.complete();
    await tester.pump();
    expect(timeouts, 1);
  });

  testWidgets('a UI image before recovery cancels all pending work',
      (tester) async {
    start();
    await tester.pump(const Duration(seconds: 5));
    watchdog.cancel();
    await tester.pump(const Duration(minutes: 5));
    expect(refreshes, 0);
    expect(timeouts, 0);
    expect(watchdog.timedOut, isFalse);
  });

  testWidgets('an image after refresh prevents timeout on a static desktop',
      (tester) async {
    start();
    await tester.pump(const Duration(seconds: 15));
    expect(refreshes, 1);
    watchdog.cancel();
    await tester.pump(const Duration(minutes: 5));
    expect(refreshes, 1);
    expect(timeouts, 0);
  });

  testWidgets('reconnect gets a fresh deadline and cancels the old one',
      (tester) async {
    start();
    await tester.pump(const Duration(seconds: 25));
    watchdog.cancel();
    start();
    await tester.pump(const Duration(seconds: 5));
    expect(timeouts, 0);
    await tester.pump(const Duration(seconds: 5));
    expect(refreshes, 2);
    await tester.pump(const Duration(seconds: 20));
    expect(timeouts, 1);
  });

  testWidgets('late image or close clears timeout state for mobile reuse',
      (tester) async {
    start();
    await tester.pump(const Duration(seconds: 30));
    expect(watchdog.timedOut, isTrue);
    watchdog.cancel();
    watchdog.cancel();
    expect(watchdog.timedOut, isFalse);
    start();
    await tester.pump(const Duration(seconds: 30));
    expect(refreshes, 2);
    expect(timeouts, 2);
  });

  test('callbacks from an old round cannot affect a new round', () {
    final callbacks = <void Function()>[];
    runZoned(() {
      start();
      watchdog.cancel();
      start();
      callbacks[0]();
      callbacks[1]();
      expect(refreshes, 0);
      expect(timeouts, 0);
      expect(watchdog.timedOut, isFalse);
      callbacks[2]();
      callbacks[3]();
      expect(refreshes, 1);
      expect(timeouts, 1);
    }, zoneSpecification: ZoneSpecification(
      createTimer: (self, parent, zone, duration, callback) {
        callbacks.add(callback);
        return _QueuedTimer();
      },
    ));
  });
}

// Simulate a callback already queued when a connection is cancelled.
class _QueuedTimer implements Timer {
  @override
  void cancel() {}

  @override
  bool get isActive => false;

  @override
  int get tick => 0;
}
