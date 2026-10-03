import 'dart:async';

/// Bounds one wait for a UI image, independently of network traffic.
class FirstImageWatchdog {
  Timer? _refreshTimer;
  Timer? _timeoutTimer;
  int _generation = 0;
  bool _started = false;
  bool _timedOut = false;

  bool get timedOut => _timedOut;

  void start({
    required void Function() onRefresh,
    required void Function() onTimeout,
  }) {
    // Repeated waiting events must not postpone the original deadline.
    if (_started) return;
    _started = true;
    final generation = ++_generation;
    _refreshTimer = Timer(const Duration(seconds: 10), () {
      if (generation == _generation) onRefresh();
    });
    _timeoutTimer = Timer(const Duration(seconds: 30), () {
      if (generation != _generation) return;
      _timedOut = true;
      onTimeout();
    });
  }

  void cancel() {
    ++_generation;
    _refreshTimer?.cancel();
    _timeoutTimer?.cancel();
    _refreshTimer = null;
    _timeoutTimer = null;
    _started = false;
    _timedOut = false;
  }
}
