enum LocalServiceStatus {
  connecting,
  connected,
  disconnected;

  static LocalServiceStatus fromConnectionStatus(Map<String, dynamic> status) {
    switch (status['ipc_status']) {
      case 'connecting':
        return connecting;
      case 'disconnected':
        return disconnected;
      default:
        // Mobile and older native libraries do not report desktop IPC state.
        return connected;
    }
  }

  String? get messageKey {
    switch (this) {
      case connecting:
        return 'Connecting to local service...';
      case disconnected:
        return 'Local service unavailable';
      case connected:
        return null;
    }
  }

  String? get passwordPlaceholder {
    switch (this) {
      case connecting:
        return 'Connecting...';
      case disconnected:
        return '-';
      case connected:
        return null;
    }
  }
}
