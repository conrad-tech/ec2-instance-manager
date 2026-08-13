<#
.SYNOPSIS
    Optional automatic Okta sign-in for the `fed up` credential refresh.

.DESCRIPTION
    Only runs when fed_auth.auto_sign_in is true in features.json. With it
    off -- the shipped default -- the app opens the activation page, copies the
    code, and this script is never invoked.

    Opens Chrome on the Okta activation URL, types the device code, and walks
    the username / password / MFA-confirm pages.

    WHERE THE PASSWORD LIVES
    ------------------------
    In Windows Credential Manager, and nowhere else. This script only ever
    READS it; neither it nor the app can write, test for, or delete it, and
    the app never sees the plaintext at all. The vault encrypts it and binds
    it to the Windows account that stored it: another user, or the same file
    copied to another machine, cannot read it back.

    Set it yourself, either way round:

        cmdkey /generic:ec2-manager-fed /user:you /pass
        Control Panel -> Credential Manager -> Windows Credentials
          -> "Add a generic credential"

    It must be a GENERIC credential, not a "Windows credential": CredRead is
    called with CRED_TYPE_GENERIC and will not see one stored under the other
    type. `cmdkey /generic:` gets this right; the Control Panel offers both
    links on the same page.

    The username recorded alongside it (the /user: value) is what gets typed
    on the Okta username page.

    What no scheme can do is stop code running as *you* from reading it --
    anything the sign-in can decrypt unattended, malware running as you can
    too.

    WHY THIS IS OPT-IN
    ------------------
    Login mode types a domain password into an identity provider and confirms
    an MFA prompt. Endpoint protection scores that as credential theft, and
    the app shipping this script has a CrowdStrike quarantine in its history.
    It needs fed_auth.enabled, a named user in fed_auth.allowed_users, AND
    fed_auth.auto_sign_in -- three separate opt-ins.

    THE FOCUS GUARD IS THE SAFETY PROPERTY
    --------------------------------------
    SendKeys types into whatever window is frontmost. If focus moves between
    the wait and the keystroke, a domain password lands wherever it went -- a
    chat window, a shared screen, a terminal that logs. So every send is
    preceded by Assert-Target, which requires the foreground window to belong
    to chrome.exe AND its title to contain -TitleMatch. On a mismatch the
    script aborts rather than typing. Do not "simplify" that away.

    EVERY FIELD IS CLEARED BEFORE IT IS TYPED
    -----------------------------------------
    SendKeys cannot read the DOM, so it cannot tell whether a field is already
    populated. Okta prefills the username most of the time and Chrome may fill
    the password box, and typing into either without clearing first appends
    rather than replaces. Send-FieldText sends Ctrl+A ahead of the text; on an
    empty field that selects nothing and costs nothing.

    THE ACTIVATION BOX IS FOCUSED AND READ BACK
    -------------------------------------------
    SendKeys types into whatever holds keyboard focus. Okta does not reliably
    focus the activation-code box, and a run that typed into the page body
    still walked every remaining step and reported success -- a "done" with
    nothing signed in, which is worse than an error.

    Three things now stand against that, weakest first. The code is passed as
    ?user_code= so the page can prefill it (RFC 8628's "complete" verification
    URI, which Okta honours -- and which costs nothing where it is ignored);
    the box is focused through the UI Automation tree, where Chrome publishes
    the same <input> the DevTools inspector shows; and the field is READ BACK
    before Enter, failing loudly when it is empty.

    The lookup is scoped to the page Document, so the omnibox and the find bar
    -- Edit controls both -- can never be the thing that gets focused. Within
    it, a field whose accessible name matches -CodeFieldMatch wins, and
    otherwise the first Edit in the page is taken. Both halves are needed:
    the name comes from the <label>, which Okta leaves unassociated often
    enough that it can be blank, while the DOM id it publishes as AutomationId
    ("input28") is regenerated per load and name="userCode" is not published
    at all. The activation page has exactly one box, which makes the
    positional fallback unambiguous there.

    Reading the tree is best-effort throughout: unavailable UIA, an unbuilt
    tree or a differently-worded label all fall back to typing blind, which is
    what happened before regardless. Only a field that reads back EMPTY is
    treated as failure; a field that cannot be read is not evidence.

