# test_resolve_recipient.ps1 - dry run of the access email's recipient logic.
# Sends NOTHING and creates no draft. Use it to see exactly who a username
# would be mailed, and whether Outlook considers the name ambiguous.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File test_resolve_recipient.ps1 -Username test.user -Domain xyz.com
#
# WHY THIS EXISTS
#   send_access_email.ps1 sends unattended only when Recipient.Resolve() says
#   the name is unambiguous. Outlook's autocomplete dropdown is NOT the same
#   test - it matches substrings, so several entries appearing as you type does
#   not mean the name is ambiguous. This script reports what Resolve() actually
#   does, and separately counts exact display-name matches in the directory so
#   you can tell the two cases apart.

param(
    [string]$Username = "",
    [string]$Domain   = ""
)
$ErrorActionPreference = "Stop"

function Title([string]$s) {
    if ([string]::IsNullOrWhiteSpace($s)) { return $s }
    return $s.Substring(0, 1).ToUpper() + $s.Substring(1).ToLower()
}

if (-not $Username) { $Username = Read-Host "Username (firstname.lastname)" }

$parts = @($Username.Split('.') | Where-Object { $_ -ne "" })
$displayName = ($parts | ForEach-Object { Title $_ }) -join ' '
"Username     : $Username"
"Display name : '$displayName'   <-- this is what Outlook is asked to resolve"
""

# Outlook is only needed for the Resolve() comparison below. The directory
# count - the part that actually decides whether the access email sends - uses
# LDAP and does not need Outlook at all, so a failure here must not stop the
# script.
$ns = $null
try {
    $ol = New-Object -ComObject Outlook.Application
    $ns = $ol.GetNamespace("MAPI")
} catch {
    "Could not attach to Outlook: $($_.Exception.Message)"
    "Classic Outlook must be installed and signed in (the new Outlook has no"
    "COM object model). Skipping the Resolve() comparison - the directory"
    "count below is the part that matters and does not need Outlook."
    ""
}

# --- What Recipient.Resolve() does, for comparison only ------------------
$resolved = $false
$smtp = ""
$kind = "(none)"
if ($null -ne $ns) {
    $recip = $ns.CreateRecipient($displayName)
    try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }
    "Resolve()    : $resolved"
}

if ($resolved) {
    try {
        $eu = $recip.AddressEntry.GetExchangeUser()
        if ($null -ne $eu) {
            $smtp = "$($eu.PrimarySmtpAddress)"
            $kind = "Exchange directory user"
        }
    } catch {}
    if (-not $smtp) {
        try { $smtp = "$($recip.Address)" } catch { $smtp = "" }
        $kind = "NOT a directory user (local Contact, or a one-off address)"
    }
    "Resolved to  : $smtp"
    "Entry type   : $kind"
    try { "Entry name   : $($recip.AddressEntry.Name)" } catch {}

    if ($Domain) {
        $ok = $false
        if ($smtp -like "*@*") {
            $ok = (($smtp -split '@')[-1]).Trim().ToLower() -eq $Domain.Trim().ToLower()
        }
        "In '$Domain' : $ok"
    }
} elseif ($null -ne $ns) {
    "Resolved to  : (nothing - Outlook treated the name as ambiguous or unknown)"
}
""

# --- The gate that actually decides: how many people does the directory
# --- suggest for this name? This is LDAP Ambiguous Name Resolution, the same
# --- resolution behind Outlook's suggestion dropdown.
"Directory matches (LDAP ANR - what Outlook's suggestion list shows):"
$found = @()
$anrCount = -1
try {
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
        $found += "  - $n  <$m>"
    }
} catch {
    "  Could not search the directory: $($_.Exception.Message)"
}

if ($anrCount -lt 0) {
    "  (lookup failed)"
} elseif ($anrCount -eq 0) {
    "  none"
} else {
    $found | ForEach-Object { $_ }
}
""

"VERDICT"
if ($anrCount -lt 0) {
    "  The directory could not be searched - this machine may not be joined to"
    "  the domain, or LDAP may be blocked. The access email FAILS CLOSED here:"
    "  it opens Outlook rather than falling back to a weaker check."
} elseif ($anrCount -gt 1) {
    "  $anrCount people match '$displayName', so the access email will NOT send."
    "  Outlook opens with an empty To field for you to pick. Working as intended."
    if ($resolved) {
        "  (Note Resolve() still returned TRUE here - that is exactly why the"
        "  ambiguity gate uses this directory count instead of Resolve().)"
    }
} elseif ($anrCount -eq 1) {
    "  Exactly one person matches. The extra entries you see while typing in"
    "  Outlook are autocomplete substring matches, not duplicates."
    "  The access email would send to the address listed above, provided it is"
    "  in your email_domain, fits email_local_format, and encryption confirms."
} else {
    "  Nobody in the directory matches '$displayName', so nothing would be sent;"
    "  Outlook opens with an empty To field."
}
