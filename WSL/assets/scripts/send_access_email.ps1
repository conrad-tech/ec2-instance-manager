# send_access_email.ps1 - compose (and, when safe, send) the "bastion access"
# email in Outlook after a user is created by ec2_manager_gui.
#
# Launched by the GUI (Windows only) once create_new_user.sh has verified the
# new user and the PEM has been pulled to ~/Downloads. All Outlook/COM logic
# lives here so it can be reviewed and tweaked without rebuilding the app.
#
# Flow:
#   * Compose headless (To/Subject/Body/PEM) - nothing is shown yet.
#   * Count how many people the directory matches for the name, the same way
#     Outlook's suggestion list does (LDAP Ambiguous Name Resolution). Exactly
#     one is required; 0 or 2+ opens Outlook instead. Recipient.Resolve() is
#     NOT the ambiguity test - it returns TRUE for a shared name by quietly
#     taking the nickname-cache entry.
#   * Verify the resolved address is in -Domain. Resolve() also matches the
#     local Contacts folder and the autocomplete cache, so this is what stops a
#     stale personal entry from being mailed a private key. Blank -Domain
#     skips the check.
#   * Encrypt via the object model using the values features.json supplies
#     (discovered with outlook_verification.ps1). Done whether or not the
#     recipient resolved, so a draft that opens is already encrypted rather
#     than depending on the Alt+6 keystroke landing.
#   * Check the address SHAPE against the username (-LocalFormat /
#     -ExpectedLocal). The domain check cannot catch an in-domain address that
#     belongs to a different person with a similar name; this can.
#   * Send headless ONLY when resolved AND in-domain AND name-shaped AND
#     encryption confirmed.
#   * Otherwise clear the To field, open the draft, apply the QAT Encrypt
#     shortcut if encryption did not confirm, and explain why.
#
# Every run ends with one machine-readable marker on stdout, which the GUI
# parses to show a status line:
#   SENT recipient='<name>' address='<smtp>'
#   OPEN recipient='<name>' resolved=<bool> domain_ok=<bool> local_ok=<bool>
#        encrypted=<bool> enc_config=<bool>

param(
    [string]$Username        = "",   # firstname.lastname, as typed in the app
    [string]$EnvTag          = "",   # MMODAL_ENV tag of the primary bastion
    [string]$Primary         = "",   # primary bastion instance id
    [string]$Secondary       = "",   # secondary bastion instance id
    [string]$Pem             = "",   # local path to the PEM to attach
    [string]$Domain          = "",   # org mail domain; resolved address must match
    [string]$LocalFormat     = "",   # non-empty turns the name-shape check on
    [string]$ExpectedLocal   = "",   # stem the address must be, e.g. "jsmith"
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

# --- How many people would Outlook suggest for this name? ----------------
# This is the ambiguity gate, and it deliberately does NOT use
# Recipient.Resolve(). Resolve() can return TRUE for a name several people
# share - it will quietly take the nickname/autocomplete cache entry - so it is
# not a safe test for "exactly one person". Outlook's suggestion list is
# Ambiguous Name Resolution against the directory, which is what LDAP's `anr`
# filter does, so we ask the directory the same question and count.
#
#   0 matches  -> nobody to send to        -> open Outlook
#   2+ matches -> the ambiguity we care about -> open Outlook
#   1 match    -> send to that address
#
# A directory that cannot be queried counts as -1. That is the normal case on
# an Entra-ID-joined machine with no on-prem AD reachable, so it does NOT
# disable the feature; it falls back to Outlook's own resolution, which is a
# weaker ambiguity check made safe by the three gates that follow (must be a
# real directory user, in -Domain, matching -ExpectedLocal).
$anrCount = -1
$anrMail  = ""
$anrList  = @()
try {
    # Escape the LDAP filter metacharacters; a name is user-supplied text.
    $esc = $displayName -replace '([\\()\*])', '\$1'
    $ds  = New-Object DirectoryServices.DirectorySearcher
    $ds.Filter    = "(&(objectCategory=person)(objectClass=user)(mail=*)(anr=$esc))"
    $ds.SizeLimit = 25
    [void]$ds.PropertiesToLoad.Add("mail")
    [void]$ds.PropertiesToLoad.Add("displayname")
    $hits = @($ds.FindAll())
    $anrCount = $hits.Count
    foreach ($h in $hits) {
        $m = ""; $n = ""
        try { $m = "$($h.Properties['mail'][0])" } catch {}
        try { $n = "$($h.Properties['displayname'][0])" } catch {}
        $anrList += "    $n <$m>"
    }
    if ($anrCount -eq 1) { $anrMail = "$($hits[0].Properties['mail'][0])" }
} catch {
    $anrCount = -1
    Write-Output "WARN directory lookup failed: $($_.Exception.Message)"
}

Write-Output "MATCHES name='$displayName' count=$anrCount"
if ($anrList.Count -gt 0) { $anrList | ForEach-Object { Write-Output $_ } }

# Address the mail by SMTP, never by display name: adding the name back would
# hand the ambiguity straight to Outlook again.
$resolved = $false
$smtp     = ""
$recip    = $null
$dirUser  = $false

if ($anrCount -eq 1 -and $anrMail) {
    $smtp  = $anrMail
    $recip = $mail.Recipients.Add($smtp)
    try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }
    $dirUser = $true   # it came out of the directory by definition
} elseif ($anrCount -lt 0) {
    # No directory to query - an Entra-ID-joined machine with no on-prem AD
    # reachable, which is normal for a cloud-first tenant. Fall back to
    # Outlook's own resolution rather than disabling the feature outright.
    #
    # This is a WEAKER ambiguity check: Recipient.Resolve() reports success for
    # a name several people share. What keeps it safe is that the resolved
    # entry must ALSO be a real Exchange directory user (not a local Contact or
    # a one-off address), be in -Domain, and match -ExpectedLocal. A wrong
    # person clearing all three would have to have the requested user's own
    # name shape, in the org's own directory.
    $recip = $mail.Recipients.Add($displayName)
    try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }
    if ($resolved) {
        try {
            $eu = $recip.AddressEntry.GetExchangeUser()
            if ($null -ne $eu) {
                $smtp    = "$($eu.PrimarySmtpAddress)"
                $dirUser = $true
            }
        } catch {}
        if (-not $dirUser) {
            # A local Contact or a one-off address. Never send to one of these:
            # a stale personal entry for the same name is precisely the way a
            # private key reaches the wrong mailbox.
            try { $smtp = "$($recip.Address)" } catch { $smtp = "" }
        }
    }
}

