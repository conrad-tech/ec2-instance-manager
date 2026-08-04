# send_access_email.ps1 - compose (and, when safe, send) the "bastion access"
# email in Outlook after a user is created by ec2_manager_gui.
#
# Launched by the GUI (Windows only) once create_new_user.sh has verified the
# new user and the PEM has been pulled to ~/Downloads. All Outlook/COM logic
# lives here so it can be reviewed and tweaked without rebuilding the app.
#
# Flow:
#   * Compose headless (To/Subject/Body/PEM) - nothing is shown yet.
#   * Resolve the recipient. Resolve() is TRUE only for a single unambiguous
#     match; FALSE for 2+ same-named people AND for no match at all. We do not
#     try to tell those two apart - the user does the same thing either way,
#     and counting would need a full GAL enumeration or an LDAP query.
#   * Verify the resolved address is in -Domain. Resolve() also matches the
#     local Contacts folder and the autocomplete cache, so this is what stops a
#     stale personal entry from being mailed a private key. Blank -Domain
#     skips the check.
#   * Encrypt via the object model using the values features.json supplies
#     (discovered with outlook_verification.ps1). Done whether or not the
#     recipient resolved, so a draft that opens is already encrypted rather
#     than depending on the Alt+6 keystroke landing.
#   * Send headless ONLY when resolved AND in-domain AND encryption confirmed.
#   * Otherwise clear the To field, open the draft, apply the QAT Encrypt
#     shortcut if encryption did not confirm, and explain why.
#
# Every run ends with one machine-readable marker on stdout, which the GUI
# parses to show a status line:
#   SENT recipient='<name>' address='<smtp>'
#   OPEN recipient='<name>' resolved=<bool> domain_ok=<bool> encrypted=<bool>

param(
    [string]$Username        = "",   # firstname.lastname, as typed in the app
    [string]$EnvTag          = "",   # MMODAL_ENV tag of the primary bastion
    [string]$Primary         = "",   # primary bastion instance id
    [string]$Secondary       = "",   # secondary bastion instance id
    [string]$Pem             = "",   # local path to the PEM to attach
    [string]$Domain          = "",   # org mail domain; resolved address must match
    [switch]$Quiet,                  # suppress message boxes (GUI shows status)
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
    # -Quiet is set on the auto-run path, where the GUI renders the outcome.
    # A modal from a process the user did not knowingly start is worse than no
    # modal at all; the stdout marker still carries the result either way.
    if ($Quiet) { return }
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

# The MMODAL_ENV tag is not consistently cased across instances, so uppercase
# it here rather than trusting whatever the tag happened to hold.
$envUpper = "$EnvTag".ToUpper()
$mail.Subject = "Bastion Access for $envUpper"
$mail.Body = @"
Hello $firstName,

See below for your login credentials and attached is your PEM file.

Username: $Username

Primary Bastion: $Primary

Secondary Bastion: $Secondary

Thanks,
$senderFirst
"@

if ($Pem -and (Test-Path -LiteralPath $Pem)) {
    try { $mail.Attachments.Add($Pem) | Out-Null } catch {}
}

# To: add the display name and resolve. Resolve() is TRUE only for ONE
# unambiguous match; FALSE for 2+ same-named people AND for no match at all.
# We deliberately do not tell those two failures apart: the user does the same
# thing either way (pick the right person in an empty To field), and counting
# would need a full GAL enumeration or an LDAP query.
$recip = $mail.Recipients.Add($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }

# Resolve() also matches the local Contacts folder and the autocomplete cache,
# not just the GAL. A stale personal entry for the same name would otherwise be
# mailed a private key, so a resolved address must sit in the configured domain.
$smtp     = ""
$domainOk = $false
if ($resolved) {
    try { $smtp = "$($recip.AddressEntry.GetExchangeUser().PrimarySmtpAddress)" } catch { $smtp = "" }
    if (-not $smtp) { try { $smtp = "$($recip.Address)" } catch { $smtp = "" } }

    if (-not $Domain) {
        # No domain configured - check disabled, preserving older behavior.
        $domainOk = $true
    } elseif ($smtp -like "*@*") {
        $addrDomain = ($smtp -split '@')[-1]
        $domainOk = $addrDomain.Trim().ToLower() -eq $Domain.Trim().ToLower()
    }
}

# The shipped placeholder GUID is all zeros. Treat it as NOT configured:
# otherwise Permission reads back non-zero and the placeholder reads back
# non-empty, encConfirmed goes true, and a private key is sent believing it
# is encrypted. Strips braces, dashes and zeros - nothing left means zeros.
$encConfigured = $true
if ($TemplateGuid -and (($TemplateGuid -replace '[{}\-0]', '') -eq '')) {
    Write-Output "WARN encrypt_template_guid is the unconfigured all-zeros placeholder"
    $TemplateGuid  = ""
    $encConfigured = $false
}
# Note: a deliberate Permission-only setup (e.g. encrypt_permission = 2, Do Not
# Forward, with no template GUID) keeps $encConfigured true and still confirms.
# Only a cleared placeholder disqualifies the bare-Permission path below.

# --- Apply encryption headless -------------------------------------------
# Applied whether or not the recipient resolved, so a draft that opens is
# already encrypted rather than depending on the Alt+6 keystroke landing.
# Alt+6 is a TOGGLE and is only sent further down when this did NOT confirm.
# Preference: RMS/IRM template -> S/MIME flag -> bare Permission value.
$encConfirmed = $false
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
        # Only counts when the admin chose a Permission-only scheme (e.g. 2 =
        # Do Not Forward) rather than falling here because the template GUID
        # was never configured.
        if ([int]$mail.Permission -ne 0 -and $encConfigured) { $encConfirmed = $true }
    } catch {}
}

