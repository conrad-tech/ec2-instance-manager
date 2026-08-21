# Store the five JSM Ops / Opsgenie values as Windows generic credentials.
# This is the ONLY supported storage location for all five: none of them may
# ever live in assets/features.json (that file is committed to git). See
# src/jsm_auth.rs for the full target <-> env-var table this script writes.
#
# 1. ec2_manager/jsm             - username = Atlassian email, password = API
#                                   token: exactly the pair curl needs for
#                                   `user = "<email>:<token>"`. REQUIRED.
# 2. ec2_manager/jsm_cloud_id     - password = Atlassian cloud id (the UUID in
#                                   the API path). Username is a placeholder.
# 3. ec2_manager/jsm_schedule_id  - password = JSM Ops schedule id, used by the
#                                   reaper on-call lookup. Optional -- leave
#                                   blank if you are not using the reaper.
# 4. ec2_manager/jsm_account_id   - password = YOUR Atlassian account id (e.g.
#                                   5b10ac8d82e05b22cc7d4ef5), used to find
#                                   yourself in the on-call response. NOT an
#                                   AWS account id. Optional, same as above.
# 5. ec2_manager/opsgenie_api_key - password = Opsgenie API key, a different
#                                   auth scheme (GenieKey, not Basic) for the
#                                   Opsgenie-lineage schedule endpoints.
#                                   Optional.
#
# Every value also has an environment-variable override that beats whatever
# is stored here (ATLASSIAN_EMAIL, JIRA_TOKEN, CLOUD_ID, SCHEDULE_ID, MY_ID,
# OPSGENIE_API_KEY) -- see src/jsm_auth.rs.
#
# Read-Host -AsSecureString rather than a cmdkey one-liner: cmdkey /pass:
# puts the value in the process list and in PowerShell history.
#
# None of this is the only supported way to store these: bare
# `cmdkey /generic:<target> /user:<user> /pass` (no value after /pass, so it
# prompts), or the Credential Manager GUI, work too -- wincred.rs reads
# whichever encoding the writer used. One-liner equivalents, if you would
# rather skip this script entirely:
#   cmdkey /generic:ec2_manager/jsm /user:<email> /pass
#   cmdkey /generic:ec2_manager/jsm_cloud_id /user:x /pass
#   cmdkey /generic:ec2_manager/jsm_schedule_id /user:x /pass
#   cmdkey /generic:ec2_manager/jsm_account_id /user:x /pass
#   cmdkey /generic:ec2_manager/opsgenie_api_key /user:x /pass
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class CredMan {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  public struct CREDENTIAL {
    public uint Flags; public uint Type;
    public string TargetName; public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public uint CredentialBlobSize; public IntPtr CredentialBlob;
    public uint Persist; public uint AttributeCount;
    public IntPtr Attributes; public string TargetAlias; public string UserName;
  }
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern bool CredWriteW(ref CREDENTIAL c, uint flags);
  public static void Save(string target, string user, string secret) {
    byte[] blob = System.Text.Encoding.Unicode.GetBytes(secret);
    IntPtr p = Marshal.AllocHGlobal(blob.Length);
    Marshal.Copy(blob, 0, p, blob.Length);
    CREDENTIAL c = new CREDENTIAL();
    c.Type = 1; c.Persist = 2; c.TargetName = target;
    c.UserName = user; c.CredentialBlob = p;
    c.CredentialBlobSize = (uint)blob.Length;
    bool ok = CredWriteW(ref c, 0);
    Marshal.FreeHGlobal(p);
    if (!ok) throw new Exception("CredWrite failed: " + Marshal.GetLastWin32Error());
  }
}
'@

function Read-Secret([string]$prompt) {
  $sec = Read-Host -AsSecureString $prompt
  return [Runtime.InteropServices.Marshal]::PtrToStringUni(
    [Runtime.InteropServices.Marshal]::SecureStringToGlobalAllocUnicode($sec))
}

# 1. Email + token pair (required).
$email = Read-Host 'Atlassian email'
$tok   = Read-Secret 'JSM API token'
[CredMan]::Save('ec2_manager/jsm', $email, $tok)
Write-Host 'Stored as ec2_manager/jsm'

# 2-5. Everything else is optional -- press Enter to skip any of them.
$cloudId = Read-Secret 'Atlassian cloud id (Enter to skip)'
if ($cloudId) {
  [CredMan]::Save('ec2_manager/jsm_cloud_id', 'x', $cloudId)
  Write-Host 'Stored as ec2_manager/jsm_cloud_id'
}

$scheduleId = Read-Secret 'JSM Ops schedule id, for the reaper on-call lookup (Enter to skip)'
if ($scheduleId) {
  [CredMan]::Save('ec2_manager/jsm_schedule_id', 'x', $scheduleId)
  Write-Host 'Stored as ec2_manager/jsm_schedule_id'
}

$accountId = Read-Secret 'Your Atlassian account id, NOT an AWS account id (Enter to skip)'
if ($accountId) {
  [CredMan]::Save('ec2_manager/jsm_account_id', 'x', $accountId)
  Write-Host 'Stored as ec2_manager/jsm_account_id'
}

$genieKey = Read-Secret 'Opsgenie API key (Enter to skip)'
if ($genieKey) {
  [CredMan]::Save('ec2_manager/opsgenie_api_key', 'x', $genieKey)
  Write-Host 'Stored as ec2_manager/opsgenie_api_key'
}

Write-Host 'Done. Verify with: cmdkey /list:ec2_manager/jsm  (and the other targets above)'
