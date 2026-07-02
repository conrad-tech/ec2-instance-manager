#!/usr/bin/env python3
"""
fedup.py — Automate the `fed up` device-activation flow.

Subcommands:
  init   Create the plaintext vars file (~/.fedup/secrets.env) and lock it
         down so only you can read it. Prompts for the allowed OS user(s),
         the account username, and the account password.
  edit   Change the stored values (keeps the existing ones on empty input).
  check  Verify the current OS user is allowed to run the flow. Exit 0 if
         allowed, non-zero otherwise. (Used by the GUI to gate the feature.)
  run    Run `fed up`, drive the browser to enter code/username/password,
         and handle the 403 / "authorization expired" retry logic.

Access control: `secrets.env` holds ALLOWED_USERS — a comma-separated list of
OS usernames permitted to use this feature. If the current OS user is not in
that list, `run`/`check` refuse (exit code 3). The file also holds every other
var the flow needs, so an operator can configure it in one place.

Run this with a WINDOWS Python under Git Bash (not WSL), because `fed up`
and Chrome live on the Windows side.

Requires (for `run`): Playwright:
    pip install playwright
    playwright install chromium      # or rely on installed Chrome via channel="chrome"
"""

import argparse
import getpass
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

# ------------------------------------------------------------------ CONFIG ---
# Where the plaintext vars live. ~/.fedup/secrets.env
SECRETS_DIR = Path(os.path.expanduser("~")) / ".fedup"
SECRETS_FILE = SECRETS_DIR / "secrets.env"

# Defaults for values that may be overridden in secrets.env ------------------
DEFAULT_FED_CMD = "fed up"        # command that starts the device flow
DEFAULT_HEADLESS = False          # show Chrome (False) or run invisibly (True)

# How long to wait for `fed up` to print the "Go to ... code ..." line.
CODE_WAIT_TIMEOUT = 60          # seconds
# How long to wait for `fed up` to exit after the browser step completes.
EXIT_WAIT_TIMEOUT = 90          # seconds

# Retry behaviour ------------------------------------------------------------
MAX_403_RETRIES = 5             # safety cap on the "just run fed up again" loop
EXPIRED_WINDOW  = 120           # seconds — keep retrying expired auth for 2 min
EXPIRED_BACKOFF = 10            # seconds between expired retries

# Exit codes -----------------------------------------------------------------
EXIT_OK          = 0
EXIT_GENERIC     = 1
EXIT_NOT_ALLOWED = 3            # current OS user not in ALLOWED_USERS

# --- Output classification patterns (tweak wording to match your service) ---
RE_CODE     = re.compile(r"go to\s+(https?://\S+)\s+and enter code\s+([A-Za-z0-9\-]+)", re.I)
RE_403      = re.compile(r"\b403\b|\bforbidden\b", re.I)
RE_EXPIRED  = re.compile(r"authoriz\w*\s+\w*\s*expired|expired.*authoriz|token\s+expired|code\s+expired", re.I)

# --- Browser selectors (best-effort; adjust once you can see the real pages) -
SEL = {
    "code_label":     re.compile(r"activation code", re.I),
    "username_label": re.compile(r"user\s*name|username|email|login", re.I),
    "next_button":    re.compile(r"^\s*(next|continue|submit|sign in|log in)\s*$", re.I),
}
# ---------------------------------------------------------------------------

STATUS_SUCCESS = "success"
STATUS_403     = "403"
STATUS_EXPIRED = "expired"
STATUS_BROWSER = "browser_error"
STATUS_UNKNOWN = "unknown"


# ============================================================ secrets store ==
def parse_env(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):]
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        k = k.strip()
        v = v.strip()
        # Strip one layer of matching surrounding quotes.
        if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
            v = v[1:-1]
        if k:
            out[k] = v
    return out


def load_secrets() -> dict:
    if not SECRETS_FILE.exists():
        raise FileNotFoundError(
            f"No vars file at {SECRETS_FILE}. Run:  python fedup.py init"
        )
    return parse_env(SECRETS_FILE.read_text(encoding="utf-8"))


