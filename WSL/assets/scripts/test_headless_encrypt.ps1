# test_headless_encrypt.ps1 - test whether your org's encryption template can be
# applied HEADLESS (no Alt+6), and auto-discover the template GUID from an
# already-encrypted draft so you never copy/paste it (avoids terminal truncation).
#
# HOW TO USE
#   1. Outlook -> New Email -> apply Encrypt (Alt+6). Leave that draft OPEN.
#   2. Run this script. It reads the template GUID from that open draft, echoes
#      the FULL value, then builds a SECOND email and tries to encrypt it headless
#      via the object model (GUID -> Permission -> PermissionService), then shows
#      it so you can confirm the banner WITHOUT pressing Alt+6.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File test_headless_encrypt.ps1 -Username first.last

param(
    [string]$Username     = "",
    [string]$TemplateGuid = ""   # fallback if no encrypted draft is open
)
$ErrorActionPreference = "Stop"

function Title([string]$s) {
    if ([string]::IsNullOrWhiteSpace($s)) { return $s }
    return $s.Substring(0,1).ToUpper() + $s.Substring(1).ToLower()
}

if (-not $Username) { $Username = Read-Host "Your username (firstname.lastname)" }
$parts = @($Username.Split('.') | Where-Object { $_ -ne "" })
$displayName = ($parts | ForEach-Object { Title $_ }) -join ' '

$ol = New-Object -ComObject Outlook.Application

# --- Discover the template GUID + Permission from the open encrypted draft ----
$srcPerm = 0
$insp = $ol.ActiveInspector()
if ($null -ne $insp -and $null -ne $insp.CurrentItem) {
    try {
        $g = "$($insp.CurrentItem.PermissionTemplateGuid)"
        if ($g) { $TemplateGuid = $g }
        $srcPerm = [int]$insp.CurrentItem.Permission
    } catch { "Could not read from active draft: $($_.Exception.Message)" }
}
"Template GUID     : '$TemplateGuid'   <-- full value; paste this into features.json"
"Source Permission : $srcPerm"
if (-not $TemplateGuid) {
    "No GUID found. Open an encrypted draft first (Alt+6), or pass -TemplateGuid. Exiting."
    return
}

# --- Build a fresh email and try to encrypt it HEADLESS -----------------------
$mail = $ol.CreateItem(0)
$mail.Subject = "Headless encryption test"
$mail.Body    = "Testing headless template encryption (no Alt+6)."

$recip = $mail.Recipients.Add($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch {}
"Recipient resolved: $resolved"

# Sequence: Permission FIRST (the value your tenant accepts, e.g. 3), then
# PermissionService = 1 (olWindows), then the GUID last. GUID-first always
# threw "The operation failed", and Permission=3 is accepted, so we lead with it.
$encOk = $false

$permCandidates = @($srcPerm, 2, 4) | Where-Object { $_ -ne 0 } | Select-Object -Unique
foreach ($p in $permCandidates) {
    try { $mail.Permission = $p; "Permission set to $p"; break }
    catch { "  Permission=$p rejected: $($_.Exception.Message)" }
}
try { $mail.PermissionService = 1 }
catch { "  PermissionService=1 failed: $($_.Exception.Message)" }
try { $mail.PermissionTemplateGuid = $TemplateGuid; "Template GUID applied" }
catch { "  PermissionTemplateGuid failed: $($_.Exception.Message)" }

# Confirm for real: BOTH the Permission and the template GUID must read back set.
$permBack = [int]$mail.Permission
$guidBack = "$($mail.PermissionTemplateGuid)"
"Read-back -> Permission: $permBack   TemplateGuid: '$guidBack'"
if ($permBack -ne 0 -and $guidBack) { $encOk = $true }
"headless encryption confirmed (Permission AND GUID set): $encOk"

# Definitive test: SEND it to yourself headless (no window, no Alt+6). If it
# lands in your inbox encrypted, headless works end-to-end.
if ($encOk -and $resolved) {
    try {
        $mail.Send()
        "SENT to yourself HEADLESS (no window, no Alt+6). Check your inbox - is it ENCRYPTED?"
    } catch {
        "Send failed: $($_.Exception.Message)"
        $mail.Display($false)
        "Draft opened instead so you can inspect it."
    }
} else {
    $mail.Display($false)
    "Encryption not confirmed - draft opened for inspection (Alt+6 path still needed)."
}
