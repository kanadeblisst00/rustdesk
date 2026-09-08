$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try {
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class RustDeskUiaDesktop {
    public delegate bool EnumProc(IntPtr window, IntPtr state);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", SetLastError=true)] public static extern bool EnumWindows(EnumProc callback, IntPtr state);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    public static IntPtr[] Windows() { return Enumerate(false); }
    public static IntPtr[] Taskbars() { return Enumerate(true); }
    private static IntPtr[] Enumerate(bool taskbars) {
        var windows = new System.Collections.Generic.List<IntPtr>();
        int limit = taskbars ? 64 : 129;
        bool completed = EnumWindows((window, state) => {
            if (!IsWindowVisible(window)) return true;
            if (taskbars) {
                var name = new StringBuilder(256);
                GetClassName(window, name, name.Capacity);
                if (name.ToString() != "Shell_TrayWnd" && name.ToString() != "Shell_SecondaryTrayWnd") return true;
            }
            windows.Add(window);
            return windows.Count < limit;
        }, IntPtr.Zero);
        if (!completed && windows.Count < limit) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        return windows.ToArray();
    }
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
    function Get-WindowInfo([IntPtr]$handle) {
        if ($handle -eq [IntPtr]::Zero) { return $null }
        $title = New-Object System.Text.StringBuilder 1024
        $className = New-Object System.Text.StringBuilder 256
        [void][RustDeskUiaDesktop]::GetWindowText($handle, $title, $title.Capacity)
        [void][RustDeskUiaDesktop]::GetClassName($handle, $className, $className.Capacity)
        [uint32]$processId = 0
        [void][RustDeskUiaDesktop]::GetWindowThreadProcessId($handle, [ref]$processId)
        $processName = $null; $started = $null
        try {
            $process = [System.Diagnostics.Process]::GetProcessById($processId)
            try {
                $processName = $process.ProcessName
                $started = $process.StartTime.ToUniversalTime().Ticks.ToString()
            } finally { $process.Dispose() }
        } catch { $started = $null }
        $rect = New-Object RustDeskUiaDesktop+Rect
        $hasBounds = [RustDeskUiaDesktop]::GetWindowRect($handle, [ref]$rect)
        return @{
            handle=$handle.ToInt64().ToString(); process_id=$processId.ToString()
            process_name=$processName; process_started=$started
            title=$title.ToString(); class_name=$className.ToString()
            minimized=[RustDeskUiaDesktop]::IsIconic($handle)
            foreground=($handle -eq [RustDeskUiaDesktop]::GetForegroundWindow())
            bounds=$(if ($hasBounds) { @{x=$rect.Left; y=$rect.Top; width=($rect.Right-$rect.Left); height=($rect.Bottom-$rect.Top)} } else { $null })
        }
    }
    if ($request.operation -in @('windows', 'foreground', 'focus_window')) {
        if ($request.operation -eq 'windows') {
            $handles = @([RustDeskUiaDesktop]::Windows())
            $windows = @($handles | Select-Object -First 128 | ForEach-Object { Get-WindowInfo $_ })
            $result = @{windows=$windows; truncated=($handles.Count -gt 128)}
        } elseif ($request.operation -eq 'foreground') {
            $result = @{active_window=(Get-WindowInfo ([RustDeskUiaDesktop]::GetForegroundWindow()))}
        } else {
            $handle = [IntPtr]([long]$request.element.handle)
            $current = Get-WindowInfo $handle
            if (-not [RustDeskUiaDesktop]::IsWindowVisible($handle)) { throw 'STALE_TARGET: window is no longer visible' }
            foreach ($key in @('handle', 'process_id', 'process_started', 'class_name', 'title')) {
                if ($null -eq $request.element.$key -or $request.element.$key -cne $current[$key]) {
                    throw 'STALE_TARGET: window identity changed; list_windows again'
                }
            }
            if ([string]::IsNullOrEmpty($current.process_started)) { throw 'Window process identity is unavailable' }
            if ([RustDeskUiaDesktop]::IsIconic($handle)) { [void][RustDeskUiaDesktop]::ShowWindowAsync($handle, 9) }
            [void][RustDeskUiaDesktop]::SetForegroundWindow($handle)
            $focusTimer = [System.Diagnostics.Stopwatch]::StartNew()
            while ([RustDeskUiaDesktop]::GetForegroundWindow() -ne $handle -and $focusTimer.ElapsedMilliseconds -lt 1000) {
                Start-Sleep -Milliseconds 20
            }
            $result = @{focused=([RustDeskUiaDesktop]::GetForegroundWindow() -eq $handle -and -not [RustDeskUiaDesktop]::IsIconic($handle)); active_window=(Get-WindowInfo ([RustDeskUiaDesktop]::GetForegroundWindow()))}
        }
        [Console]::Write(($result | ConvertTo-Json -Depth 8 -Compress))
        exit 0
    }
    if ($request.operation -eq 'capabilities') {
        [Console]::Write('{"available":true,"provider":"Windows UI Automation","scope":"foreground_window","scopes":["foreground_window","taskbar"],"window_operations":true}')
        exit 0
    }
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $window = [RustDeskUiaDesktop]::GetForegroundWindow()
    if ($window -eq [IntPtr]::Zero) { throw 'No foreground window' }
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $queue = New-Object System.Collections.Queue
    $scope = 'foreground_window'
    if ($request.operation -eq 'taskbar_tree' -or $request.element.scope -eq 'taskbar') {
        $scope = 'taskbar'
        foreach ($handle in [RustDeskUiaDesktop]::Taskbars()) {
            $queue.Enqueue(@([System.Windows.Automation.AutomationElement]::FromHandle($handle), $null, 0))
        }
    } else {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $queue.Enqueue(@($root, $null, 0))
    }
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
                source='uia'; scope=$scope; element_id=$id; parent_id=$parent; depth=$depth
                name=$label; automation_id=[string]$current.AutomationId
                control_type=$current.ControlType.ProgrammaticName.Replace('ControlType.', '')
                enabled=$current.IsEnabled; offscreen=$current.IsOffscreen; password=$current.IsPassword
                focused=$current.HasKeyboardFocus; patterns=$patterns; value=$value; toggle_state=$toggle
                bounds=@{x=$bounds.X; y=$bounds.Y; width=$bounds.Width; height=$bounds.Height}
            }
            if (-not $bounds.IsEmpty -and -not $current.IsOffscreen) { [void]$elements.Add($node) }
            if ($request.operation -notin @('tree', 'taskbar_tree') -and $request.element.element_id -ceq $id) {
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
    if ($request.operation -in @('tree', 'taskbar_tree')) {
        $activeTitle = if ($scope -eq 'taskbar') { (Get-WindowInfo $window).title } else { $root.Current.Name }
        $result = @{available=$true; provider='Windows UI Automation'; scope=$scope; elements=@($elements.ToArray()); truncated=$truncated; unavailable_nodes=$unavailable; active_window=@{title=$activeTitle; handle=$window.ToInt64()}}
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
