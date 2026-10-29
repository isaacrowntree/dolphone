# What this bundle carries besides Dolphone's own code

- **Dolphone** source: MIT (see LICENSE). This binary links
  [flipctl-app](https://github.com/flipperdevices/flipctl-slint) and through it
  [Slint](https://slint.dev), taken under Slint's GPL-3.0-only option, so **the
  binary as a whole is GPL-3.0-only**. The corresponding source for this exact
  build is attached to the release it came from (`dolphone-<version>-source.tar.zst`,
  with every dependency vendored) and is at https://github.com/isaacrowntree/dolphone.
- **Panel fonts**, compiled in through flipctl-app, from
  [flipctl-fonts](https://github.com/flipperdevices/flipctl-fonts):
  - *HaxrCorp 4090* by haxrcorp, adapted by Flipper Devices: CC BY-SA 3.0. The
    adaptation and the glyph tables generated from it stay under CC BY-SA 3.0.
  - *Born2bSportyV2* by Fredrik Forsberg: Unlicense.
  - *Busy9px*, Flipper Devices: MIT.
  Their license texts are under `fonts/` beside this file.
- Dolphone is not made or endorsed by Flipper Devices Inc.