# The one match must still be in our own domain.
$domainOk = $false
if ($resolved) {
    if (-not $Domain) {
        # No domain configured - check disabled, preserving older behavior.
        $domainOk = $true
    } elseif ($smtp -like "*@*") {
        $addrDomain = ($smtp -split '@')[-1]
        $domainOk = $addrDomain.Trim().ToLower() -eq $Domain.Trim().ToLower()
    }
}

# The address must also LOOK like this person. The domain check cannot catch an
# in-domain address belonging to someone else with a similar name - resolving
# "Test User" to testuser@ when the username is test.user (so tuser@) is exactly
# that. The app derives $ExpectedLocal; an optional numeric suffix is allowed,
# since organizations disambiguate duplicates that way (jsmith, jsmith2).
$localOk = $true
if ($LocalFormat) {
    $localOk = $false
    if (-not $ExpectedLocal) {
        # The format is configured but no stem could be derived (an unknown
        # format name, or a username with no surname). Fail closed.
        Write-Output "WARN could not derive an expected address for '$Username' (LocalFormat='$LocalFormat')"
    } elseif ($smtp -like "*@*") {
        $localPart = ($smtp -split '@')[0]
        $localOk = $localPart.Trim().ToLower() -match ('^' + [regex]::Escape($ExpectedLocal.Trim().ToLower()) + '\d*$')
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
if ($resolved -and $dirUser -and $domainOk -and $localOk -and $encConfirmed) {
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
    if ($anrCount -eq 0) {
        "Nobody in the directory matches '$displayName', so nothing was sent.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif ($anrCount -gt 1) {
        "$anrCount people match '$displayName', so nothing was sent:`n`n" +
        (($anrList -join "`n").Trim()) + "`n`n" +
        "The email is ready below with the To field empty. Pick the correct person, confirm it still shows encrypted, then click Send."
    } elseif (-not $resolved) {
        "Outlook could not identify a single recipient for '$displayName', so nothing was sent.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $dirUser) {
        "'$displayName' matched $smtp, which is not an entry in the company directory - it looks like a local Contact or a saved address.`n`n" +
        "Nothing was sent: that is how a private key reaches the wrong mailbox.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $domainOk) {
        "'$displayName' resolved to $smtp, which is not in $Domain.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $localOk) {
        "'$displayName' resolved to $smtp, but the username '$Username' expects an address like '$ExpectedLocal@$Domain' (an optional number is allowed).`n`n" +
        "That usually means Outlook matched a different person with a similar name. Nothing was sent.`n`n" +
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
Write-Output "OPEN recipient='$displayName' matches=$anrCount resolved=$resolved dir_user=$dirUser domain_ok=$domainOk local_ok=$localOk encrypted=$encConfirmed enc_config=$encConfigured"
