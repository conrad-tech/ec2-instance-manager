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

try {
    $ol = New-Object -ComObject Outlook.Application
    $ns = $ol.GetNamespace("MAPI")
} catch {
    "Could not attach to Outlook: $($_.Exception.Message)"
    "Classic Outlook must be running (the new Outlook has no COM object model)."
    return
}

# --- What send_access_email.ps1 would do ---------------------------------
$recip = $ns.CreateRecipient($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }
"Resolve()    : $resolved"

$smtp = ""
$kind = "(none)"
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
} else {
    "Resolved to  : (nothing - Outlook treated the name as ambiguous or unknown)"
}
""

# --- How many people REALLY share that exact display name? ---------------
# Autocomplete matches substrings, so seeing several entries while typing does
# not mean the name is ambiguous. This counts EXACT display-name matches only.
# Stops at 5 so a large directory does not take forever.
"Exact display-name matches in the Global Address List:"
$found = @()
try {
    $gal = $ns.GetGlobalAddressList().AddressEntries
    $total = $gal.Count
    "  (scanning $total entries - this can take a while on a big directory)"
    for ($i = 1; $i -le $total; $i++) {
        $e = $gal.Item($i)
        if ("$($e.Name)".Trim() -eq $displayName) {
            $addr = ""
            try { $addr = "$($e.GetExchangeUser().PrimarySmtpAddress)" } catch { $addr = "$($e.Address)" }
            $found += "  - $($e.Name)  <$addr>"
            if ($found.Count -ge 5) { break }
        }
    }
} catch {
    "  Could not scan the address list: $($_.Exception.Message)"
}

if ($found.Count -eq 0) {
    "  none"
} else {
    $found | ForEach-Object { $_ }
}
""

"VERDICT"
if ($found.Count -gt 1) {
    "  $($found.Count)+ people share the exact name '$displayName'."
    if ($resolved) {
        "  Resolve() still returned TRUE, so the access email WOULD send to"
        "  $smtp without asking. That is a real ambiguity the guard misses -"
        "  report this, it needs fixing."
    } else {
        "  Resolve() returned FALSE, so the access email would open Outlook"
        "  with an empty To field. Working as intended."
    }
} elseif ($found.Count -eq 1) {
    "  Exactly one person has that name. The entries you see while typing are"
    "  substring matches from autocomplete, not duplicates. Sending to $smtp"
    "  is correct."
} else {
    "  Nobody in the directory has that exact display name."
    if ($resolved) {
        "  Resolve() still returned TRUE, so it matched a local Contact or the"
        "  autocomplete cache rather than the directory - see 'Entry type' above."
    } else {
        "  The access email would open Outlook with an empty To field."
    }
}
