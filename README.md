# OA Volume Controller Plugin

> **Fork notice:** This is a fork of [mdvictor/opendeck-volume-controller](https://github.com/mdvictor/opendeck-volume-controller) by Victor Marin (MIT licensed). It adds encoder/dial support for Stream Deck + devices and a redesigned 100×100 encoder LCD layout that renders the app name, icon, and a thin right-side volume bar. The original button-grid functionality is unchanged.

A per-application volume control plugin for Stream Deck using OpenDeck on Linux.

![Showcase](./img/readme.png)

## Overview

Take full control of your sound experience with fine-tuned per-app volume management! This plugin integrates with PulseAudio to provide a visual mixer interface directly on your Stream Deck, allowing you to control individual application volumes with dedicated buttons.

## Features

- **Per-Application Volume Control**: Adjust volume levels for each running audio application independently
- **Visual Volume Bars**: Real-time graphical representation of volume levels on your Stream Deck
- **Mute Toggle**: Quickly mute/unmute applications with a single button press
- **System Mixer Support**: Optional system-wide mixer control
- **Auto-Detection**: Automatically discovers and tracks running audio applications
- **App Icons**: Displays application icons for easy identification
- **Real-time Updates**: Monitors PulseAudio events and updates the interface dynamically
- **Ignore apps**: Exclude specific apps from showing in the volume controller
- **Encoder/Dial Support** *(fork addition)*: Assign the action to an encoder dial on Stream Deck + / Stream Deck + XL. Rotate to adjust volume, press to mute, and see the app name, icon, and a live volume bar on the encoder's LCD zone.

## Installation

### From source (Linux x86_64)

Prerequisites:

- Rust toolchain (`rustup` / `cargo`)
- PulseAudio, or PipeWire with the PulseAudio compatibility layer
- OpenDeck installed and running
- A sans-serif Bold font on the system for the encoder LCD title rendering — the plugin looks for Noto Sans Bold or DejaVu Sans Bold at the usual Linux font paths and silently skips the title if neither is present

Build and install:

```sh
git clone https://github.com/koconnorgit/opendeck-volume-controller.git
cd opendeck-volume-controller
cargo build --release
```

If you already have the upstream plugin installed through OpenDeck's plugin store, you can replace just the binary:

```sh
cp target/release/oa-volume-controller \
  ~/.config/opendeck/plugins/com.victormarin.volume-controller.sdPlugin/oa-volume-controller-x86_64-unknown-linux-gnu
```

For a fresh install without the upstream plugin, also copy the plugin assets:

```sh
PLUGIN_DIR=~/.config/opendeck/plugins/com.victormarin.volume-controller.sdPlugin
mkdir -p "$PLUGIN_DIR"
cp manifest.json pi.html LICENSE "$PLUGIN_DIR/"
cp -r img "$PLUGIN_DIR/"
cp target/release/oa-volume-controller "$PLUGIN_DIR/oa-volume-controller-x86_64-unknown-linux-gnu"
```

Reload the plugin without restarting OpenDeck:

```sh
opendeck --reload-plugin com.victormarin.volume-controller.sdPlugin
```

Or restart OpenDeck fully to pick up the new binary.

## Usage

### Button-grid layout (original)

Drag the `Volume Control Auto Grid` action across the SD grid. This was tested and developed with the SD3x5 in mind, so at least one full column (3 actions per column) is needed to show one volume mixer, where the first action button is the mixer icon together with the mute/unmute button, the second is Vol+ and the remaining button is Vol-.

After setting your grid, switch profiles and return to your volume controller profile to kick things off.

Pressing the volume app icon will mute it.
Long pressing the volume app icon will set it as ignored and remove that specific volume bar from the device. To revert this action click on any volume controller grid cell in the opendeck UI and remove it from the list of ignored apps.

### Encoder/dial layout *(fork addition)*

Assign the action directly to an encoder dial on a Stream Deck + or Stream Deck + XL. Each encoder gets its own per-app mixer:

- **Rotate** the dial to adjust volume up or down
- **Press** the dial to toggle mute
- The LCD zone shows the app name at the top, the app icon centered, and a vertical volume bar along the right edge; muting dims the whole zone

## ToDo (from upstream)

- ~~Support for dials~~ *(done in this fork)*
- Support for mini devices (2×3 grid cells)
- Manual setup for specifying which specific app would live on what column

Contributions are welcome!

## Credits

Original plugin by **Victor Marin** — [mdvictor/opendeck-volume-controller](https://github.com/mdvictor/opendeck-volume-controller), MIT licensed. This fork preserves the original license and history.