.NOTES
    Run from beside the executable, never copied to %TEMP% and run from there,
    and never with -WindowStyle Hidden. Both are patterns EDRs quarantine on
    sight; see the access-email notes in CLAUDE.md.
#>

[CmdletBinding()]
param(
    # Credential Manager target name holding the federation password. Read
    # only -- this script never writes to the vault.
    [string]$CredentialTarget = 'ec2-manager-fed',

    # The activation URL from `fed up`.
    [string]$Url = '',

    # The device code from `fed up`.
    [string]$Code = '',

    # Typed over the username field (cleared first, so a prefill is replaced
    # rather than appended to). Empty reads it from the vault entry.
    [string]$Username = '',

    # Process names accepted at the MFA confirm step only, pipe-separated and
    # matched as an unanchored, case-insensitive regex.
    #
    # Okta Verify is its OWN window -- borderless and centred, so it looks
    # like a Chrome popup, but it is a separate process and the chrome-only
    # guard refuses it. That step types no secret (a bare Enter), so widening
    # it there is safe in a way it would not be for the password.
    [string]$MfaProcessMatch = 'chrome|okta',

    # Foreground-window title fragments accepted before anything is typed,
    # pipe-separated; any one matching is enough. A list rather than one
    # value because the sign-in walks several pages that do not share a
    # title.
    [string]$TitleMatch = 'okta|sign in|verify',

    # Seconds the focus guard will wait for a matching window before giving
    # up. Sampling a single instant made every page transition a race.
    [int]$GuardWaitSec = 10,

    # Arguments passed to Chrome ahead of the URL, space-separated.
    #
    # --new-window is the default and it matters. With Chrome already
    # running, a bare `chrome.exe <url>` opens a TAB in whatever window
    # exists -- which may be minimised, behind other windows, or on another
    # virtual desktop. The wait below then times out on a page that loaded
    # perfectly well, just nowhere it could be typed into. That is the
    # "sometimes it opens visibly, sometimes it does not" case. A new window
    # is created foreground.
    #
    # A separate profile (--user-data-dir=...) is deliberately NOT the
    # default. It would guarantee a clean window, but remembered-device
    # cookies live in the profile, so signing in from a fresh one tends to
    # invite MORE verification, not less.
    [string]$ChromeArgs = '--new-window',

    # Explicit chrome.exe path. Empty searches the usual install locations.
    [string]$ChromePath = '',

    # Accessible name of the activation-code box, matched as an unanchored
    # case-insensitive regex. Chrome computes that name from the field's
    # <label>, so this is the visible "Activation Code" text -- NOT the DOM
    # id, which Okta generates fresh each load ("input28") and which would
    # therefore match nothing on the next visit.
    [string]$CodeFieldMatch = 'activation.?code|usercode',

    # Accessible name of the username box, same idea as -CodeFieldMatch.
    [string]$UserFieldMatch = 'user.?name|^user$|email',

    # Accessible name of the password box. Focused but never read back.
    [string]$PasswordFieldMatch = 'password',

    # Seconds to let a page settle before touching its fields. A window can
    # be foreground and correctly titled while the document is still laying
    # out, and focusing a field that is about to be replaced loses it again.
    [int]$SettleSec = 2,

    # Seconds to wait for the activation page to appear.
    [int]$PageTimeoutSec = 60,

    # Seconds to allow between page transitions.
    [int]$StepDelaySec = 3,

    # Walk every step and run every guard, but send no keystrokes. Use this to
    # check timing and the focus guard without the password going anywhere.
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Markers the GUI parses out of stdout. Keep these stable.
function Write-Status([string]$state) { Write-Output "FEDLOGIN_STATUS:$state" }
function Write-Fail([string]$msg) {
    Write-Output "FEDLOGIN_ERROR:$msg"
    exit 1
}

# ------------------------------------------------- Credential Manager -------

Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class Cred {
    private const uint GENERIC = 1;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct CREDENTIAL {
        public uint Flags;
        public uint Type;
        public IntPtr TargetName;
        public IntPtr Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public IntPtr TargetAlias;
        public IntPtr UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CredRead(string target, uint type, uint flags, out IntPtr cred);

    [DllImport("advapi32.dll", EntryPoint = "CredFree")]
    private static extern void CredFree(IntPtr buffer);

    /// Returns the stored password, or null when there is no such credential.
    public static string Read(string target) {
        IntPtr raw;
        if (!CredRead(target, GENERIC, 0, out raw)) { return null; }
        try {
            CREDENTIAL c = (CREDENTIAL)Marshal.PtrToStructure(raw, typeof(CREDENTIAL));
            if (c.CredentialBlob == IntPtr.Zero || c.CredentialBlobSize == 0) { return null; }
            // The blob is UTF-16 and is NOT null-terminated, so the length has
            // to come from CredentialBlobSize rather than PtrToStringUni's own
            // scan.
            return Marshal.PtrToStringUni(c.CredentialBlob, (int)(c.CredentialBlobSize / 2));
        } finally {
            CredFree(raw);
        }
    }

    /// The username recorded alongside the password, or null.
    public static string ReadUser(string target) {
        IntPtr raw;
        if (!CredRead(target, GENERIC, 0, out raw)) { return null; }
        try {
            CREDENTIAL c = (CREDENTIAL)Marshal.PtrToStructure(raw, typeof(CREDENTIAL));
            if (c.UserName == IntPtr.Zero) { return null; }
            return Marshal.PtrToStringUni(c.UserName);
        } finally {
            CredFree(raw);
        }
    }

}
'@

if ([string]::IsNullOrWhiteSpace($CredentialTarget)) {
    Write-Fail 'no -CredentialTarget given'
}

# ------------------------------------------------------------------ Login ---

if ([string]::IsNullOrWhiteSpace($Url)) { Write-Fail 'no -Url given' }
if ([string]::IsNullOrWhiteSpace($Code)) { Write-Fail 'no -Code given' }
if ([string]::IsNullOrWhiteSpace($TitleMatch)) {
    Write-Fail 'no -TitleMatch given; refusing to type without a focus guard'
}

Add-Type -AssemblyName System.Windows.Forms

Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class Fg {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")]
    public static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
    public static string Title() {
        IntPtr h = GetForegroundWindow();
        if (h == IntPtr.Zero) return "";
        int n = GetWindowTextLength(h);
        if (n <= 0) return "";
        StringBuilder sb = new StringBuilder(n + 1);
        GetWindowText(h, sb, sb.Capacity);
        return sb.ToString();
    }
    public static IntPtr Handle() { return GetForegroundWindow(); }
    public static long Handle() {
        return (long)GetForegroundWindow();
    }
    public static int Pid() {
        IntPtr h = GetForegroundWindow();
        if (h == IntPtr.Zero) return 0;
        int pid; GetWindowThreadProcessId(h, out pid); return pid;
    }
}
'@

function Get-ForegroundProcessName {
    $procId = [Fg]::Pid()
    if ($procId -eq 0) { return '' }
    try { return (Get-Process -Id $procId -ErrorAction Stop).ProcessName }
    catch { return '' }
}

# Any one of the -TitleMatch fragments matching is enough.
function Test-TitleMatch([string]$title) {
    foreach ($frag in ($TitleMatch -split '\|')) {
        $f = $frag.Trim()
        if ($f -and $title -like "*$f*") { return $true }
    }
    return $false
}

<#
    The guard. Both halves matter: the title alone would pass for a lookalike
    page or a renamed window in another app, and the process alone would pass
    for any Chrome tab, including whatever the user just switched to.

    It WAITS for a match rather than sampling one instant. Each step of the
    sign-in submits a form and the next page takes a moment to load and
    retitle, so a single sample turns every transition into a race -- which is
    exactly how the MFA step failed: the title had moved on from the sign-in
    page and the guard refused before the page had settled.
#>
function Assert-Target([string]$what, [string]$procPattern = '^(?i)chrome$') {
    $deadline = (Get-Date).AddSeconds($GuardWaitSec)
    $title = ''
    $proc = ''
    while ((Get-Date) -lt $deadline) {
        $title = [Fg]::Title()
        $proc = Get-ForegroundProcessName
        if ($proc -match $procPattern -and (Test-TitleMatch $title)) { return }
        Start-Sleep -Milliseconds 250
    }
    # Report whichever half is actually wrong, and name the value that needs
    # configuring -- both halves are things a tenant can differ on.
    if ($proc -notmatch $procPattern) {
        Write-Fail "focus guard: foreground window belongs to '$proc', which does not match '$procPattern' -- aborted before $what"
    }
    Write-Fail "focus guard: foreground title '$title' matches none of '$TitleMatch' -- aborted before $what (add a fragment of that title to fed_auth.browser_title_match)"
}

function Wait-ForTarget([int]$timeoutSec) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $title = [Fg]::Title()
        $proc = Get-ForegroundProcessName
        if ($proc -match '^(?i)chrome$' -and (Test-TitleMatch $title)) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

# SendKeys reads +^%~(){}[] as control characters, so every one of them in a
# literal string has to be braced. A password containing '(' would otherwise
# be silently mistyped.
function ConvertTo-SendKeysLiteral([string]$s) {
    $sb = New-Object System.Text.StringBuilder
    foreach ($ch in $s.ToCharArray()) {
        if ('+^%~(){}[]'.Contains($ch)) {
            [void]$sb.Append('{').Append($ch).Append('}')
        } else {
            [void]$sb.Append($ch)
        }
    }
    return $sb.ToString()
}

function Send-Guarded([string]$keys, [string]$what, [string]$procPattern = '^(?i)chrome$') {
    Assert-Target $what $procPattern
    if ($DryRun) {
        Write-Output "FEDLOGIN_DRYRUN:would send $what"
        return
    }
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}

# Type into a field that may already have something in it -- see the header.
function Send-FieldText([string]$text, [string]$what) {
    Send-Guarded '^a' "clearing $what"
    Send-Guarded (ConvertTo-SendKeysLiteral $text) $what
}

# --- Reading the page ------------------------------------------------------
#
# SendKeys types into whatever holds keyboard focus, and cannot tell whether
# anything received it. That is how a run reported "done" having entered
# nothing: Okta did not autofocus the activation box, every keystroke went to
# the page body, and the script pressed on regardless.
#
# The accessibility tree is the way out. Chrome publishes the same <input> to
# UI Automation, where it can be focused deliberately and -- the point -- read
# back afterwards. This does not replace the focus guard: UIA says which
# control to type into, Assert-Target still decides whether typing is allowed
# at all.
#
# Every function here is best-effort and returns $null on any failure. Chrome
# may not have its accessibility tree built yet, UIA may be unavailable, and a
# tenant may word the label differently -- none of which should be the reason
# a sign-in fails, since blind typing is exactly what happens today.
$script:UiaReady = $false
try {
    Add-Type -AssemblyName UIAutomationClient -ErrorAction Stop
    Add-Type -AssemblyName UIAutomationTypes -ErrorAction Stop
    $script:UiaReady = $true
} catch {
    Write-Output "FEDLOGIN_NOTE:accessibility unavailable ($($_.Exception.Message)); typing blind"
}

# The activation-code box, as Chrome publishes it to UI Automation.
#
# Scoped to the page Document, never the whole window. The omnibox and the
# find bar are Edit controls too, and focusing one of those would type the
# activation code into the address bar. Everything inside the Document is
# page content, which is what the selector in the DevTools inspector refers
# to.
#
# Matching is by accessible name first and position second, because neither
# alone is reliable:
#
#   - The name comes from the field's <label>. Okta's markup leaves aria-label
#     empty, so the name exists only if that label is associated with the
#     input; when it is not, the name is blank and no pattern can match it.
#   - The DOM id ("input28") IS published, as AutomationId -- but Okta
#     regenerates it per page load, so it is worthless across visits, and
#     name="userCode" is not published by UIA at all.
#
# So: prefer a named match, and otherwise take the first Edit in the page.
# The activation page has exactly one box, which makes that unambiguous
# there; the named match is what keeps it honest on the busier pages.
function Get-PageDocument {
    if (-not $script:UiaReady) { return $null }
    try {
        $h = [Fg]::Handle()
        if ($h -eq [IntPtr]::Zero) { return $null }
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
        if ($null -eq $root) { return $null }
        $cond = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Document)
        return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    } catch {
        return $null
    }
}

function Get-WebField([string]$namePattern) {
    if (-not $script:UiaReady) { return $null }
    try {
        $doc = Get-PageDocument
        if ($null -eq $doc) { return $null }
        $cond = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Edit)
        $found = $doc.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($null -eq $found -or $found.Count -eq 0) { return $null }

        $first = $null
        foreach ($el in $found) {
            $name = ''
            try { $name = $el.Current.Name } catch { $name = '' }
            if ($namePattern -and $name -and ($name -match $namePattern)) { return $el }
            if ($null -eq $first) { $first = $el }
        }
        # Nothing named like the code box. On a single-field page that is the
        # unlabelled activation box; on a page with several it is the first,
        # which is the one a fresh page focuses anyway.
        return $first
    } catch {
        return $null
    }
}

