# Store the JSM Ops email + API token as one Windows generic credential.
# Username = Atlassian email, password = API token: exactly the pair curl
# needs for `user = "<email>:<token>"`.
#
# Read-Host -AsSecureString rather than a cmdkey one-liner: cmdkey /pass:
# puts the token in the process list and in PowerShell history.
#
# This script is not the only supported way to store it: bare
# `cmdkey /generic:ec2_manager/jsm /user:<email> /pass` (no value after
# /pass, so it prompts) or the Credential Manager GUI work too — wincred.rs
# reads whichever encoding the writer used.
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

$email = Read-Host 'Atlassian email'
$sec   = Read-Host -AsSecureString 'JSM API token'
$tok   = [Runtime.InteropServices.Marshal]::PtrToStringUni(
           [Runtime.InteropServices.Marshal]::SecureStringToGlobalAllocUnicode($sec))
[CredMan]::Save('ec2_manager/jsm', $email, $tok)
Write-Host 'Stored as ec2_manager/jsm — verify with: cmdkey /list:ec2_manager/jsm'
