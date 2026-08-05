# test_directory_access.ps1 - can this machine query an on-prem Active
# Directory, and if not, is Outlook's address book usable instead?
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File test_directory_access.ps1
#
# WHY THIS EXISTS
#   The access email counts how many people match a name before it will send
#   unattended. The cheap way is an LDAP ANR query against on-prem AD. That
#   fails on an Entra-ID-only ("Azure AD joined") machine - but so does a
#   domain-joined laptop that simply cannot reach a domain controller, and the
#   two need different fixes. Outlook working proves nothing either way:
#   Outlook talks to Exchange Online over HTTPS, not LDAP to a DC.
#
# Reads only. Sends nothing, changes nothing.

$ErrorActionPreference = "Continue"

function Section([string]$t) { ""; "=== $t ==="; }

Section "1. What kind of join is this?"
"The authoritative answer. DomainJoined=YES means on-prem AD exists for this"
"machine; AzureAdJoined=YES alone means it does not."
try {
    $out = & dsregcmd /status 2>&1
    $keep = $out | Select-String -Pattern 'AzureAdJoined|EnterpriseJoined|DomainJoined|DomainName|TenantName'
    if ($keep) { $keep | ForEach-Object { "  " + $_.ToString().Trim() } }
    else { "  (dsregcmd returned nothing recognizable)" }
} catch {
    "  Could not run dsregcmd: $($_.Exception.Message)"
}

Section "2. Domain environment variables"
"  USERDOMAIN     : $env:USERDOMAIN"
"  USERDNSDOMAIN  : $env:USERDNSDOMAIN   <-- empty is a strong hint there is no on-prem domain"
"  LOGONSERVER    : $env:LOGONSERVER     <-- a DC name here means you logged on against one"
"  COMPUTERNAME   : $env:COMPUTERNAME"

Section "3. Can a domain controller be located?"
try {
    $dc = & nltest /dsgetdc: 2>&1
    $dc | Select-Object -First 6 | ForEach-Object { "  " + $_.ToString().Trim() }
} catch {
    "  Could not run nltest: $($_.Exception.Message)"
}

Section "4. Can we bind to AD at all? (this is what the access email tried)"
$ldapOk = $false
try {
    $root = New-Object DirectoryServices.DirectoryEntry("LDAP://RootDSE")
    $nc   = $root.Properties["defaultNamingContext"].Value
    if ($nc) {
        "  Bound OK. defaultNamingContext = $nc"
        $ldapOk = $true
    } else {
        "  Bound, but no defaultNamingContext came back."
    }
} catch {
    "  FAILED: $($_.Exception.Message)"
}

if ($ldapOk) {
    Section "5. Does an ANR search work?"
    try {
        $ds = New-Object DirectoryServices.DirectorySearcher
        $ds.Filter    = "(&(objectCategory=person)(objectClass=user)(mail=*)(anr=test))"
        $ds.SizeLimit = 5
        [void]$ds.PropertiesToLoad.Add("mail")
        $hits = @($ds.FindAll())
        "  ANR search returned $($hits.Count) result(s) for 'test' (capped at 5)."
        "  => LDAP works. The access email can count duplicate names properly."
    } catch {
        "  FAILED: $($_.Exception.Message)"
        "  => Binding works but searching does not; check permissions."
    }
} else {
    Section "5. Skipped (no LDAP bind)"
    "  => The access email falls back to Outlook's own name resolution, which"
    "     cannot count duplicates. The domain, directory-user and name-format"
    "     checks still apply."
}

Section "6. Is Outlook's address book usable as a fallback?"
try {
    $ol  = New-Object -ComObject Outlook.Application
    $ns  = $ol.GetNamespace("MAPI")
    $gal = $ns.GetGlobalAddressList()
    "  GAL name  : $($gal.Name)"
    $entries = $gal.AddressEntries
    $count = -1
    try { $count = $entries.Count } catch {}
    "  GAL size  : $count entries"
    if ($count -gt 20000) {
        "  NOTE: that is large. Scanning it per send would be far too slow;"
        "        it would only be usable with a targeted lookup, not a sweep."
    } elseif ($count -ge 0) {
        "  A bounded scan of this list is probably viable as a fallback."
    }
} catch {
    "  Could not read the GAL: $($_.Exception.Message)"
}

Section "Summary"
if ($ldapOk) {
    "  LDAP is available. Nothing to change."
} else {
    "  LDAP is NOT available from this machine."
    "  Check section 1: if DomainJoined says NO, this is expected and permanent"
    "  for this machine - there is no on-prem AD to query, and the duplicate-name"
    "  count needs a different source (Microsoft Graph, or the Outlook GAL)."
    "  If DomainJoined says YES, the machine should be able to reach a domain"
    "  controller - check VPN, and rerun this on the corporate network."
}
