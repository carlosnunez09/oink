# Oink Remote

See and control the Windows PC from the Mac when the two are not on the same
network. Oink's own sharing (one keyboard and mouse across screens that sit
side by side) needs both computers in front of you; Oink Remote shows the
PC's screen in a window instead.

It uses its own connection, separate from Oink's device network. Tailscale
carries and encrypts it; Oink Remote adds an access key so that only your
viewer can connect.

## Use

On the Windows PC (leave it running):

```
oink-remote host
```

It prints the access key and the command for the other computer, for example
`oink-remote connect 100.101.102.103`.

On the Mac:

```
oink-remote connect 100.101.102.103
```

The first time it asks for the access key and then remembers it for that
address. The Tailscale machine name works in place of the address.

| In the viewer | |
|---|---|
| Left Command | Ctrl on the PC (Command-C copies) |
| Right Command | Windows key |
| Control-Option-Command-F | Full screen on or off |
| Closing the window | Disconnects |

Options: `oink-remote host --help`, `oink-remote connect --help` (monitor,
frame rate, quality, port, a new key).

## How it works

| Part | File |
|---|---|
| Messages on the connection | `src/proto.rs` |
| Access key and handshake, which addresses a host accepts | `src/auth.rs` |
| Changed 64-pixel tiles sent as JPEG, sharpened when the screen rests | `src/codec.rs` |
| Host session: frame pacing, input, releasing held keys | `src/host.rs` |
| Windows capture (Desktop Duplication, GDI fallback) and `SendInput` | `src/windows_host.rs` |
| Viewer connection | `src/client.rs` |
| Viewer window, keys | `src/viewer.rs`, `src/keymap.rs`, `src/scale.rs` |

The host sends a frame only when the viewer has shown the one before the
last, so a slow connection lowers the frame rate instead of adding delay.

## Limits

- Host on Windows only; viewer on macOS and Windows.
- A host accepts only Tailscale addresses (100.64.0.0/10) unless started
  with `--allow-lan`. The connection itself is not encrypted by Oink Remote.
- The lock screen, the sign-in screen and UAC prompts cannot be seen or
  controlled, so leave the PC signed in and unlocked. Windows of programs run
  as administrator ignore the input unless `oink-remote host` itself runs as
  administrator.
- The pointer you see is the Mac's own; the PC's pointer shape (text beam,
  resize arrows) is not shown.
- No sound, clipboard or file transfer yet.
- One viewer at a time; a new one replaces the current one.