# Wait for the field to exist, then give it keyboard focus. Returns $true only
# when focus was actually set, so the caller can say what it did.
function Focus-WebField([string]$namePattern, [int]$timeoutSec = 10) {
    if (-not $script:UiaReady) { return $false }
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $el = Get-WebField $namePattern
        if ($null -ne $el) {
            try {
                $el.SetFocus()
                return $true
            } catch {
                # Focusable only while the window is foreground; the guard
                # below will catch a window that moved.
                return $false
            }
        }
        Start-Sleep -Milliseconds 400
    }
    return $false
}

# What the field currently contains, or $null when it cannot be read.
#
# $null and '' are deliberately different: empty means "read it, it is empty"
# (a real failure), while $null means "could not read it" (fall back to the
# old blind behaviour rather than inventing a verdict).
function Get-WebFieldValue([string]$namePattern) {
    $el = Get-WebField $namePattern
    if ($null -eq $el) { return $null }
    try {
        $pattern = $el.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        return $pattern.Current.Value
    } catch {
        return $null
    }
}

<#
    Which element currently has keyboard focus, desktop-wide.

    This is how a click outside the box becomes visible: the moment focus
    moves, this returns something else. UI Automation also raises a
    focus-changed *event*, but a synchronous script gets the same protection
    by reading this immediately before each send -- and without an event
    handler that could fire on another thread mid-keystroke.

    $null means "cannot tell", which is never treated as failure.
