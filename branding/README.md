# PS5Upload brand

The PS5Upload mark is a PS5 Pro drawn in negative space: only the shadows, the dark core and
the three Pro stripes are drawn, and the white plate is the background itself. The name is set
in [Titan One](https://fonts.google.com/specimen/Titan+One) (SIL Open Font License,
[`fonts/TitanOne-OFL.txt`](fonts/TitanOne-OFL.txt)) and kept as outlines, so no font is needed
to use the files.

## Masters (SVG)

| File | Use |
| --- | --- |
| `icon.svg` | App icon: black mark on a white rounded tile |
| `icon-dark.svg` | White mark on a black rounded tile |
| `icon-play.svg` | PS5Upload Play app icon: the mark with a play badge |
| `icon-minion.svg` | The Minion home-screen tile: the app icon with a black round badge holding a white browser window, bottom right (`payload/assets/webui_tile_icon0.png`) |
| `mark.svg`, `mark-white.svg` | The mark alone, transparent |
| `lockup-horizontal-black.svg`, `-white.svg` | Mark beside the name, transparent |
| `lockup-stacked-black.svg`, `-white.svg` | Mark above the name, transparent |

Use the black files on light backgrounds and the white files on dark ones.

## Images (`png/`)

The masters at full size, plus `github-social-preview.png` (1280×640, for the repository's
social preview), `discord-server-icon.png` (512×512) and `discord-banner.png` (960×540).

## Changing the logo

Edit the SVG masters, then run:

```sh
python3 scripts/release/brand-assets.py           # app icon source, web, payload tile, README, png/
cd client && npx tauri icon src-tauri/icons/sources/macos_squircle_1024.png -o src-tauri/icons
cd .. && python3 scripts/release/android-icons.py && python3 scripts/release/ios-icons.py
```
