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

Press `R` on the GUI automation row to request the native prompt or re-check
the result. Press `O` to open **System Settings → Privacy & Security →
Accessibility**. The app name shown by Apple's native prompt or Accessibility
pane is the authoritative TCC identity; Finch also reports its executable path
and launch context as diagnostic hints. Restarting Finch may be necessary after
changing the setting.

macOS exposes current trust as a Boolean; it does not tell Finch whether an
untrusted user is still considering the asynchronous prompt or explicitly
denied it. Finch therefore records only that a prompt was requested and whether
access was previously observed. It can identify a missing/revoked prior grant,
but never turns prompt history into authority or claims a pending prompt was
denied.

## Which process to grant

Apple's trust API checks the current process, but macOS may attribute the grant
to a responsible host or containing app. Finch reports both its executable and
the launcher when known; the name macOS displays remains authoritative:

- For a normal Terminal, iTerm, or IDE-terminal launch, the responsible entry
  may be that host application rather than a standalone Finch path.
- A packaged Finch app must be granted under its packaged identity.
- The process that performs automation needs the grant. If automation runs in
  a Finch daemon, grant the daemon's reported Finch executable; granting only a
  separate frontend does not grant the daemon.
- Native prompts are suppressed for SSH and noninteractive/headless sessions.
  Establish the permission from a local interactive Finch session instead.

Rebuilding or moving an unsigned development binary can cause macOS to treat
it as a different client. Re-open setup and verify the state after changing the
binary or if permission was revoked.