#>
function Get-FocusedElement {
    if (-not $script:UiaReady) { return $null }
    try { return [System.Windows.Automation.AutomationElement]::FocusedElement }
    catch { return $null }
}

<#
    Is the focused element the field we mean? $true / $false / $null for
    "cannot tell".

    Matched on name and automation id, the same two properties Get-WebField
    searches, so a pattern that finds a field also recognises it here.
#>
function Test-FieldFocused([string]$namePattern) {
    $el = Get-FocusedElement
    if ($null -eq $el) { return $null }
    try {
        $name = [string]$el.Current.Name
        $id = [string]$el.Current.AutomationId
        return (($name -match $namePattern) -or ($id -match $namePattern))
    } catch {
        return $null
    }
}

<#
    Put $text into the field matching $namePattern, and keep at it.

    Focus is the whole problem: the box is not reliably focused when a page
    settles, and a stray click while the sequence runs takes it away again.
    So rather than typing once and hoping, each attempt re-focuses through
    the accessibility tree, clears whatever is there, types, and reads the
    value back. A miss costs one more attempt instead of the whole run.

    Returns $true when the field reads back as $text, $false when it does
    not, and $null when the value cannot be read at all (a password box
    reports empty by design, so "cannot read" must never be treated as
    failure).
#>
function Set-FieldWithRetry(
    [string]$namePattern,
    [string]$text,
    [string]$what,
    [int]$attempts = 3
) {
    for ($i = 1; $i -le $attempts; $i++) {
        [void](Focus-WebField $namePattern $GuardWaitSec)
        # Focus-WebField reports whether SetFocus threw, not whether focus
        # actually landed -- a click elsewhere between the two is exactly the
        # case we are guarding. Ask the desktop who really has it.
        $focused = Test-FieldFocused $namePattern
        if ($focused -eq $false) {
            Write-Output "FEDLOGIN_NOTE:$what lost focus before typing (attempt $i); retrying"
            Start-Sleep -Milliseconds 600
            continue
        }
        Send-FieldText $text $what

        $seen = Get-WebFieldValue $namePattern
        if ($null -eq $seen) {
            # Unreadable: no evidence either way, so accept it rather than
            # retyping into a field that may already be correct.
            return $null
        }
        if ($seen.Trim() -eq $text.Trim()) {
            return $true
        }
        Write-Output "FEDLOGIN_NOTE:$what did not take (attempt $i of $attempts); retrying"
        Start-Sleep -Milliseconds 600
    }
    return $false
}

