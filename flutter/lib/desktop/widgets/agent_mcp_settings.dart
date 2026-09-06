import 'dart:async';
import 'dart:convert';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/models/platform_model.dart';

class AgentMcpSettings extends StatefulWidget {
  const AgentMcpSettings({super.key});

  @override
  State<AgentMcpSettings> createState() => _AgentMcpSettingsState();
}

class _AgentMcpSettingsState extends State<AgentMcpSettings> {
  late final TextEditingController _devices;
  late final TextEditingController _address;
  late final TextEditingController _token;
  final _form = GlobalKey<FormState>();
  bool _tokenEdited = false;
  bool _showToken = false;
  Timer? _timer;
  bool _busy = false;

  String _option(String key) => bind.mainGetLocalOption(key: key);

  @override
  void initState() {
    super.initState();
    _devices = TextEditingController(text: _option('agent-mcp-devices'));
    final address = _option('agent-mcp-bind-address');
    _address = TextEditingController(
        text: address.isEmpty ? _option('agent-mcp-default-address') : address);
    _token = TextEditingController(text: _option('agent-mcp-token'));
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) {
        final token = _option('agent-mcp-token');
        if (!_tokenEdited && _token.text != token) _token.text = token;
        setState(() {});
      }
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    _devices.dispose();
    _address.dispose();
    _token.dispose();
    super.dispose();
  }

  String? _validateAddress(String? value) {
    final address = (value ?? '').trim();
    if (address.isEmpty) return null;
    final parts = address.split(':');
    final octets = parts.first.split('.');
    if (parts.length > 2 ||
        octets.length != 4 ||
        octets.any((s) =>
            !RegExp(r'^(0|[1-9][0-9]{0,2})$').hasMatch(s) ||
            (int.tryParse(s) ?? 256) > 255)) {
      return translate('Invalid IP');
    }
    final numbers = octets.map(int.parse).toList();
    if ((numbers.first >= 224 && numbers.first <= 239) ||
        numbers.every((n) => n == 255) ||
        (numbers.first == 0 && numbers.any((n) => n != 0))) {
      return translate('Invalid IP');
    }
    if (parts.length == 2) {
      final port = int.tryParse(parts[1]) ?? 0;
      if (!RegExp(r'^[0-9]+$').hasMatch(parts[1]) || port < 1 || port > 65535) {
        return '${translate('Port')}: 1–65535';
      }
    }
    return null;
  }

  bool _validToken(String token) =>
      token.length >= 32 &&
      token.length <= 256 &&
      token.codeUnits.every((c) => c >= 33 && c <= 126);

  String _newToken() {
    final random = Random.secure();
    return List.generate(
            32, (_) => random.nextInt(256).toRadixString(16).padLeft(2, '0'))
        .join();
  }

  Future<void> _saveSettings() async {
    if (!(_form.currentState?.validate() ?? false)) return;
    setState(() => _busy = true);
    try {
      final token = _token.text.isEmpty ? _newToken() : _token.text;
      final devices = _devices.text.trim();
      final address = _address.text.trim();
      await bind.mainSetLocalOption(key: 'agent-mcp-token', value: token);
      await bind.mainSetLocalOption(key: 'agent-mcp-devices', value: devices);
      await bind.mainSetLocalOption(
          key: 'agent-mcp-bind-address', value: address);
      if (mounted) {
        _token.text = token;
        _tokenEdited = false;
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _save(String key, String value) async {
    setState(() => _busy = true);
    try {
      await bind.mainSetLocalOption(key: key, value: value);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Widget _networkSettings() => Form(
        key: _form,
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          const SizedBox(height: 8),
          TextFormField(
            controller: _address,
            enabled: !_busy,
            decoration: InputDecoration(
              labelText: translate('MCP listen address'),
              hintText: _option('agent-mcp-default-address'),
            ),
            validator: _validateAddress,
            onFieldSubmitted: (_) => _saveSettings(),
          ),
          const SizedBox(height: 8),
          Text(translate('agent-mcp-network-tip')),
          TextFormField(
            controller: _token,
            enabled: !_busy,
            obscureText: !_showToken,
            keyboardType: TextInputType.visiblePassword,
            enableSuggestions: false,
            autocorrect: false,
            smartDashesType: SmartDashesType.disabled,
            smartQuotesType: SmartQuotesType.disabled,
            enableIMEPersonalizedLearning: false,
            decoration: InputDecoration(
              labelText: translate('MCP token'),
              suffixIcon: IconButton(
                onPressed: () => setState(() => _showToken = !_showToken),
                icon:
                    Icon(_showToken ? Icons.visibility_off : Icons.visibility),
              ),
            ),
            validator: (value) =>
                value == null || value.isEmpty || _validToken(value)
                    ? null
                    : translate('agent-mcp-token-tip'),
            onChanged: (_) => _tokenEdited = true,
            onFieldSubmitted: (_) => _saveSettings(),
          ),
          const SizedBox(height: 8),
          Text(translate('agent-mcp-token-tip')),
          TextButton(
            onPressed: _busy
                ? null
                : () => setState(() {
                      _token.text = _newToken();
                      _tokenEdited = true;
                    }),
            child: Text(translate('Generate MCP token')),
          ),
        ]),
      );

  @override
  Widget build(BuildContext context) {
    final enabled = _option('enable-agent-mcp') == 'Y';
    final token = _option('agent-mcp-token');
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 15, vertical: 8),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child:
              Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(translate('Enable MCP server')),
              value: enabled,
              onChanged: _busy
                  ? null
                  : (value) => _save('enable-agent-mcp', value ? 'Y' : 'N'),
            ),
            Text(translate('agent-mcp-tip')),
            const SizedBox(height: 8),
            SelectableText(_option('agent-mcp-status')),
            _networkSettings(),
            CheckboxListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(translate('Read-only')),
              value: _option('agent-mcp-read-only') == 'Y',
              onChanged: _busy
                  ? null
                  : (value) =>
                      _save('agent-mcp-read-only', value == true ? 'Y' : 'N'),
            ),
            TextField(
              controller: _devices,
              enabled: !_busy,
              decoration:
                  InputDecoration(labelText: translate('MCP device allowlist')),
              onSubmitted: (value) => _save('agent-mcp-devices', value.trim()),
            ),
            const SizedBox(height: 8),
            Wrap(spacing: 12, children: [
              TextButton(
                onPressed: _busy ? null : _saveSettings,
                child: Text(translate('Save')),
              ),
              TextButton(
                onPressed: _busy ||
                        !enabled ||
                        !_validToken(token) ||
                        _option('agent-mcp-endpoint').isEmpty
                    ? null
                    : () => Clipboard.setData(ClipboardData(
                            text: const JsonEncoder.withIndent('  ').convert({
                          'mcpServers': {
                            'rustdesk': {
                              'url': _option('agent-mcp-endpoint'),
                              'headers': {'Authorization': 'Bearer $token'}
                            }
                          }
                        }))),
                child: Text(translate('Copy MCP configuration')),
              ),
            ]),
          ]),
        ),
      ),
    );
  }
}
