# Handoff notes: branch `claude/charming-cray-95q0tb`

Context for continuing this work in a new chat. Fork: `carlosnunez09/oink`
(upstream: `oinkvm/oink`). Camera sharing and file-transfer tuning were only
discussed, not started, and are deliberately left out of these notes.

## Setup

- Windows 11 PC (docked laptop) and an Apple Silicon MacBook Air, both running Oink.
- The Windows mouse and keyboard control the Mac.
- The two are joined by a direct Ethernet cable with no router, so each gets a
  link-local `169.254.x.x` address. Wi-Fi is also on, for internet.
- The cloud session cannot reach either computer. All installs are done by
  hand from the commands below.

## What is on the branch

| Commit | Change |
|---|---|
| `70d7875` | Window-drag fix (macOS) |
| `b4dcb05`, `93ae45f` | Unsigned test-build workflow |
| `1ab1ee0` | Direct connection setting; faster cable discovery |
| `8dfe58f` | Battery-aware check interval |
| `ce875f1` | Cable-only connections (never Wi-Fi) |

### 1. Window-drag fix
Problem: with the Windows mouse, Mac windows could be resized but not moved by
the title bar. Cause: the button press was posted natively (`IOHIDPostEvent`)
but drag motion was posted as CG events, and WindowServer's own title-bar drag
only follows drags from the same HID stream as the press.

Fix: left-button drags after a native press now also go through
`IOHIDPostEvent`. Cross-device file drags, relative (game) motion, right/other
button drags and CG-fallback presses keep the CG path.

- `crates/oink-input/src/macos/cursor_reveal.rs` (`post_mouse`, `Kind::Hid.ty`, `native_motion_type`)
- `crates/oink-input/src/macos/inject.rs` (`submit_move_with_buttons`)

Unverified assumption: `NX_LMOUSEDRAGGED` reads the absolute position the same
way `NX_MOUSEMOVED` does. If wrong, a dragged window would jump.

### 2. Direct connection setting
Settings → **Direct connection** → **Connect only over the direct cable**
(stored as `direct_cable_pause`, off by default). While on:

- Oink dials only link-local or loopback addresses, refuses incoming
  connections from other addresses, and closes existing ones so they reconnect
  over the cable. Global switch: `oink_network::link::set_cable_only`.
- Unplugging the cable pauses local sharing; plugging it back resumes. Only a
  pause made by the monitor is lifted, never a user, remote or emergency pause.
- A **Cable status** row shows the cable state and addresses
  (`direct_cable_status` request).
- With the cable unplugged, devices do not connect at all, Wi-Fi included.

Files:
- `crates/oink-agent/src/node/direct_cable.rs` (monitor, `Monitor::step`, tests)
- `crates/oink-network/src/link/mod.rs` (`set_cable_only`, `is_cable_address`, `address_allowed`)
- `crates/oink-network/src/link/runtime.rs` (dial filter, inbound refuse, close)
- `crates/oink-agent/src/api.rs`, `api/preferences.rs`, `config.rs`, `agent_api.rs`, `remote.rs`
- `apps/oink/src/settings/DirectCable.tsx`, `schema.tsx`, `SettingsPage.tsx`, `model.ts`, `prefs.ts`
- `apps/oink/src/i18n/messages/{en,ja,ko}.ts` (keys are the Chinese source text)

### 3. Discovery
- Addresses heard from a device in the last 10 s are all kept, not just the
  latest beacon's, and cable addresses are listed first
  (`crates/oink-agent/src/config_module/runtime.rs`: `Seen.addr_seen`,
  `direct_first`, `member_candidates`).
- Why Wi-Fi used to win: a direct connection is never moved to a better
  address; only relayed links retry direct. Cable-only mode is the fix.

### 4. Battery-aware checks
The cable monitor and local-address check run every 1 s on AC and every 3 s on
battery (`crates/oink-agent/src/power_source.rs`; `GetSystemPowerStatus` on
Windows, IOKit on macOS, cached 30 s, unknown counts as AC).

### 5. Test-build workflow
`.github/workflows/macos-test-build.yml` ("Test build (unsigned)") runs on
pushes to `claude/**` that touch `crates/**`, `apps/oink/**` or the workflow.
It builds Apple Silicon macOS and Windows x64 with `build-support/build.py`,
uploads `oink-macos-arm64-unsigned` and `oink-windows-x64-unsigned`, and runs
the macOS input tests plus the new tests. It also builds and tests
`oink-remote` and adds it to both artifacts. It produces program files, not
signed installers. Each run takes about 20 min (Mac) and 40 min (Windows).

Latest passing run with everything above: **42676921592**.

### 6. Oink Remote (remote desktop over Tailscale)
Goal: when the Mac is away from the PC (no cable, not the same LAN), see and
control the Windows PC from the Mac. Oink has no screen view, so this is a new
crate, `crates/oink-remote`, with its own connection (TCP, default port 47489)
that is separate from Oink's device network. Tailscale provides reachability
and encryption; Oink Remote adds an access key. See
`crates/oink-remote/README.md` for use, design and limits.

- It is a separate program, `oink-remote` (`host` on Windows, `connect` on
  the Mac). The test-build workflow builds it and puts it in both artifacts.
- Not done yet: starting it from the Oink app (a setting on the PC, a
  "Connect remotely" button on the Mac), connecting automatically when the
  cable or LAN is gone, the lock screen (needs the `OinkInput` service),
  clipboard and sound, hosting on macOS.
