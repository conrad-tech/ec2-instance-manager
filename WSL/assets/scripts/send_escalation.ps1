# send_escalation.ps1 -- send one content-free escalation email via Outlook.
#
# Deliberately NOT part of send_access_email.ps1. That script's recipient
# gates (directory user, domain allow-list, local-format match) exist because
# it attaches a PRIVATE KEY. A fixed configured address with no attachment
# must not inherit them, and those gates must not be relaxed to fit this.
#
# Prints exactly one marker line the GUI parses:
#   SENT address='someone@example.com'
#   FAILED reason='Outlook is not available'
param(
    [Parameter(Mandatory = $true)][string]$To,
    [Parameter(Mandatory = $true)][string]$Subject
)

$ErrorActionPreference = 'Stop'

function Write-Marker {
    param([string]$Line)
    Write-Output $Line
}

if ([string]::IsNullOrWhiteSpace($To)) {
    Write-Marker "FAILED reason='no recipient configured'"
    exit 1
}

try {
    $outlook = New-Object -ComObject Outlook.Application
} catch {
    Write-Marker "FAILED reason='Outlook is not available'"
    exit 1
}

try {
    $mail = $outlook.CreateItem(0)
    $mail.To = $To
    $mail.Subject = $Subject
    # The body stays empty. The subject is the entire payload -- see
    # 2026-08-14-escalation-notifier-design.md.
    $mail.Body = ''
    $mail.Send()
    Write-Marker "SENT address='$To'"
    exit 0
} catch {
    $reason = $_.Exception.Message -replace "'", '' -replace "`r?`n", ' '
    Write-Marker "FAILED reason='$reason'"
    exit 1
}
