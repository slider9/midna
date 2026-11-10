# midna shell integration (written by midnad; setting agents.adopt_typed). Windows PowerShell and
# PowerShell 7 on Windows.
# midna starts PowerShell with -NoExit and a command that runs this once the user's profiles have
# run. It wraps the user's prompt, which then, before each prompt:
# - keeps midna's shims first on PATH (a profile may have put other folders in front);
# - makes PowerShell's location the process's working folder. midna reads a terminal's folder from
#   there, and Set-Location never changes it;
# - marks the prompt and the input (OSC 133 A and B) around the user's prompt.
# With PSReadLine, Enter also marks where the command's output starts (C).
#
# ConPTY on Windows 10 passes a mark on at once but paints text on its next frame, so a mark sent
# along with text would arrive ahead of it. Each mark goes out on its own, after a pause that lets
# the text before it reach the screen.
if (-not (Test-Path variable:global:__midna_prompt)) {
    $global:__midna_prompt = $function:prompt
    function global:prompt {
        # First, so the user's prompt sees $? and $LASTEXITCODE as the command left them.
        $p = -join (& $global:__midna_prompt)
        $s = $env:MIDNA_SHIMS
        if ($s -and ($env:PATH -split ';')[0] -ne $s) {
            $env:PATH = (@($s) + @($env:PATH -split ';' | Where-Object { $_ -and $_ -ne $s })) -join ';'
        }
        $loc = $ExecutionContext.SessionState.Path.CurrentLocation
        if ($loc.Provider.Name -eq 'FileSystem') {
            [Environment]::CurrentDirectory = $loc.ProviderPath
        }
        $e = [char]27
        $b = [char]7
        Start-Sleep -Milliseconds 15
        $Host.UI.Write("$e]133;A$b")
        $Host.UI.Write($p)
        Start-Sleep -Milliseconds 15
        "$e]133;B$b"
    }
    $enter = if (Get-Module PSReadLine) { Get-PSReadLineKeyHandler -Bound | Where-Object { $_.Key -eq 'Enter' } | Select-Object -First 1 }
    # Only Enter's own functions are passed on; a handler of the user's own is left alone.
    if ($enter -and $enter.Function -in 'AcceptLine', 'ValidateAndAcceptLine') {
        $global:__midna_enter = $enter.Function
        Set-PSReadLineKeyHandler -Key Enter -Description "$($enter.Description) (midna marks the output)" -ScriptBlock {
            param($key, $arg)
            $line = $null
            $cursor = $null
            [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
            $errors = $null
            $null = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$null, [ref]$errors)
            # An unfinished command gets another line, not run.
            $runs = -not ($errors | Where-Object { $_.IncompleteInput })
            [Microsoft.PowerShell.PSConsoleReadLine]::$($global:__midna_enter)($key, $arg)
            if ($runs) {
                $Host.UI.Write("$([char]27)]133;C$([char]7)")
            }
        }
    }
    Remove-Variable enter
}
