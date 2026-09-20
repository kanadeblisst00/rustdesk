import 'dart:convert';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/models/platform_model.dart';

class AgentMcpLocalSettings extends StatefulWidget {
  const AgentMcpLocalSettings({super.key});

  @override
  State<AgentMcpLocalSettings> createState() => _AgentMcpLocalSettingsState();
}

class _AgentMcpLocalSettingsState extends State<AgentMcpLocalSettings> {
  bool _busy = false;
  String _option(String key) => bind.mainGetLocalOption(key: key);

  Future<void> _set(String key, String value) async {
    setState(() => _busy = true);
    try {
      await bind.mainSetLocalOption(key: key, value: value);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _rotate() async {
    final random = Random.secure();
    final token = List.generate(32,
            (_) => random.nextInt(256).toRadixString(16).padLeft(2, '0'))
        .join();
    await _set('agent-mcp-local-token', token);
  }

  Future<void> _enable(bool enabled) async {
    if (enabled && _option('agent-mcp-local-token').isEmpty) await _rotate();
    await _set('agent-mcp-local-enable', enabled ? 'Y' : 'N');
  }

  @override
  Widget build(BuildContext context) {
    final enabled = _option('agent-mcp-local-enable') == 'Y' &&
        _option('agent-mcp-local-stopped') != 'Y';
    final token = _option('agent-mcp-local-token');
    return Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
      const Divider(),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: Text(translate('Allow MCP local desktop capture')),
        value: enabled,
        onChanged: _busy ? null : _enable,
      ),
      Text(translate('agent-mcp-local-tip')),
      CheckboxListTile(
        contentPadding: EdgeInsets.zero,
        title: Text(translate('Allow MCP local keyboard and mouse')),
        value: _option('agent-mcp-local-input') == 'Y',
        onChanged: _busy || !enabled ? null : (value) =>
            _set('agent-mcp-local-input', value == true ? 'Y' : 'N'),
      ),
      Wrap(spacing: 12, children: [
        TextButton(
          onPressed: _busy || !enabled || token.length < 32 ||
                  _option('enable-agent-mcp') != 'Y' ||
                  _option('agent-mcp-endpoint').isEmpty
              ? null
              : () => Clipboard.setData(ClipboardData(
                  text: const JsonEncoder.withIndent('  ').convert({
                  'mcpServers': {
                    'rustdesk-local': {
                      'url': _option('agent-mcp-endpoint'),
                      'headers': {'Authorization': 'Bearer $token'}
                    }
                  }
                }))),
          child: Text(translate('Copy local MCP configuration')),
        ),
        TextButton(
          onPressed: _busy ? null : _rotate,
          child: Text(translate('Rotate local MCP token')),
        ),
        TextButton(
          onPressed: !enabled ? null : () => _set('agent-mcp-local-enable', 'N'),
          child: Text(translate('Stop local MCP control')),
        ),
      ]),
    ]);
  }
}