function Resolve-Chrome {
    if (-not [string]::IsNullOrWhiteSpace($ChromePath)) {
        if (Test-Path $ChromePath) { return $ChromePath }
        Write-Fail "chrome not found at -ChromePath '$ChromePath'"
    }
    $candidates = @(
        "$env:ProgramFiles\Google\Chrome\Application\chrome.exe",
        "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
    )
    foreach ($c in $candidates) { if (Test-Path $c) { return $c } }
    Write-Fail 'chrome.exe not found; set the chrome path in features.json'
}

# The vault is read up front: with nothing stored there is no point opening a
# browser the user then has to finish in anyway. Failing here lets the app
# fall back to its open-page-and-copy-code path with the code still fresh.
$plain = [Cred]::Read($CredentialTarget)
if ([string]::IsNullOrEmpty($plain)) {
    Write-Fail "no password in Credential Manager under '$CredentialTarget'. Set one with: cmdkey /generic:$CredentialTarget /user:<your-okta-user> /pass  (it must be a GENERIC credential)"
}

# The username is stored with the password, so the one entered in the app's
# password dialog is the one typed on the Okta page. -Username overrides it;
# %USERNAME% is the last resort.
if ([string]::IsNullOrWhiteSpace($Username)) {
    $Username = [Cred]::ReadUser($CredentialTarget)
}
if ([string]::IsNullOrWhiteSpace($Username)) {
    $Username = $env:USERNAME
}
Write-Status "username-$Username"

