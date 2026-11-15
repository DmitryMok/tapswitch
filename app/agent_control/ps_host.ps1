# Persistent PowerShell host (see workers.py): runs UI Automation scripts without paying
# for PowerShell start-up and assembly loading on every call.
# Each input line is JSON {"script": path, "env": {name: value}}; each reply is one JSON
# line {"code": exit code, "out": script output}. `exit N` inside the script ends only
# the script and becomes its exit code.
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
[Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
while ($true) {
    $line = [Console]::In.ReadLine()
    if ($null -eq $line) { break }
    try {
        $request = $line | ConvertFrom-Json
        foreach ($property in $request.env.PSObject.Properties) {
            Set-Item "env:$($property.Name)" ([string]$property.Value)
        }
        $global:LASTEXITCODE = 0
        $out = & $request.script | Out-String
        $code = $global:LASTEXITCODE
    } catch {
        $out = $_.ToString()
        $code = 1
    }
    [Console]::Out.WriteLine((@{ code = $code; out = $out } | ConvertTo-Json -Compress))
    [Console]::Out.Flush()
}