- Dependencies were kept to crates already in `Cargo.lock`. `jpeg-encoder`
  would be faster but its IJG licence is not on the allowed list in
  `deny.toml`; the `image` crate's encoder is used instead.

## Verification status

- Oink Remote: unit tests and a whole-session test (fake screen and input over
  a local connection) pass on Linux; the Windows and macOS code type-checks
  and passes clippy for those targets from Linux. Not yet run on the real
  computers: Windows capture and input, the Mac viewer window.

- Compiled on GitHub macOS and Windows runners; the new unit tests pass there.
- The app type check and the i18n and settings-persistence tests pass.
- `oink-network` compiles and tests locally on Linux. Two tests in
  `link::runtime::tests` fail in the cloud container with or without these
  changes (they need a network reply the container does not send).
- `oink-agent` cannot be compiled in the cloud container (needs platform SDKs).
- Not yet confirmed on the real computers: the window-drag fix, cable-only
  reconnection, and whether the upstream-signed macOS extensions still load
  next to the ad-hoc signed app.

## Installing a build

Replace `RUN` with the run number. Original official copies were backed up to
`~/Oink-backup.app` (Mac) and `%USERPROFILE%\Oink-backup` (Windows); do not
overwrite them.

### Mac (use the Mac's own keyboard and trackpad)
```bash
cd ~/Downloads && rm -rf oink-macos-arm64-unsigned && gh run download RUN -R carlosnunez09/oink -n oink-macos-arm64-unsigned -D oink-macos-arm64-unsigned
pkill -x oink; pkill -x oink-agent; pkill -x oink-backend
cd ~/Downloads/oink-macos-arm64-unsigned && cp oink-backend oink-agent oink-crash-collector /Applications/Oink.app/Contents/MacOS/ && cp oink /Applications/Oink.app/Contents/Helpers/Oink.app/Contents/MacOS/
chmod +x /Applications/Oink.app/Contents/MacOS/oink-* /Applications/Oink.app/Contents/Helpers/Oink.app/Contents/MacOS/oink && xattr -dr com.apple.quarantine /Applications/Oink.app
codesign --force --sign - --preserve-metadata=entitlements,flags /Applications/Oink.app/Contents/Helpers/Oink.app
codesign --force --sign - --preserve-metadata=entitlements,flags /Applications/Oink.app
open /Applications/Oink.app
```
Then re-add Oink under System Settings → Privacy & Security → Accessibility
and Input Monitoring if the Windows mouse stops controlling the Mac.

Undo: `rm -rf /Applications/Oink.app && mv ~/Oink-backup.app /Applications/Oink.app`

### Windows (PowerShell as administrator)
```powershell
cd $env:USERPROFILE\Downloads; Remove-Item oink-new -Recurse -Force -ErrorAction SilentlyContinue; gh run download RUN -R carlosnunez09/oink -n oink-windows-x64-unsigned -D oink-new
Stop-Process -Name oink,oink-agent -Force -ErrorAction SilentlyContinue; Stop-Service OinkInput
Get-ChildItem "$env:USERPROFILE\Downloads\oink-new\*.exe" | Where-Object { Test-Path "C:\Program Files\Oink\$($_.Name)" } | ForEach-Object { Unblock-File $_.FullName; Copy-Item $_.FullName "C:\Program Files\Oink\" -Force; $_.Name }
Start-Service OinkInput; Start-Process "C:\Program Files\Oink\oink.exe"
```
Undo: stop as above, then
`Copy-Item "$env:USERPROFILE\Oink-backup\*" "C:\Program Files\Oink\" -Recurse -Force; Start-Service OinkInput`

### Oink Remote
The program is in the same artifacts (`oink-remote.exe`, `oink-remote`). It
does not need to be copied into the Oink installation.

Windows (PowerShell, in the download folder; allow it through the firewall
when Windows asks):
```powershell
cd $env:USERPROFILE\Downloads\oink-new; Unblock-File .\oink-remote.exe; .\oink-remote.exe host
```
Mac (Terminal; use the address the PC printed):
```bash
cd ~/Downloads/oink-macos-arm64-unsigned && chmod +x oink-remote && xattr -d com.apple.quarantine oink-remote 2>/dev/null; ./oink-remote connect 100.x.y.z
```

## Useful settings and facts

- **Pointer event rate limit**: 125 Hz by default (8 ms per update). "No
  limit" is better on the cable. An offered but unbuilt idea: switch to "No
  limit" automatically while on the cable and AC power.
- **Audio quality**: Quality (Opus 20 ms, default), Smooth (Opus 5 ms),
  Lossless (PCM, about 3.3 ms frames). Lossless should be lowest latency on
  the cable.
- Windows treats the cable link as a Public "Unidentified network"; the
  firewall may block Oink there until it is set to Private.
- The cable link has no gateway, so it does not affect internet access.
- Oink does not compress its traffic in general (audio uses Opus, images are
  PNG, diagnostics use deflate).
- Debug logging: `OINK_LOG=oink_input=debug`.

## Working conventions

- Develop on `claude/charming-cray-95q0tb`; no pull request unless asked.
- The user prefers Markdown over HTML, and plain step-by-step commands.
- Unsafe Rust blocks need `// SAFETY:` comments; new UI text needs entries in
  all three message catalogs.