$chrome = Resolve-Chrome
Write-Status 'opening-browser'

# Ask Okta to prefill the code rather than typing it. The device-activation
# page accepts it as a query parameter, and a field that arrives already
# filled cannot be missed by a focus problem. It is additive: a tenant that
# ignores the parameter just shows the empty box, and the typing below runs
# as before.
$openUrl = $Url
if (-not [string]::IsNullOrWhiteSpace($Code) -and $Url -notmatch '[?&]user_code=') {
    $sep = '?'
    if ($Url.Contains('?')) { $sep = '&' }
    $openUrl = "$Url$sep" + 'user_code=' + [uri]::EscapeDataString($Code)
}
$chromeArgv = @()
foreach ($a in ($ChromeArgs -split '\s+')) {
    if (-not [string]::IsNullOrWhiteSpace($a)) { $chromeArgv += $a }
}
$chromeArgv += $openUrl
Start-Process -FilePath $chrome -ArgumentList $chromeArgv | Out-Null

if (-not (Wait-ForTarget $PageTimeoutSec)) {
    Write-Fail "the activation page did not reach the foreground within ${PageTimeoutSec}s (looking for a chrome window whose title matches one of '$TitleMatch')"
}

# Tell the app which window this is, so it can close that one -- and only
# that one -- when the credentials come back renewed. A handle, not a pid:
# with --new-window on an already-running Chrome the process is shared with
# the rest of the user's browsing, and killing it would take their tabs with
# it.
Write-Output "FEDLOGIN_HWND:$([Fg]::Handle())"

