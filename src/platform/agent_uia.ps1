$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try {
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class RustDeskUiaDesktop {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern bool GetUserObjectInformation(IntPtr obj, int index, StringBuilder data, int length, out int needed);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
}
'@
    if ([System.Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) {
        throw 'UIA is unavailable in service session 0; use the interactive RustDesk server'
    }
    $desktop = [RustDeskUiaDesktop]::OpenInputDesktop(0, $false, 1)
    if ($desktop -eq [IntPtr]::Zero) { throw 'Input desktop is unavailable' }
    try {
        $name = New-Object System.Text.StringBuilder 256
        $needed = 0
        if (-not [RustDeskUiaDesktop]::GetUserObjectInformation($desktop, 2, $name, 512, [ref]$needed) -or $name.ToString() -ne 'Default') {
            throw 'UIA is unavailable on a locked or secure desktop'
        }
    } finally { [void][RustDeskUiaDesktop]::CloseDesktop($desktop) }
    [void][RustDeskUiaDesktop]::SetThreadDpiAwarenessContext([IntPtr](-4))
    if ($request.operation -eq 'capabilities') {
        [Console]::Write('{"available":true,"provider":"Windows UI Automation","scope":"foreground_window"}')
        exit 0
    }
    $window = [RustDeskUiaDesktop]::GetForegroundWindow()
    if ($window -eq [IntPtr]::Zero) { throw 'No foreground window' }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $queue = New-Object System.Collections.Queue
    $queue.Enqueue(@($root, $null, 0))
    $elements = New-Object System.Collections.ArrayList
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $truncated = $false
    $unavailable = 0
    $target = $null
    while ($queue.Count -gt 0) {
        if ($elements.Count -ge 512 -or $timer.ElapsedMilliseconds -ge 4000) { $truncated = $true; break }
        $item = $queue.Dequeue()
        $element, $parent, $depth = $item
        try {
            $current = $element.Current
            $rid = [string]::Join(',', [int[]]$element.GetRuntimeId())
            $id = [string]$current.ProcessId + ':' + $rid
            $bounds = $current.BoundingRectangle
            $patterns = @($element.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName.Replace('PatternIdentifiers.Pattern', '') })
            $value = $null
            $toggle = $null
            if (-not $current.IsPassword -and $patterns -contains 'Value') {
                $value = ([System.Windows.Automation.ValuePattern]$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)).Current.Value
                if ($value.Length -gt 1024) { $value = $value.Substring(0, 1024) }
            }
            if ($patterns -contains 'Toggle') {
                $toggle = ([System.Windows.Automation.TogglePattern]$element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)).Current.ToggleState.ToString()
            }
            $label = [string]$current.Name
            if ($current.IsPassword) { $label = '' }
            if ($label.Length -gt 4096) { $label = $label.Substring(0, 4096) }
            $node = [ordered]@{
                source='uia'; element_id=$id; parent_id=$parent; depth=$depth
                name=$label; automation_id=[string]$current.AutomationId
                control_type=$current.ControlType.ProgrammaticName.Replace('ControlType.', '')
                enabled=$current.IsEnabled; offscreen=$current.IsOffscreen; password=$current.IsPassword
                focused=$current.HasKeyboardFocus; patterns=$patterns; value=$value; toggle_state=$toggle
                bounds=@{x=$bounds.X; y=$bounds.Y; width=$bounds.Width; height=$bounds.Height}
            }
            if (-not $bounds.IsEmpty -and -not $current.IsOffscreen) { [void]$elements.Add($node) }
            if ($request.operation -ne 'tree' -and $request.element.element_id -ceq $id) {
                foreach ($key in @('name', 'automation_id', 'control_type')) {
                    if ($request.element.$key -cne $node[$key]) { throw 'STALE_TARGET: UIA identity changed; find the control again' }
                }
                $target = $element
                break
            }
            if ($depth -lt 12) {
                $child = $walker.GetFirstChild($element)
                while ($null -ne $child) {
                    if ($queue.Count + $elements.Count -ge 512 -or $timer.ElapsedMilliseconds -ge 4000) { $truncated = $true; break }
                    $queue.Enqueue(@($child, $id, ($depth + 1)))
                    $child = $walker.GetNextSibling($child)
                }
            } else { $truncated = $true }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            $unavailable++
        }
    }
    if ($request.operation -eq 'tree') {
        $result = @{available=$true; provider='Windows UI Automation'; scope='foreground_window'; elements=@($elements.ToArray()); truncated=$truncated; unavailable_nodes=$unavailable; active_window=@{title=$root.Current.Name; handle=$window.ToInt64()}}
    } else {
        if ($null -eq $target) { throw 'STALE_TARGET: UIA element no longer exists in the foreground window' }
        $current = $target.Current
        if (-not $current.IsEnabled -or $current.IsOffscreen -or $current.IsPassword) { throw 'UIA target is disabled, offscreen, or a password control' }
        if ([RustDeskUiaDesktop]::GetForegroundWindow() -ne $window) { throw 'STALE_TARGET: foreground window changed' }
        switch ($request.operation) {
            'invoke' { ([System.Windows.Automation.InvokePattern]$target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)).Invoke() }
            'toggle' { ([System.Windows.Automation.TogglePattern]$target.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)).Toggle() }
            'set_value' {
                $pattern = [System.Windows.Automation.ValuePattern]$target.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
                if ($pattern.Current.IsReadOnly) { throw 'UIA value is read-only' }
                $pattern.SetValue([string]$request.value)
            }
            default { throw 'Unknown UIA operation' }
        }
        $result = @{acknowledged=$true; operation=$request.operation}
    }
    [Console]::Write(($result | ConvertTo-Json -Depth 20 -Compress))
} catch {
    [Console]::Write((@{error=$_.Exception.Message} | ConvertTo-Json -Compress))
}
