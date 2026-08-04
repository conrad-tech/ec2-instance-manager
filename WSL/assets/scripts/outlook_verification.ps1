# outlook_verification.ps1 - one-off diagnostic to discover exactly what your
# Outlook "Encrypt" button applies, so the access email can replicate it.
#
# HOW TO USE
#   1. Outlook -> New Email -> apply Encrypt the way you normally do (Alt+6).
#      Leave that draft OPEN (do not send it).
#   2. Switch to PowerShell and run this script.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File outlook_verification.ps1
#
# It finds the draft whether you composed in a pop-out window OR inline in the
# reading pane, and falls back to the newest item in Drafts. If everything comes
# back blank, the "WHAT THIS MEANS" block at the end tells you which case you
# are in and what to put in features.json.

$ErrorActionPreference = "Stop"

$SEC_PROP   = "http://schemas.microsoft.com/mapi/proptag/0x6E010003"
$LABEL_PROP = "http://schemas.microsoft.com/mapi/string/{00020386-0000-0000-C000-000000000046}/msip_labels"

function Get-Prop($item, [string]$schema) {
    try { return $item.PropertyAccessor.GetProperty($schema) } catch { return $null }
}

try {
    $ol = New-Object -ComObject Outlook.Application
} catch {
    "Could not attach to Outlook: $($_.Exception.Message)"
    ""
    "Classic Outlook must be running. The 'new Outlook' for Windows has no COM"
    "object model - switch it off (toggle 'New Outlook' in the top-right) and"
    "rerun, or run this on a machine with classic Outlook."
    return
}

# --- Find the draft: pop-out window, then inline compose, then Drafts ---------
$item   = $null
$source = ""

$insp = $ol.ActiveInspector()
if ($null -ne $insp) {
    try { $item = $insp.CurrentItem; $source = "pop-out compose window" } catch {}
}

if ($null -eq $item) {
    try {
        $expl = $ol.ActiveExplorer()
        if ($null -ne $expl) {
            $inline = $expl.ActiveInlineResponse
            if ($null -ne $inline) { $item = $inline; $source = "inline compose (reading pane)" }
        }
    } catch {}
}

if ($null -eq $item) {
    try {
        $drafts = $ol.Session.GetDefaultFolder(16)   # olFolderDrafts
        $items  = $drafts.Items
        $items.Sort("[LastModificationTime]", $true)
        if ($items.Count -gt 0) {
            $item   = $items.Item(1)
            $source = "newest item in Drafts ('" + $item.Subject + "')"
        }
    } catch {}
}

if ($null -eq $item) {
    "No draft found (no compose window, no inline compose, no drafts)."
    ""
    "Open a New Email, apply Encrypt, leave it open, and rerun. If you composed"
    "in the reading pane, that is fine - this script reads that too. If you sent"
    "or discarded the draft, make a new one."
    return
}

"Read from            : $source"
""

# --- What the Encrypt button actually set ------------------------------------
$perm     = 0
$permSvc  = 0
$guid     = ""
try { $perm    = [int]$item.Permission } catch {}
try { $permSvc = [int]$item.PermissionService } catch {}
try { $guid    = "$($item.PermissionTemplateGuid)" } catch {}

$smime = Get-Prop $item $SEC_PROP
$label = Get-Prop $item $LABEL_PROP

"Permission             : $perm"
"PermissionService      : $permSvc"
"PermissionTemplateGuid : '$guid'"
if ($null -ne $smime) { "S/MIME security flag   : $smime" } else { "S/MIME security flag   : (not set)" }
if ($label) {
    "Sensitivity label      : set"
    "  $label"
} else {
    "Sensitivity label      : (not set)"
}
""

# --- Tell the user which case they are in ------------------------------------
"WHAT THIS MEANS"
if ($guid) {
    "  Tenant RMS/IRM template - the good case. In assets/features.json set:"
    "    `"encrypt_template_guid`": `"$guid`","
    if ($perm -ne 0) { "    `"encrypt_permission`": $perm," } else { "    `"encrypt_permission`": 4," }
    "    `"encrypt_permission_service`": 1"
} elseif ($perm -eq 2) {
    "  Do Not Forward (no template). In assets/features.json set:"
    "    `"encrypt_template_guid`": `"`", `"encrypt_permission`": 2"
} elseif ($smime -eq 1) {
    "  S/MIME. In assets/features.json set:"
    "    `"encrypt_template_guid`": `"`", `"encrypt_permission`": 0, `"encrypt_smime_flag`": 1"
} elseif ($label) {
    "  Purview sensitivity label / OME 'Encrypt-Only'. These do NOT expose a"
    "  template GUID, so there is nothing to paste and headless encryption"
    "  cannot be set from the object model. Use the visible path - in"
    "  assets/features.json set:"
    "    `"encrypt_template_guid`": `"`", `"encrypt_permission`": 0"
    "  Every access email then opens in Outlook with your Encrypt shortcut"
    "  applied (encrypt_sendkeys, default Alt+6) for a one-click Send."
} else {
    "  Nothing encryption-related is set on this draft."
    ""
    "  Either the Encrypt did not actually apply, or it is a label that Outlook"
    "  only stamps on save/send. Try: click in the body, type a character, press"
    "  Ctrl+S, then rerun this script. If it is still blank, use the visible"
    "  path - in assets/features.json set:"
    "    `"encrypt_template_guid`": `"`", `"encrypt_permission`": 0"
}
