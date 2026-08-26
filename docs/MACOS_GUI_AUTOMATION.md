# macOS GUI automation permissions

Finch requires two independent approvals before it can inspect or control the
macOS desktop:

1. **Finch capability consent:** enable **GUI automation** in `finch setup` or
   `/setup`. This persists `features.gui_automation = true`.
2. **macOS Accessibility trust:** macOS must trust the Finch process that will
   perform the action. The setup wizard requests Apple's native Accessibility
   prompt and then checks the permission separately. A requested prompt does
   not mean access was granted.

The Settings page reports one of these effective states:

- **disabled** — Finch capability consent is off;
- **unsupported** — this Finch build or platform has no native backend;
- **permission required** — Finch consent is on, but macOS trust is absent or
  was revoked;
- **available** — both gates are currently open. Individual automation effects
  still go through Finch's normal capability approval.

Press `R` on the GUI automation row for a passive re-check; it never requests
the native prompt. Press `P` to explicitly request or retry Apple's native
Accessibility prompt, and then press `R` to verify the current process. Press
`O` to open **System Settings → Privacy & Security → Accessibility**. The app
name shown by Apple's native prompt or Accessibility pane is the authoritative
user-facing target. Finch also reports its executable path and launcher as
diagnostic hints, but cannot derive the underlying TCC identity from those
hints. Restarting Finch may be necessary after changing the setting. Even after
macOS trust is verified, individual effects still require Finch's normal
capability approval.

macOS exposes current trust as a Boolean; it does not tell Finch whether an
untrusted user is still considering the asynchronous prompt or explicitly
denied it. Finch therefore records only that a prompt was requested and whether
access was previously observed. It can report that the current check is false
after a prior true observation, but cannot tell whether access was revoked or a
responsible app, signature, or build changed. Finch never turns prompt history
into authority or claims a pending prompt was denied.

## Which process to grant

Apple's trust API checks the current process, but macOS may attribute the grant
to a responsible host or containing app. Finch reports both its executable and
the launcher when known; those values are not a claim about which TCC record
macOS selected. The name macOS displays remains authoritative:

- For a normal Terminal, iTerm, or IDE-terminal launch, the responsible entry
  may be that host application rather than a standalone Finch path.
- A packaged Finch app must be granted under its packaged identity.
- The process that performs automation needs the grant. If automation runs in
  a Finch daemon, establish and verify trust in that execution context;
  granting only a separate frontend does not establish that the daemon process
  is trusted. Follow the app name macOS displays rather than inferring a TCC
  identity from Finch's executable diagnostic.
- Native prompts are suppressed for SSH and noninteractive/headless sessions.
  Establish the permission from a local interactive Finch session instead.

Rebuilding or moving an unsigned development binary can cause macOS to treat
it as a different client. If System Settings visibly contains a grant while
Finch still reports that its current process is untrusted, the entry may cover a
different responsible app, signature, or build; Finch cannot determine which
from the Boolean trust API. Re-open setup and verify the state after changing
the binary or if permission was revoked.
