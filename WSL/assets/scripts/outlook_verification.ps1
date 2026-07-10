# outlook_verification.ps1 - one-off diagnostic to discover exactly what your
# Outlook "Encrypt" button applies, so headless auto-send can replicate it.
#
# HOW TO USE
#   1. Outlook -> New Email -> apply Encrypt the way you normally do (Alt+6).
#      Leave that compose window OPEN (do not send it).
#   2. Switch to PowerShell (the compose window can go behind - that's fine;
#      ActiveInspector() is Outlook's internal active window, not the OS
#      foreground window).
#   3. Run this script.
#
# READING THE RESULT
#   Permission = 2                              -> Do Not Forward   (set headless via $mail.Permission = 2)
#   Permission = 4  + a GUID                    -> template/Encrypt-Only (set via PermissionTemplateGuid + Permission = 4)
#   Permission = 0, no GUID, S/MIME flag = 1    -> S/MIME (set via PropertyAccessor 0x6E010003 = 1)
#   Permission = 0, no GUID, S/MIME not set     -> sensitivity-label OME (not cleanly settable headless)

$ol   = New-Object -ComObject Outlook.Application
$insp = $ol.ActiveInspector()
if ($null -eq $insp) {
    "No active compose window found. Open your encrypted draft, click it once to focus, then rerun."
} else {
    $item = $insp.CurrentItem
    "Permission             : {0}" -f $item.Permission
    "PermissionTemplateGuid : '{0}'" -f $item.PermissionTemplateGuid
    try {
        $flag = $item.PropertyAccessor.GetProperty("http://schemas.microsoft.com/mapi/proptag/0x6E010003")
        "S/MIME security flag   : $flag"
    } catch { "S/MIME security flag   : (not set)" }
}