def write_secrets(values: dict) -> None:
    """Write secrets.env and lock it down to the current user."""
    SECRETS_DIR.mkdir(parents=True, exist_ok=True)
    lines = [
        "# fedup.py secrets — plaintext; protected by file permissions.",
        "# ALLOWED_USERS: comma-separated OS usernames permitted to renew.",
        "",
    ]
    for k, v in values.items():
        lines.append(f"{k}={v}")
    SECRETS_FILE.write_text("\n".join(lines) + "\n", encoding="utf-8")
    _lock_down(SECRETS_FILE)


def _lock_down(path: Path) -> None:
    """Best-effort: make the file readable only by its owner."""
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass
    if os.name == "nt":
        # Remove inherited ACLs and grant only the current user full control.
        user = os.environ.get("USERNAME", "")
        if user:
            try:
                subprocess.run(
                    ["icacls", str(path), "/inheritance:r",
                     "/grant:r", f"{user}:F"],
                    capture_output=True, check=False,
                )
            except OSError:
                pass


def current_os_user() -> str:
    """The current OS username, robust across platforms."""
    try:
        u = os.getlogin()
        if u:
            return u
    except OSError:
        pass
    for key in ("USERNAME", "USER", "LOGNAME"):
        v = os.environ.get(key, "").strip()
        if v:
            return v
    return ""


def allowed_users(secrets: dict) -> list:
    raw = secrets.get("ALLOWED_USERS", "")
    return [u.strip() for u in raw.split(",") if u.strip()]


def user_is_allowed(secrets: dict, user: str = None) -> bool:
    """Case-insensitive membership check; empty list denies everyone."""
    user = (user if user is not None else current_os_user()).strip().lower()
    if not user:
        return False
    return user in [u.lower() for u in allowed_users(secrets)]


def truthy(v: str) -> bool:
    return str(v).strip().lower() in ("1", "true", "yes", "on")


# ================================================================ commands ===
def cmd_init(_args):
    if SECRETS_FILE.exists():
        if input(f"{SECRETS_FILE} exists. Overwrite? [y/N] ").strip().lower() != "y":
            return
    print("Configure the auto-renew feature.")
    default_user = current_os_user()
    allowed = input(
        f"  OS username(s) allowed to renew (comma-separated) [{default_user}]: "
    ).strip() or default_user
    username = input("  Account username (for the login pages): ").strip()
    password = getpass.getpass("  Account password: ")
    fed_cmd = input(f"  Fed command [{DEFAULT_FED_CMD}]: ").strip() or DEFAULT_FED_CMD
    headless = input("  Run Chrome headless (invisible)? [y/N]: ").strip().lower() == "y"
    auto = input("  Auto-start renewal when auth expires? [y/N]: ").strip().lower() == "y"
    write_secrets({
        "ALLOWED_USERS": allowed,
        "USERNAME": username,
        "PASSWORD": password,
        "FED_CMD": fed_cmd,
        "HEADLESS": "true" if headless else "false",
        "AUTO_RENEW": "true" if auto else "false",
    })
    print(f"Saved vars to {SECRETS_FILE} (locked to owner).")


def cmd_edit(_args):
    current = load_secrets()
    print("Press Enter to keep the current value.")
    allowed = input(
        f"  Allowed OS user(s) [{current.get('ALLOWED_USERS','')}]: "
    ).strip() or current.get("ALLOWED_USERS", "")
    username = input(
        f"  Account username [{current.get('USERNAME','')}]: "
    ).strip() or current.get("USERNAME", "")
    password = getpass.getpass("  Account password [unchanged]: ") or current.get("PASSWORD", "")
    fed_cmd = input(
        f"  Fed command [{current.get('FED_CMD', DEFAULT_FED_CMD)}]: "
    ).strip() or current.get("FED_CMD", DEFAULT_FED_CMD)
    headless = input(
        f"  Headless? [{current.get('HEADLESS','false')}]: "
    ).strip() or current.get("HEADLESS", "false")
    auto = input(
        f"  Auto-renew on expiry? [{current.get('AUTO_RENEW','false')}]: "
    ).strip() or current.get("AUTO_RENEW", "false")
    write_secrets({
        "ALLOWED_USERS": allowed,
        "USERNAME": username,
        "PASSWORD": password,
        "FED_CMD": fed_cmd,
        "HEADLESS": headless,
        "AUTO_RENEW": auto,
    })
    print("Updated.")


