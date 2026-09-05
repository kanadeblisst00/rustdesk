import 'dart:async';
import 'dart:convert';

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
  Timer? _timer;
  bool _busy = false;

  String _option(String key) => bind.mainGetLocalOption(key: key);

  @override
  void initState() {
    super.initState();
    _devices = TextEditingController(text: _option('agent-mcp-devices'));
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    _devices.dispose();
    super.dispose();
  }

  Future<void> _save(String key, String value) async {
    setState(() => _busy = true);
    try {
      await bind.mainSetLocalOption(key: key, value: value);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

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
                onPressed: _busy
                    ? null
                    : () => _save('agent-mcp-devices', _devices.text.trim()),
                child: Text(translate('Save')),
              ),
              TextButton(
                onPressed: !enabled || token.length < 32
                    ? null
                    : () => Clipboard.setData(ClipboardData(
                            text: const JsonEncoder.withIndent('  ').convert({
                          'mcpServers': {
                            'rustdesk': {
                              'url': 'http://127.0.0.1:59940/mcp',
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
