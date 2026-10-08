# Opening server terminals from the dev container

With `terminal: Native`, incremusic-yue2 starts each server by running
`gio launch <run dir>/<name>.desktop`. That `.desktop` file has `Terminal=true`, so `gio` has to
find a terminal emulator to run the launcher script in. The dev container image has no terminal
emulator, so the launch fails:

```
gio: Unable to launch application …/gpu1.desktop: Unable to find terminal required for application
```

The server then never starts, and the Log tab shows this error.

`gio` itself is already in the image (from `libglib2.0-bin`), and the container can already open
windows on the host's X server (`DISPLAY` and `/tmp/.X11-unix` are passed through in
`devcontainer.json`). The only thing missing is a terminal emulator.

## The change

Add `xterm` to the first `apt-get install` in `.devcontainer/Dockerfile`:

```dockerfile
RUN apt-get update && apt-get install -y \
    libx11-6 \
    libxcursor1 \
    libxrandr2 \
    libxi6 \
    libxkbcommon0 \
    libxkbcommon-x11-0 \
    libgl1 \
    libegl1 \
    ffmpeg \
    libasound2-dev \
    xterm \
    && rm -rf /var/lib/apt/lists/*
```

`devcontainer.json` needs no changes.

Why `xterm`:

- GLib 2.84 (Debian 13) looks for a terminal in this order: `xdg-terminal-exec`,
  `gnome-terminal`, `mate-terminal`, `xfce4-terminal`, `tilix`, `konsole`, `nxterm`,
  `color-xterm`, `rxvt`, `dtterm`, `xterm`. `xterm` is on that list and also registers itself
  as `x-terminal-emulator`.
- It is small, uses plain X11 (which the container already has), and doesn't need D-Bus, a
  GPU, or a desktop session.
- It still works with `--cap-drop=ALL`. It can't write login records (utmp) without extra
  privileges, but it skips that and carries on.

If you would rather have `gnome-terminal` or `konsole`, those are on the list too. They pull in
many more packages, and `gnome-terminal` also needs a D-Bus session bus, which the container
doesn't run.

## Applying it

The container mounts `Dockerfile` and `devcontainer.json` read-only, so make the edit **on the
host**, then run **Dev Containers: Rebuild Container** in VS Code.

## Checking that it works

In the rebuilt container:

```sh
command -v xterm x-terminal-emulator                      # both should print a path
gio launch ~/.cache/incremusic-yue2/run/gpu1.desktop          # should open an xterm running gpu1
```

(`~/.cache/incremusic-yue2/run/` is the run directory used when `XDG_RUNTIME_DIR` is unset, as it
is in the container. The `.desktop` files are there once the app has tried to launch a server
at least once.)

Then start the app. Each server should open in its own xterm titled `audiocpp gpu1 :9123`
and so on, and the server should show as ready in the server bar.

## Notes

- **The window closes when the server exits.** The launcher script `exec`s the server, so the
  terminal goes away with it, even after a crash. Everything the server printed is still in
  `<run dir>/<name>.log` and in that server's pane in the Log tab.
- **Keeping the window open after exit:** skip `gio` and name the terminal directly in the
  config:

  ```ron
  terminal: Command(["xterm", "-hold", "-T", "{name}", "-e", "{script}"]),
  ```

  `-hold` keeps the window open after the server exits. This works with the same `xterm`
  package.
- **No terminal windows at all:** `terminal: Headless` needs no Dockerfile change. The app runs
  the servers itself, and their output appears only in the Log tab and the `.log` files.