def cmd_check(_args):
    """Exit 0 if the current OS user may run the flow, else non-zero."""
    try:
        secrets = load_secrets()
    except FileNotFoundError as e:
        print(str(e))
        sys.exit(EXIT_GENERIC)
    user = current_os_user()
    if user_is_allowed(secrets, user):
        print(f"OK: '{user}' is allowed.")
        sys.exit(EXIT_OK)
    print(f"DENIED: '{user}' is not in ALLOWED_USERS.")
    sys.exit(EXIT_NOT_ALLOWED)


# =============================================================== fed runner ==
class FedProcess:
    """Runs `fed up`, streams its output, and lets us watch for the code line."""

    def __init__(self, fed_cmd):
        self.lines = []
        self._lock = threading.Lock()
        self.proc = subprocess.Popen(
            fed_cmd, shell=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1,
        )
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        for line in self.proc.stdout:
            line = line.rstrip("\n")
            with self._lock:
                self.lines.append(line)
            print(f"  [fed] {line}")

    def full_output(self) -> str:
        with self._lock:
            return "\n".join(self.lines)

    def wait_for_code(self, timeout):
        """Return (url, code) once the code line appears, or None on timeout/exit."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            m = RE_CODE.search(self.full_output())
            if m:
                return m.group(1), m.group(2)
            if self.proc.poll() is not None:      # exited before printing a code
                return None
            time.sleep(0.3)
        return None

    def wait_exit(self, timeout):
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return self.proc.poll()

    def classify(self) -> str:
        out = self.full_output()
        if RE_EXPIRED.search(out):
            return STATUS_EXPIRED
        if RE_403.search(out):
            return STATUS_403
        if self.proc.poll() == 0:
            return STATUS_SUCCESS
        return STATUS_UNKNOWN

    def kill(self):
        if self.proc.poll() is None:
            try:
                self.proc.kill()
            except OSError:
                pass


# ============================================================ browser flow ===
def run_browser(url, code, username, password, headless) -> bool:
    """Drive Chrome through code -> username -> password -> popup. True on completion."""
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print("!! Playwright not installed.  pip install playwright  &&  playwright install chromium")
        return False

    shot_dir = SECRETS_DIR / "shots"
    shot_dir.mkdir(parents=True, exist_ok=True)

    with sync_playwright() as pw:
        try:
            browser = pw.chromium.launch(headless=headless, channel="chrome")
        except Exception:
            browser = pw.chromium.launch(headless=headless)   # fall back to bundled chromium
        ctx = browser.new_context()
        page = ctx.new_page()

        # Auto-accept any native dialog/popup (equivalent to "press enter").
        page.on("dialog", lambda d: d.accept())

        def click_next():
            btn = page.get_by_role("button", name=SEL["next_button"])
            if btn.count():
                btn.first.click()
            else:
                page.keyboard.press("Enter")

        try:
            print(f"  -> opening {url}")
            page.goto(url, wait_until="domcontentloaded")

            # 1) Activation code
            code_field = page.get_by_label(SEL["code_label"])
            if not code_field.count():
                code_field = page.locator("input:visible").first
            code_field.first.fill(code)
            click_next()
            page.wait_for_load_state("networkidle")

            # 2) Username
            user_field = page.get_by_label(SEL["username_label"])
            if not user_field.count():
                user_field = page.locator(
                    "input[type=email], input[name*=user i], input[type=text]:visible"
                )
            user_field.first.fill(username)
            click_next()
            page.wait_for_load_state("networkidle")

            # 3) Password
            pass_field = page.locator("input[type=password]:visible")
            pass_field.first.fill(password)
            click_next()

            # 4) Popup -> press Enter (dialog handler above covers native popups;
            #    this covers an in-page modal that submits on Enter)
            page.wait_for_timeout(1500)
            page.keyboard.press("Enter")
            page.wait_for_timeout(1500)
            return True

        except Exception as e:
            shot = shot_dir / f"fail_{int(time.time())}.png"
            try:
                page.screenshot(path=str(shot))
            except Exception:
                shot = "(no screenshot)"
            print(f"!! Browser step failed: {e}")
            print(f"!! Screenshot: {shot}")
            print("!! Adjust the SEL selectors near the top of fedup.py to match the real pages.")
            return False
        finally:
            browser.close()


# ================================================================ run loop ===
def one_attempt(username, password, fed_cmd, headless) -> str:
    print("\n=== running `fed up` ===")
    fed = FedProcess(fed_cmd)
    try:
        got = fed.wait_for_code(CODE_WAIT_TIMEOUT)
        if got is None:
            # fed either exited immediately or never printed a code
            fed.wait_exit(1)
            status = fed.classify()
            return status if status != STATUS_UNKNOWN else STATUS_UNKNOWN
        url, code = got
        print(f"  code: {code}")
        if not run_browser(url, code, username, password, headless):
            fed.kill()
            return STATUS_BROWSER
        fed.wait_exit(EXIT_WAIT_TIMEOUT)
        return fed.classify()
    finally:
        fed.kill()


def cmd_run(_args):
    secrets = load_secrets()

    # --- access gate: only listed OS users may run the flow -----------------
    user = current_os_user()
    if not user_is_allowed(secrets, user):
        print(f"\n❌ User '{user}' is not permitted to use auto-renew "
              f"(not in ALLOWED_USERS). Ask an admin to add you.")
        sys.exit(EXIT_NOT_ALLOWED)

    username = secrets.get("USERNAME", "")
    password = secrets.get("PASSWORD", "")
    fed_cmd = secrets.get("FED_CMD", DEFAULT_FED_CMD) or DEFAULT_FED_CMD
    headless = truthy(secrets.get("HEADLESS", str(DEFAULT_HEADLESS)))
    if not username or not password:
        print("USERNAME/PASSWORD missing from vars. Run:  python fedup.py edit")
        sys.exit(EXIT_GENERIC)

    print(f"Renewing as OS user '{user}' (headless={headless}).")

    n403 = 0
    expired_deadline = None

    while True:
        status = one_attempt(username, password, fed_cmd, headless)

        if status == STATUS_SUCCESS:
            print("\n✅ Success — empty prompt returned.")
            return

        if status == STATUS_403:
            expired_deadline = None
            n403 += 1
            print(f"\n⚠  Got 403 (attempt {n403}/{MAX_403_RETRIES}). Re-running `fed up`.")
            if n403 >= MAX_403_RETRIES:
                print("\n❌ Too many 403s. Stopping. Check Jitney, see above")
                sys.exit(EXIT_GENERIC)
            continue

        if status == STATUS_EXPIRED:
            n403 = 0
            now = time.time()
            if expired_deadline is None:
                expired_deadline = now + EXPIRED_WINDOW
            if now >= expired_deadline:
                print("\n❌ Authorization still expiring after 2 min.\n\nCheck Jitney, see above")
                sys.exit(EXIT_GENERIC)
            remaining = int(expired_deadline - now)
            print(f"\n⏳ Authorization expired. Waiting {EXPIRED_BACKOFF}s "
                  f"(retrying for ~{remaining}s more).")
            time.sleep(EXPIRED_BACKOFF)
            continue

        if status == STATUS_BROWSER:
            print("\n❌ Browser automation didn't complete — fix selectors and re-run.")
            sys.exit(EXIT_GENERIC)

        print("\n❓ Couldn't classify the `fed up` result (not success/403/expired).")
        print("   See the [fed] output above.  Check Jitney, see above")
        sys.exit(EXIT_GENERIC)


# =================================================================== main ====
def main():
    ap = argparse.ArgumentParser(description="Automate the `fed up` device activation flow.")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init",  help="create the plaintext vars file")
    sub.add_parser("edit",  help="edit the plaintext vars file")
    sub.add_parser("check", help="check whether the current OS user is allowed")
    sub.add_parser("run",   help="run the activation flow")
    args = ap.parse_args()

    {"init": cmd_init, "edit": cmd_edit, "check": cmd_check, "run": cmd_run}[args.cmd](args)


if __name__ == "__main__":
    main()
