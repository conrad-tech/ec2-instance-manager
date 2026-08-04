# test_access_email.ps1 - quick standalone check of the access-email encryption
# path. Type your username (firstname.lastname); it turns that into
# "Firstname Lastname", resolves you in Outlook, applies encryption headless,
# and sends the test to YOU so you can confirm it arrives encrypted.
#
# Run:
#   powershell -NoProfile -ExecutionPolicy Bypass -File test_access_email.ps1
# or with args:
#   ... -File test_access_email.ps1 -Username john.smith -TemplateGuid <guid> -Permission 3

param(
    [string]$Username     = "",
    [string]$TemplateGuid = "",   # paste your tenant RMS/IRM template GUID
    [int]   $Permission   = 0,    # the MailItem.Permission value you discovered (e.g. 3)
    [int]   $SmimeFlag    = 0     # only if you use S/MIME instead of a template
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms
$SEC_PROP = "http://schemas.microsoft.com/mapi/proptag/0x6E010003"

if (-not $Username)     { $Username     = Read-Host "Username (firstname.lastname)" }
if (-not $TemplateGuid -and $Permission -eq 0 -and $SmimeFlag -eq 0) {
    $TemplateGuid = Read-Host "Encryption template GUID (leave blank to skip)"
    if ($TemplateGuid) { $Permission = [int](Read-Host "Permission value (e.g. 3)") }
}

function Title([string]$s) {
    if ([string]::IsNullOrWhiteSpace($s)) { return $s }
    return $s.Substring(0, 1).ToUpper() + $s.Substring(1).ToLower()
}

# firstname.lastname -> "Firstname Lastname"
$parts = @($Username.Split('.') | Where-Object { $_ -ne "" })
$displayName = ($parts | ForEach-Object { Title $_ }) -join ' '
"To (display name): '$displayName'"

$outlook = New-Object -ComObject Outlook.Application
$mail    = $outlook.CreateItem(0)   # olMailItem
$mail.Subject = "Encryption test - access email"
$mail.Body    = "This is a test of the access-email encryption path. If this arrived encrypted, the automation works."

# Resolve the recipient (you). Resolve() is TRUE only for one unambiguous match;
# FALSE if the name is ambiguous (2+ people) or not found.
$recip = $mail.Recipients.Add($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch { "Resolve error: $_" }
"Recipient resolved: $resolved"
if ($resolved) {
    try { "Resolved to: $($recip.AddressEntry.Name) <$($recip.Address)>" } catch {}
}

# Apply encryption headless, then read it back to confirm.
$encConfirmed = $false
if ($TemplateGuid) {
    try {
        # Try the discovered Permission value (e.g. 3), else olPermissionTemplate
        # (4), set before the GUID. Both tend to be rejected in tenants where
        # encryption is a sensitivity label - the Alt+6 path below covers that.
        if ($Permission -ne 0) { $mail.Permission = $Permission } else { $mail.Permission = 4 }
        $mail.PermissionTemplateGuid = $TemplateGuid
        if ([int]$mail.Permission -ne 0) { $encConfirmed = $true }
    } catch { "Template set error: $_" }
} elseif ($SmimeFlag -ne 0) {
    try {
        $mail.PropertyAccessor.SetProperty($SEC_PROP, $SmimeFlag)
        if ([int]($mail.PropertyAccessor.GetProperty($SEC_PROP)) -ne 0) { $encConfirmed = $true }
    } catch { "S/MIME set error: $_" }
} elseif ($Permission -ne 0) {
    try { $mail.Permission = $Permission; if ([int]$mail.Permission -ne 0) { $encConfirmed = $true } } catch {}
}
"Permission now: $([int]$mail.Permission)   encryption confirmed: $encConfirmed"

if ($resolved -and $encConfirmed) {
    $mail.Send()
    "SENT headless (encryption confirmed) - check your inbox."
} elseif ($resolved) {
    # Headless encryption wasn't settable - use the visible Alt+6 path, then
    # send via COM. This is the auto-send flow we're confirming.
    $mail.Display($false)
    Start-Sleep -Milliseconds 700
    try { $mail.GetInspector.Activate() } catch {}
    Start-Sleep -Milliseconds 400
    try { [System.Windows.Forms.SendKeys]::SendWait("%6") } catch { "SendKeys error: $_" }
    Start-Sleep -Milliseconds 1500          # let the encryption apply before sending
    try { $mail.Send(); "SENT via Alt+6 - check your inbox and confirm it is encrypted." }
    catch { "Send error: $_  (draft left open)" }
} else {
    $mail.Display($false)
    "NOT sent - '$displayName' did not resolve to a single person."
}
