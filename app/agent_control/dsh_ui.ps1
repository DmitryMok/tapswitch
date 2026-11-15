# Reads or changes the session list of the DeepSeek Harness window through UI Automation.
# Input comes from environment variables to avoid quoting problems:
#   AGENT_CONTROL_ACTION - "read": print JSON with the rows of the session list in screen order
#                          "select": select the session row AGENT_CONTROL_INDEX
#                          "click": click a collapsed workspace row or the "Show N more sessions"
#                                   button AGENT_CONTROL_INDEX with the mouse
#   AGENT_CONTROL_WINDOW - window handle
#   AGENT_CONTROL_NAME   - for select/click: the row must still have this name
# Only this window is queried: scanning the desktop would also query the calling
# application's window, which is blocked waiting for this script, and hang both.
# Chromium exposes the page only while the window is visible: covered by other windows
# it shows an empty document, so the caller brings the window forward first.
# Exit code 3: window or row not found. Exit code 4: the row cannot be clicked
# (scrolled out of view or covered by another window).
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

$element = [Windows.Automation.AutomationElement]
$descendants = [Windows.Automation.TreeScope]::Descendants
$controlType = [Windows.Automation.ControlType]
$treeCondition = New-Object Windows.Automation.AndCondition(
    (New-Object Windows.Automation.PropertyCondition($element::ControlTypeProperty, $controlType::Tree)),
    (New-Object Windows.Automation.PropertyCondition($element::NameProperty, 'Sessions'))
)
$rowCondition = New-Object Windows.Automation.OrCondition(
    (New-Object Windows.Automation.PropertyCondition($element::ControlTypeProperty, $controlType::TreeItem)),
    (New-Object Windows.Automation.PropertyCondition($element::ControlTypeProperty, $controlType::Button))
)

try {
    $window = $element::FromHandle([IntPtr][long]$env:AGENT_CONTROL_WINDOW)
} catch {
    exit 3
}

# Chromium builds the accessibility tree only after the first request: wait for it.
$tree = $null
for ($attempt = 0; $attempt -lt 15 -and $tree -eq $null; $attempt++) {
    $tree = $window.FindFirst($descendants, $treeCondition)
    if ($tree -eq $null) { Start-Sleep -Milliseconds 200 }
}
if ($tree -eq $null) { exit 3 }

$rows = @()
foreach ($item in $tree.FindAll($descendants, $rowCondition)) {
    $current = $item.Current
    if ($current.ControlType -eq $controlType::Button) {
        if ($current.Name -match '^Show \d+ more') { $rows += ,@($item, 'more') }
    } elseif ($current.ClassName -match 'sessionRow') {
        $rows += ,@($item, 'session')
    } else {
        $rows += ,@($item, 'workspace')
    }
}

if ($env:AGENT_CONTROL_ACTION -eq 'read') {
    $entries = @()
    foreach ($row in $rows) {
        $item, $kind = $row
        $selected = $false
        $expanded = $null
        try { $selected = $item.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected } catch {}
        try {
            $state = $item.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Current.ExpandCollapseState
            $expanded = $state -ne [Windows.Automation.ExpandCollapseState]::Collapsed
        } catch {}
        $entries += [pscustomobject]@{ kind = $kind; name = $item.Current.Name; selected = $selected; expanded = $expanded }
    }
    ConvertTo-Json -InputObject @($entries) -Compress
    exit 0
}

$index = [int]$env:AGENT_CONTROL_INDEX
if ($index -lt 0 -or $index -ge $rows.Count) { exit 3 }
$item = $rows[$index][0]
if ($item.Current.Name -ne $env:AGENT_CONTROL_NAME) { exit 3 }  # list changed since it was read

if ($env:AGENT_CONTROL_ACTION -eq 'select') {
    $item.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
    exit 0
}
if ($env:AGENT_CONTROL_ACTION -ne 'click') { exit 2 }

# Expanding a workspace through UI Automation (ExpandCollapse.Expand) crashes the dsh
# renderer, so rows are opened the way a user does it: a real mouse click on the row's text.
# In the persistent host (workers.py) the type stays loaded between calls.
if (-not ('Mouse' -as [type])) {
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Mouse {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int x, int y, uint data, UIntPtr extra);
    public static bool Covers(IntPtr window, int x, int y) {
        POINT p; p.X = x; p.Y = y;
        return GetAncestor(WindowFromPoint(p), 2) == window;  // GA_ROOT
    }
    public static void Click(int x, int y) {
        POINT old; GetCursorPos(out old);
        SetCursorPos(x, y);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);  // left down
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);  // left up
        System.Threading.Thread.Sleep(50);
        SetCursorPos(old.X, old.Y);
    }
}
"@
}
[Mouse]::SetProcessDPIAware() | Out-Null  # UI Automation reports physical pixels

$target = $item
$text = $item.FindFirst([Windows.Automation.TreeScope]::Children,
    (New-Object Windows.Automation.PropertyCondition($element::ControlTypeProperty, $controlType::Text)))
if ($text -ne $null -and -not $text.Current.BoundingRectangle.IsEmpty) { $target = $text }
$box = $target.Current.BoundingRectangle
if ($box.IsEmpty) { exit 4 }  # row is scrolled out of view
$x = [int]($box.Left + $box.Width / 2)
$y = [int]($box.Top + $box.Height / 2)
if (-not [Mouse]::Covers([IntPtr][long]$env:AGENT_CONTROL_WINDOW, $x, $y)) { exit 4 }  # another window is on top
[Mouse]::Click($x, $y)
exit 0
