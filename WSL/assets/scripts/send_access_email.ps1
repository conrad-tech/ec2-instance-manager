# send_access_email.ps1 - compose (and, when safe, send) the "bastion access"
# email in Outlook after a user is created by ec2_manager_gui.
#
# Launched by the GUI (Windows only) once create_new_user.sh has verified the
# new user and the PEM has been pulled to ~/Downloads. All Outlook/COM logic
# lives here so it can be reviewed and tweaked without rebuilding the app.
#
# Flow:
#   * Compose headless (To/Subject/Body/PEM) - nothing is shown yet.
#   * Apply encryption via the Outlook object model using the values the app
#     passes from features.json (discovered with outlook_verification.ps1).
#   * Resolve the recipient. Resolve() is TRUE only for a single unambiguous
#     match; FALSE for 2+ same-named people (or none).
#   * If single match AND encryption confirmed -> send headless, then show a
#     "sent" confirmation.
#   * Otherwise (ambiguous/none, encryption unconfirmed, or send error) ->
#     open the draft, apply the QAT Encrypt shortcut, and let the user resolve
#     the recipient / encryption and click Send.

param(
    [string]$Username        = "",   # firstname.lastname, as typed in the app
    [string]$EnvTag          = "",   # MMODAL_ENV tag of the primary bastion
    [string]$Primary         = "",   # primary bastion instance id
    [string]$Secondary       = "",   # secondary bastion instance id
    [string]$Pem             = "",   # local path to the PEM to attach
    [string]$TemplateGuid      = "",   # RMS/IRM template GUID (tenant-specific)
    [int]   $Permission        = 0,    # MailItem.Permission value to set (0=skip)
    [int]   $PermissionService = 0,    # MailItem.PermissionService (1=olWindows)
    [int]   $SmimeFlag         = 0,    # S/MIME security flag (0=skip)
    [string]$EncryptSendKeys   = "%6"  # QAT Encrypt shortcut for the visible path
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms

$SEC_PROP = "http://schemas.microsoft.com/mapi/proptag/0x6E010003"

function Title([string]$s) {
    if ([string]::IsNullOrWhiteSpace($s)) { return $s }
    return $s.Substring(0, 1).ToUpper() + $s.Substring(1).ToLower()
}

function Show-Box([string]$text, [string]$icon) {
    try {
        [System.Windows.Forms.MessageBox]::Show(
            $text, "EC2 Manager - Access Email",
            [System.Windows.Forms.MessageBoxButtons]::OK,
            [System.Windows.Forms.MessageBoxIcon]::$icon) | Out-Null
    } catch {}
}

# firstname.lastname -> "Firstname Lastname" for the To field, first name alone
# for the greeting.
$parts = @($Username.Split('.') | Where-Object { $_ -ne "" })
if ($parts.Count -ge 1) {
    $firstName   = Title $parts[0]
    $displayName = ($parts | ForEach-Object { Title $_ }) -join ' '
} else {
    $firstName   = Title $Username
    $displayName = Title $Username
}

$outlook = New-Object -ComObject Outlook.Application
$ns      = $outlook.GetNamespace("MAPI")

# Sender first name (signature) from the Outlook profile. Handles both
# "Last, First" and "First Last" orderings.
$senderFirst = ""
try {
    $me = "$($ns.CurrentUser.Name)".Trim()
    if ($me -match ',') {
        $senderFirst = Title ((($me.Split(',')[1]).Trim() -split '\s+')[0])
    } elseif ($me) {
        $senderFirst = Title (($me -split '\s+')[0])
    }
} catch { $senderFirst = "" }

$mail = $outlook.CreateItem(0)   # olMailItem
$mail.Subject = "Access for $EnvTag Bastion EC2s"
$mail.Body = @"
Hello $firstName,

You have been given access to the $EnvTag bastion EC2s. Attached is your PEM file and below are the login details.

Username: $Username

Primary Bastion: $Primary
Secondary Bastion: $Secondary

Thanks,
$senderFirst
"@

if ($Pem -and (Test-Path -LiteralPath $Pem)) {
    try { $mail.Attachments.Add($Pem) | Out-Null } catch {}
}

# To: add the display name and resolve. Resolve() is TRUE only for one
# unambiguous match; FALSE for 2+ same-named people (or none).
$recip = $mail.Recipients.Add($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }

# --- Apply encryption headless (single-recipient path only) --------------
# Only encrypt headless when the recipient resolved to exactly one person. For
# an ambiguous/no match we leave the item a plain draft so the window reliably
# opens and Alt+6 encrypts it visibly (Alt+6 is a TOGGLE, so it must never be
# pressed on an already-encrypted item).
# Preference: RMS/IRM template -> S/MIME flag -> bare Permission value.
$encConfirmed = $false
if ($resolved) {
if ($TemplateGuid) {
    # Proven headless sequence: Permission first, then PermissionService, then
    # the GUID last. Setting the GUID throws "The operation failed" but the value
    # sticks and the template is applied at send time - so we confirm by reading
    # BOTH Permission and PermissionTemplateGuid back, not by the setter's error.
    try {
        if ($Permission -ne 0) { $mail.Permission = $Permission } else { $mail.Permission = 4 }
    } catch {}
    if ($PermissionService -ne 0) { try { $mail.PermissionService = $PermissionService } catch {} }
    try { $mail.PermissionTemplateGuid = $TemplateGuid } catch {}
    if ([int]$mail.Permission -ne 0 -and "$($mail.PermissionTemplateGuid)") { $encConfirmed = $true }
} elseif ($SmimeFlag -ne 0) {
    try {
        $mail.PropertyAccessor.SetProperty($SEC_PROP, $SmimeFlag)
        if ([int]($mail.PropertyAccessor.GetProperty($SEC_PROP)) -ne 0) { $encConfirmed = $true }
    } catch {}
} elseif ($Permission -ne 0) {
    try {
        $mail.Permission = $Permission
        if ([int]$mail.Permission -ne 0) { $encConfirmed = $true }
    } catch {}
}
}  # end: encrypt only when resolved to a single recipient

# --- Decide: send headless, or open for the user -------------------------
$sent = $false
if ($resolved -and $encConfirmed) {
    try { $mail.Send(); $sent = $true } catch { $sent = $false }
}

if ($sent) {
    Show-Box "Email sent successfully to $displayName." "Information"
    Write-Output "SENT recipient='$displayName'"
    return
}

# Not sent - open the draft, apply the QAT Encrypt shortcut, and explain.
$reason =
    if (-not $resolved) {
        "More than one contact (or none) matches '$displayName'.`n`n" +
        "Encryption has been applied for you (Alt+6). Pick the correct recipient " +
        "in the To field, confirm it still shows encrypted, then click Send."
    } elseif (-not $encConfirmed) {
        "The email is ready but encryption could not be confirmed automatically.`n`n" +
        "Applying your Encrypt shortcut now - verify the email shows as encrypted, then click Send.`n`n" +
        "If it did not encrypt, click Options > Encrypt manually before sending."
    } else {
        "The email could not be sent automatically. It is open below - review it and click Send."
    }

$inspector = $mail.GetInspector
$inspector.Display($false)
Start-Sleep -Milliseconds 700
try { $inspector.Activate() } catch {}
Start-Sleep -Milliseconds 250
# Best-effort: apply the QAT Encrypt shortcut on the now-visible window.
if (-not $encConfirmed -and $EncryptSendKeys) {
    try { [System.Windows.Forms.SendKeys]::SendWait($EncryptSendKeys); Start-Sleep -Milliseconds 400 } catch {}
}

Show-Box $reason "Warning"
Write-Output "OPEN recipient='$displayName' resolved=$resolved encrypted=$encConfirmed"
