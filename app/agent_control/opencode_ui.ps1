# Switches the OpenCode Desktop window to the tab of a session through UI Automation.
# Input comes from environment variables to avoid quoting problems:
#   AGENT_CONTROL_WINDOW - window handle
#   AGENT_CONTROL_ID     - session id (ses_...)
# Every tab of the title bar is a hyperlink whose value is the session address
# "oc://renderer/server/<server>/session/<id>", so the tab is found by id, not by title.
# Invoke opens the tab even when it is scrolled out of the tab strip.
# Only this window is queried: scanning the desktop would also query the calling
# application's window, which is blocked waiting for this script, and hang both.
# Chromium exposes the page only while the window is visible: covered by other windows
# it shows an empty document, so the caller brings the window forward first.
# Exit code 3: window or tab strip not found. Exit code 5: no tab of this session.
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

$element = [Windows.Automation.AutomationElement]
$descendants = [Windows.Automation.TreeScope]::Descendants
$linkCondition = New-Object Windows.Automation.PropertyCondition(
    $element::ControlTypeProperty, [Windows.Automation.ControlType]::Hyperlink)

try {
    $window = $element::FromHandle([IntPtr][long]$env:AGENT_CONTROL_WINDOW)
} catch {
    exit 3
}

function Get-Tabs {
    $tabs = @()
    foreach ($link in $window.FindAll($descendants, $linkCondition)) {
        $value = ''
        try { $value = $link.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value } catch {}
        if ($value -match '^oc://[^/]+/server/[^/]+/session/([^/?#]+)$') { $tabs += ,@($link, $Matches[1]) }
    }
    return ,$tabs
}

# Chromium builds the accessibility tree only after the first request: wait for it.
$tabs = @()
for ($attempt = 0; $attempt -lt 15 -and $tabs.Count -eq 0; $attempt++) {
    $tabs = Get-Tabs
    if ($tabs.Count -eq 0) { Start-Sleep -Milliseconds 200 }
}
if ($tabs.Count -eq 0) { exit 3 }

foreach ($tab in $tabs) {
    $link, $id = $tab
    if ($id -ne $env:AGENT_CONTROL_ID) { continue }
    try { $link.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView() } catch {}
    $link.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    exit 0
}
exit 5