# --- Decide: send headless, or open for the user -------------------------
# All three must hold. The attachment is a private key; it never leaves
# unattended without a confirmed single recipient in our own domain and
# confirmed encryption.
$sent = $false
if ($resolved -and $domainOk -and $encConfirmed) {
    try { $mail.Send(); $sent = $true } catch { $sent = $false }
}

if ($sent) {
    Show-Box "Email sent successfully to $displayName ($smtp)." "Information"
    Write-Output "SENT recipient='$displayName' address='$smtp'"
    return
}

# Not sent. Clear the To field first so nobody is pre-selected - an ambiguous
# name left sitting there invites sending to the wrong person.
try { while ($mail.Recipients.Count -gt 0) { $mail.Recipients.Remove(1) } } catch {}

$reason =
    if (-not $resolved) {
        "Could not identify a single recipient for '$displayName' - either nobody matches or more than one person does.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $domainOk) {
        "'$displayName' resolved to $smtp, which is not in $Domain.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $encConfigured) {
        "Encryption is not configured: encrypt_template_guid is still the all-zeros placeholder.`n`n" +
        "Nothing was sent. Discover your tenant's template GUID with outlook_verification.ps1, put it in features.json and rebuild.`n`n" +
        "Applying your Encrypt shortcut now - verify the email shows as encrypted before sending it by hand."
    } elseif (-not $encConfirmed) {
        "The email is ready but encryption could not be confirmed automatically.`n`n" +
        "Applying your Encrypt shortcut now - verify the email shows as encrypted, then enter the recipient and click Send.`n`n" +
        "If it did not encrypt, click Options > Encrypt manually before sending."
    } else {
        "The email could not be sent automatically. It is open below - enter the recipient, review it and click Send."
    }

$inspector = $mail.GetInspector
$inspector.Display($false)
Start-Sleep -Milliseconds 700
try { $inspector.Activate() } catch {}
Start-Sleep -Milliseconds 250
# Best-effort: apply the QAT Encrypt shortcut on the now-visible window. Only
# when headless encryption did NOT confirm - Alt+6 toggles, so pressing it on
# an already-encrypted item would strip the encryption.
if (-not $encConfirmed -and $EncryptSendKeys) {
    try { [System.Windows.Forms.SendKeys]::SendWait($EncryptSendKeys); Start-Sleep -Milliseconds 400 } catch {}
}

Show-Box $reason "Warning"
Write-Output "OPEN recipient='$displayName' resolved=$resolved domain_ok=$domainOk encrypted=$encConfirmed enc_config=$encConfigured"
