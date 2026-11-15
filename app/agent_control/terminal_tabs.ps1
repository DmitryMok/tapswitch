# Reads or selects a Windows Terminal tab through UI Automation.
# Input comes from environment variables to avoid quoting problems:
#   AGENT_CONTROL_ACTION - "read": print JSON with the selected tab id and its screen text
#                          "select": select the tab with id AGENT_CONTROL_TAB
#                          "list": print JSON with the ids and titles of all tabs and which one is selected
#                          "focus": give keyboard focus to the terminal of the selected tab
#   AGENT_CONTROL_WINDOW - window handle
# Only this window is queried: scanning the desktop would also query the calling
# application's window, which is blocked waiting for this script, and hang both.
# Exit code 3: window or tab not found.
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

$element = [Windows.Automation.AutomationElement]
$descendants = [Windows.Automation.TreeScope]::Descendants
$tabCondition = New-Object Windows.Automation.PropertyCondition($element::ControlTypeProperty, [Windows.Automation.ControlType]::TabItem)
$textCondition = New-Object Windows.Automation.PropertyCondition($element::IsTextPatternAvailableProperty, $true)
$selectionPattern = [Windows.Automation.SelectionItemPattern]::Pattern

try {
    $window = $element::FromHandle([IntPtr][long]$env:AGENT_CONTROL_WINDOW)
    $tabs = $window.FindAll($descendants, $tabCondition)
} catch {
    exit 3
}

function Get-TabId($tab) { $tab.GetRuntimeId() -join '.' }

if ($env:AGENT_CONTROL_ACTION -eq 'select') {
    foreach ($tab in $tabs) {
        if ((Get-TabId $tab) -eq $env:AGENT_CONTROL_TAB) {
            $tab.GetCurrentPattern($selectionPattern).Select()
            exit 0
        }
    }
    exit 3
}

# Selecting a tab through UI Automation leaves keyboard focus on the tab header, so typed
# text goes nowhere. Move it into the terminal itself; with split panes, into the largest one.
if ($env:AGENT_CONTROL_ACTION -eq 'focus') {
    $termCondition = New-Object Windows.Automation.PropertyCondition($element::ClassNameProperty, 'TermControl')
    $best = $null
    $bestArea = -1
    foreach ($term in $window.FindAll($descendants, $termCondition)) {
        $rect = $term.Current.BoundingRectangle
        $area = if ($rect.IsEmpty) { 0 } else { $rect.Width * $rect.Height }
        if ($area -gt $bestArea) { $best = $term; $bestArea = $area }
    }
    if ($null -eq $best) { exit 3 }
    $best.SetFocus()
    exit 0
}

if ($env:AGENT_CONTROL_ACTION -eq 'list') {
    $entries = @()
    foreach ($tab in $tabs) {
        $entries += @{ tab = (Get-TabId $tab); title = $tab.Current.Name; selected = $tab.GetCurrentPattern($selectionPattern).Current.IsSelected }
    }
    ConvertTo-Json -InputObject @($entries) -Compress
    exit 0
}

$selected = $null
foreach ($tab in $tabs) {
    if ($tab.GetCurrentPattern($selectionPattern).Current.IsSelected) { $selected = $tab }
}
# A full-screen window hides its tab strip: the screen is readable, the tabs are not.
$selectedId = if ($null -ne $selected) { Get-TabId $selected } else { $null }

# The terminal buffer is the largest text element; the others are short tab titles.
$screen = ''
$screenElement = $null
foreach ($item in $window.FindAll($descendants, $textCondition)) {
    $textPattern = $item.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern)
    $text = $textPattern.DocumentRange.GetText(400000)
    if ($text.Length -gt $screen.Length) { $screen = $text; $screenElement = $textPattern }
}
# herdr draws a sidebar ("spaces" and the workspace list) and a tab strip on the first row.
# Report each entry with its background colour; the caller decides which one is highlighted.
function Get-Chrome($textElement, $lines) {
    $separator = [string][char]0x2502
    $head = $lines[0]
    $column = $head.IndexOf($separator)
    if ($column -lt 0) { return $null }
    # Expanded sidebar: "spaces" header, then "<mark> <label>" rows.
    # Collapsed sidebar: "<number> <mark>" rows starting on the first row.
    $sidebarHead = $head.Substring(0, $column)
    $collapsed = $sidebarHead -match '^\s*\d+\s+\S\s*$'
    if (-not $collapsed -and $sidebarHead.Trim() -ne 'spaces') { return $null }

    $pattern = [Windows.Automation.TextPattern]
    $unit = [Windows.Automation.Text.TextUnit]
    $startPoint = [Windows.Automation.Text.TextPatternRangeEndpoint]::Start
    $endPoint = [Windows.Automation.Text.TextPatternRangeEndpoint]::End
    function Get-Background($row, $col) {
        $range = $textElement.DocumentRange.Clone()
        $range.MoveEndpointByRange($endPoint, $range, $startPoint)
        [void]$range.Move($unit::Line, $row)
        [void]$range.Move($unit::Character, $col)
        [void]$range.ExpandToEnclosingUnit($unit::Character)
        [int]$range.GetAttributeValue($pattern::BackgroundColorAttribute)
    }

    $workspaces = @()
    $rowPattern = if ($collapsed) { '^\s*(\d+)\s+\S\s*$' } else { '^\s*\S\s+(\S.*?)\s*$' }
    $sidebarBackground = $null
    for ($row = $(if ($collapsed) { 0 } else { 1 }); $row -lt $lines.Count; $row++) {
        if ($lines[$row].Length -lt $column) { break }
        $cell = $lines[$row].Substring(0, $column)
        $match = [regex]::Match($cell, $rowPattern)
        if ($match.Success) {
            $workspaces += @{ label = $match.Groups[1].Value; background = (Get-Background $row $match.Groups[1].Index) }
        } elseif ($workspaces.Count -gt 0) {
            $sidebarBackground = Get-Background $row 0
            break
        }
    }
    $tabEntries = @()
    $strip = $head.Substring($column + 1)
    foreach ($match in [regex]::Matches($strip, '\S+(?: \S+)*')) {
        if ($match.Value -ne '+') {
            $tabEntries += @{ label = $match.Value; background = (Get-Background 0 ($column + 1 + $match.Index)) }
        }
    }
    if (-not $collapsed) { $sidebarBackground = Get-Background 0 0 }
    @{ collapsed = [bool]$collapsed; sidebar_background = $sidebarBackground; workspaces = $workspaces; tabs = $tabEntries }
}

$chrome = $null
try { $chrome = Get-Chrome $screenElement ($screen -split "`r?`n") } catch { $chrome = $null }
@{ tab = $selectedId; text = $screen; chrome = $chrome } | ConvertTo-Json -Compress -Depth 5