# Let the page finish settling before touching it. A window can be foreground
# and titled correctly while the document is still laying out, and focusing a
# field that is about to be replaced just loses it again.
Start-Sleep -Seconds $SettleSec

# --- Activation code -------------------------------------------------------
#
# The box is not reliably focused when the page settles -- that is the whole
# reason this step used to type into nothing and still report success. Focus
# it deliberately where the accessibility tree allows, and either way check
# afterwards that something is actually in it.
Write-Status 'entering-code'
$prefilled = Get-WebFieldValue $CodeFieldMatch
if ($prefilled -eq $Code) {
    # The URL prefill worked; typing would only risk appending to it.
    Write-Output 'FEDLOGIN_NOTE:code was prefilled from the URL'
} elseif (-not $DryRun) {
    $ok = Set-FieldWithRetry $CodeFieldMatch $Code 'the activation code'
    if ($ok -eq $false) {
        Write-Fail "the activation code box would not take the code after several tries -- the page kept losing keyboard focus. Nothing was signed in. If the box is labelled something other than 'Activation Code' on this tenant, pass -CodeFieldMatch to match it."
    }
} else {
    Send-FieldText $Code 'the activation code'
}

# Read it back before submitting. An empty box here means every keystroke went
# somewhere else, and pressing Enter on it walks the rest of the script
# through pages that never appear -- ending, as it did, with a cheerful
# "done" and no sign-in. $null is "could not read", which is not evidence of
# failure and must not fail the run.
Send-Guarded '{ENTER}' 'submitting the activation code'
Start-Sleep -Seconds $StepDelaySec

# --- Username --------------------------------------------------------------
Write-Status 'entering-username'
Start-Sleep -Seconds $SettleSec
if (-not [string]::IsNullOrWhiteSpace($Username)) {
    if ($DryRun) {
        Send-FieldText $Username 'the username'
    } else {
        $ok = Set-FieldWithRetry $UserFieldMatch $Username 'the username'
        if ($ok -eq $false) {
            Write-Fail "the username box would not take the value after several tries -- the page kept losing keyboard focus. Nothing was signed in. If it is labelled something else on this tenant, pass -UserFieldMatch to match it."
        }
    }
}
Send-Guarded '{ENTER}' 'submitting the username'
Start-Sleep -Seconds $StepDelaySec

# --- Password --------------------------------------------------------------
Write-Status 'entering-password'
Start-Sleep -Seconds $SettleSec
try {
    # Focused the same way as the others, but NOT read back: a password box
    # reports an empty value through the accessibility tree by design, so a
    # read would prove nothing -- and anything that did read it would be a
    # place the plaintext could leak to.
    # Focus is checked, then re-checked, and this is the one field where that
    # is the ONLY check available: a password box reports an empty value
    # through the accessibility tree by design, so there is no reading it
    # back. Typing a password into whatever the user just clicked is the
    # failure this whole guard exists to prevent, so a miss here aborts
    # rather than typing blind.
    $pwFocused = $null
    for ($i = 1; $i -le 3; $i++) {
        [void](Focus-WebField $PasswordFieldMatch $GuardWaitSec)
        $pwFocused = Test-FieldFocused $PasswordFieldMatch
        if ($pwFocused -ne $false) { break }
        Write-Output "FEDLOGIN_NOTE:the password box lost focus (attempt $i); retrying"
        Start-Sleep -Milliseconds 600
    }
    if ($pwFocused -eq $false) {
        Write-Fail 'the password box would not hold keyboard focus -- something else kept taking it. Nothing was typed, and nothing was signed in.'
    }
    Send-FieldText $plain 'the password'
} finally {
    $plain = $null
    [GC]::Collect()
}
Send-Guarded '{ENTER}' 'submitting the password'
Start-Sleep -Seconds $StepDelaySec

# --- MFA confirm -----------------------------------------------------------
Write-Status 'confirming-mfa'
# The only send that accepts a non-Chrome window: Okta Verify owns this
# prompt. It carries no secret, and the title check still applies.
Send-Guarded '{ENTER}' 'confirming the MFA prompt' $MfaProcessMatch

Write-Status 'done'
exit 0
