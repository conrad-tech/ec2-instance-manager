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
#
# The parameters are deliberately NOT Mandatory, and take "" defaults --
# the same shape send_access_email.ps1 uses, for the same two reasons.
# Mandatory would have the binder reject `-To ''` before the guard below
# could name it, so the guard would be dead code and the GUI would see a
# PowerShell binder error instead of `FAILED reason='no recipient
# configured'`. Worse, an OMITTED mandatory parameter makes PowerShell
# *prompt* for it -- and this is spawned with CREATE_NO_WINDOW and no
# console, so the prompt is an invisible hang, not a failure.
param(
    [string]$To      = "",   # escalation mailbox; blank is refused below
    [string]$Subject = ""    # "<CODE> <createdAt>" -- the entire payload
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

if ([string]::IsNullOrWhiteSpace($Subject)) {
    # An empty subject IS an empty payload: the subject is the whole
    # message, so this would send a blank mail the daemon cannot tier.
    Write-Marker "FAILED reason='no subject to send'"
    exit 1
}

try {
    $outlook = New-Object -ComObject Outlook.Application
    # GetNamespace("MAPI") is inside the same try on purpose. New-Object
    # generally succeeds whenever Outlook is merely *installed*; the "no
    # mail profile" failure only surfaces at the first MAPI call, and out
    # here it would fall out of the Send() catch below as a raw COM string
    # instead of the sentence that names the actual problem.
    # send_access_email.ps1 pairs the two calls for the same reason.
    $null = $outlook.GetNamespace("MAPI")
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
