# SLC Local TTS

A local text-to-speech server for Self-Loading Cargo (SLC).
SLC thinks it is talking to ElevenLabs, but every captain and cabin-crew
announcement is spoken on your own PC with free voices (Piper, Kokoro and
Supertonic). No ElevenLabs account or real API key is needed, and no
internet connection once your voices are installed.

## For SLC users

### Install

You need 64-bit Windows 10 (version 1903 or later) or Windows 11.

1. Download `SLC-Local-TTS-win-Setup.exe` from the latest release on
   [GitHub](https://github.com/mjlyman/slc-local-tts/releases/latest) and
   run it.
2. Windows SmartScreen may say it protected your PC, because the installer
   isn't signed. Click **More info**, then **Run anyway**.

It installs for your Windows user only, without asking for administrator
rights. It adds "SLC Local TTS" to the Start Menu (no desktop shortcut) and
starts SLC Local TTS at once.

**If you used an earlier copy of SLC Local TTS** (a plain
`slc-local-tts.exe` you started yourself):

1. Close the earlier copy before you run Setup.
2. Setup finds the existing SLC Local TTS folder and asks whether to
   overwrite or repair it. Accept it: your voices and settings are
   kept.
3. From then on, start SLC Local TTS from the Start Menu. Stop using the
   old `slc-local-tts.exe`, or delete it: it never updates itself.

### Quick start

1. **Start SLC Local TTS before SLC, and keep its window open while you fly.**
   SLC crashes when SLC Local TTS isn't there. Start it from the Start Menu.
   The window shows the address to use. Closing it stops the voices.
2. In SLC's ElevenLabs settings, set the **Custom API URL** to
   `http://127.0.0.1:8765` (no `/v1`, nothing after the port). SLC wants an
   API key too; any text works, because SLC Local TTS ignores it.
3. Keep SLC's option **"Automatically Switch Back to 'Voicepack' if TTS
   generation fails"** turned on. SLC hasn't been tested with SLC Local TTS
   with that option off.
4. Refresh SLC's voice list. Each voice's name says its engine and accent,
   for example "Joe (US, medium, Piper)". SLC's Text Speech Rate slider
   works.
5. **Install your voices before flying.** Each voice downloads its model the
   first time it is used (about 70 to 350 MB), and a first download can take
   a few minutes. All Kokoro voices share one model, and so do all
   Supertonic voices: installing one of them installs them all. Do this on
   the ground, in one of two ways:
   - **Press the voice's preview button in SLC.** If the voice is already
     installed, the preview plays. If the preview stays silent, the voice is
     still downloading: the SLC Local TTS window shows a line like
     `Voice af_heart: is still downloading, so it was not ready in time. The
     preview played silence. The download continues in the background; press
     preview again in a minute or two.` Keep doing that until the preview
     plays.
   - **Or download it from a terminal** (see below).

   SLC waits at most 10 seconds for each announcement, and a voice that
   isn't installed yet can't download in that time. Its first lines get
   SLC's failure sound instead.

### Downloading voices from a terminal

This works whether or not SLC Local TTS is already running. These are
PowerShell commands. Open PowerShell: right-click the Start button and
choose **Terminal** (Windows 11) or **Windows PowerShell** (Windows 10).
Then run:

```powershell
& "$env:LOCALAPPDATA\SLC-Local-TTS\current\slc-local-tts.exe" download en_US-joe-medium
```

Put the voice you want in place of `en_US-joe-medium`. It says when the
voice is installed. If the running SLC Local TTS is already downloading that
voice, the terminal waits for it to finish and then says the voice is
installed.

To list every voice, its engine, and whether it is installed:

```powershell
& "$env:LOCALAPPDATA\SLC-Local-TTS\current\slc-local-tts.exe" voices
```

### If something goes wrong

- **SLC plays a short failure sound instead of a line.** SLC Local TTS
  couldn't speak that line in time. The console window says why, and so does
  the log file in `%LocalAppData%\SLC-Local-TTS\logs\`. SLC doesn't keep the
  failure, so it should ask again the next time that line comes up.
- **Kokoro voices are slow on long announcements.** Lines longer than about
  390 characters may not finish in SLC's 10 seconds and get the failure
  sound. Use a Piper or Supertonic voice for those, or raise `num_threads` in
  `config.toml` (see below): faster speech, but more CPU taken from the sim.
- **"SLC Local TTS is already running, so this copy won't start a second
  one."** SLC Local TTS is already open, perhaps minimized or in another
  window. Use that one; SLC can keep talking to it. Press Enter to close the
  new window.
- **"Port 8765 is already in use".** Another program is using that port.
  Close it, or choose another `port` in `config.toml` and use the same port
  in SLC's Custom API URL.
- **You want SLC to say a line again with a different voice or setting.**
  SLC keeps every line it gets in its own audio cache. To clear it for one
  voice, use SLC's Cache Manager, or, with SLC closed, delete
  `C:\ProgramData\SLC\cache\perm_*-*-2-<voiceId>-*.mp3`, where `<voiceId>`
  is the voice's ID (for example `en_US-joe-medium` or `M1`).

### Stopping

Press **Ctrl+C** in the window. SLC Local TTS finishes the lines it is
speaking (up to a minute for a preview that is still downloading) and then
says "SLC Local TTS stopped". Press **Ctrl+C** again to quit at once.

### Updates

SLC Local TTS updates itself from GitHub:

- Each time it starts, it checks for a new version and, if there is one,
  downloads it in the background. Your flight isn't affected.
- When the download is done, the window says `Version X is ready. It
  installs the next time SLC Local TTS starts, so it never interrupts a
  flight.` (with the real version number in place of X).
- The next time you start SLC Local TTS, it says `Installing version X. SLC
  Local TTS closes now and starts again by itself in a few seconds.` and
  does that: its window closes, and a new one opens a few seconds later.
  Wait for the new window to show the address before you start SLC. The
  new window says `SLC Local TTS updated itself to X.`
- If the check can't reach GitHub, the window says so in one line, and SLC
  Local TTS keeps working as usual.
- To turn update checks off, put `check_for_updates = false` in
  `config.toml` (see below).

You don't need Setup.exe to update. If you run it again anyway, it offers to
repair the install. A repair keeps your voices and settings. Setup closes
SLC Local TTS while it works, so don't do this mid-flight.

### Uninstall

Open Windows **Settings**, then **Apps**, then find **SLC Local TTS** and
choose **Uninstall**.

This deletes the whole `%LocalAppData%\SLC-Local-TTS\` folder, including
your voices, settings and logs. To start fresh, uninstall and then install
again.

### Where your files are

Everything lives in `%LocalAppData%\SLC-Local-TTS\`:

- `models\`: the downloaded voices. Delete a voice's folder to remove it; it
  downloads again the next time it is used. Every Kokoro voice uses the one
  `kokoro-multi-lang-v1_0` folder, and every Supertonic voice the one
  `sherpa-onnx-supertonic-3-tts-int8-2026-05-11` folder, so deleting either
  folder removes every voice of that engine.
- `logs\`: one log file per day; the last 8 are kept.
- `espeak\`: speech data SLC Local TTS unpacks for itself.
- `config.toml`: your settings (optional, see below).
- `current\`: the program itself, `slc-local-tts.exe`, with `LICENSE.txt`
  and `THIRD_PARTY_NOTICES.txt` next to it. Each update replaces this
  folder.
- `Update.exe`, `packages\` and the rest: the installer's and updater's own
  files. Leave them alone.

### Settings (`config.toml`)

SLC Local TTS works without a settings file. To change something, create
`%LocalAppData%\SLC-Local-TTS\config.toml` with only the lines you want to
change, then restart SLC Local TTS. If a line is wrong, SLC Local TTS says
which one and doesn't start; fix or delete that line.

```toml
# The port SLC connects to. Use the same one in SLC's Custom API URL.
port = 8765

# CPU threads per synthesis, 1 to 8. More is faster but takes more CPU from
# the sim.
num_threads = 2

# "normal" or "below_normal". Below-normal can starve speech when the sim
# keeps every core busy, so normal is the default.
priority = "normal"

# Minutes a voice may sit unused before it is unloaded from memory, 1 to 1440.
idle_unload_minutes = 10

# How much the log says: "error", "warn", "info", "debug" or "trace".
# "debug" also logs the text of each line.
log_level = "info"

# Check GitHub for a new version at each start and download it in the
# background; it installs the next time SLC Local TTS starts.
check_for_updates = true

# Speed of each engine before SLC's own speed setting, 0.5 to 2.0.
[base_speed]
piper = 1.0
kokoro = 1.0
supertonic = 1.05

# Supertonic quality steps, 1 to 20. More steps sound a little better but
# take longer.
[supertonic]
num_steps = 5
```

## For developers

### Prerequisites

- Windows x64.
- Visual Studio 2022 **17.14 or newer** (MSVC toolset 14.44+) with the
  "Desktop development with C++" workload. The prebuilt sherpa-onnx
  libraries were built with it; older toolsets fail to link with `LNK1120`
  (18 unresolved `__std_*` symbols).
- Rust 1.99 or newer (edition 2024; the `rust-version` in `Cargo.toml`),
  MSVC target.
- Network access while building: `piper-phonemize-sys` downloads its
  espeak-ng data in its build script.

The repository path may contain spaces; quote it in every command.

### Build and test

Run these from the repository root.

One-time setup: fetch the native libraries (sha256-verified, kept in
`.native\`, outside `target\`):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-native-libs.ps1
```

Then:

```powershell
cargo build --release      # target\release\slc-local-tts.exe
cargo test                 # unit, API and socket tests; no models needed
cargo test -- --ignored    # real-model tests: real downloads and synthesis
```

`cargo test -- --ignored` downloads about 600 MB of models the first time.
They go into `target\real-model-home\`, so later runs reuse them.

`build.rs` embeds `windows\slc-local-tts.manifest` in every binary, the
test executables included. It switches the process's code page to UTF-8,
so espeak-ng and sherpa-onnx can open files under a Windows user name with
accents.

### The data folder

The program keeps its files in `%LocalAppData%\SLC-Local-TTS\`. Set
`SLC_LOCAL_TTS_HOME` to use another folder, for example to keep a test
setup apart from your real one; the real-model tests use it too when it is
set.

```powershell
$env:SLC_LOCAL_TTS_HOME = "D:\slc-tts-test"
target\release\slc-local-tts.exe
```

A copy that wasn't installed with Setup.exe (`cargo run`, or the exe in
`target\`) never checks for or installs updates.

### The voice catalog

`src\catalog\data.rs` is generated; don't edit it by hand. To change the
voices, edit `scripts\catalog-piper.csv` (or the Kokoro and Supertonic lists
in the script), then run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\gen-catalog.ps1
```

It needs an authenticated GitHub CLI (`gh`) and access to Hugging Face.

### Release tools

Packing the installer needs two more tools, at exactly these versions:

- **vpk 1.2.161**, Velopack's command-line tool. It needs the .NET SDK (8 or
  newer):

  ```powershell
  winget install Microsoft.DotNet.SDK.8
  dotnet tool install -g vpk --version 1.2.161
  ```

  Open a new terminal afterwards: `%USERPROFILE%\.dotnet\tools` is on the
  PATH only in terminals opened after the install. vpk 1.2.161 has no
  `--version`; the first line of `vpk -h` shows the version.
- **cargo-about 0.9.2**, which lists the Rust crates' licences. Without
  `--features cli`, 0.9.2 installs no program:

  ```powershell
  cargo install cargo-about --version 0.9.2 --locked --features cli
  ```

### Packing a release

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\pack-release.ps1
```

It checks the vpk version, builds the release, generates
`THIRD_PARTY_NOTICES.txt` (with `scripts\make-notices.ps1`), and packs full
x64 packages only. The output goes in `target\velopack\Releases\`:
`SLC-Local-TTS-win-Setup.exe`, `SLC-Local-TTS-<version>-full.nupkg`,
`releases.win.json` and `assets.win.json`. Pass `-OutputDir <folder>` to
put it somewhere else.

vpk won't repack a version that is already in the output folder. To repack
the same version, delete that folder (or use a new one) first.

### Testing an update from a local feed

1. Pack one version into a feed folder, then run the `Setup.exe` it made
   there to install it:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\pack-release.ps1 -OutputDir "$env:USERPROFILE\slc-feed"
   ```

2. Raise the version in `Cargo.toml`, run `cargo build` (it updates
   `Cargo.lock`; the pack script builds with `--locked`, which stops on a
   stale one), then pack again into the same folder.
3. Close the installed copy that Setup started, then start it again with
   the feed override, so its update check reads that folder instead of
   GitHub:

   ```powershell
   $env:SLC_LOCAL_TTS_UPDATE_FEED = "$env:USERPROFILE\slc-feed"
   & "$env:LOCALAPPDATA\SLC-Local-TTS\current\slc-local-tts.exe"
   ```

   It downloads the new version. Press Ctrl+C, then start it again to
   install it. Afterwards, clear the variable with
   `Remove-Item Env:SLC_LOCAL_TTS_UPDATE_FEED`, or close that terminal.

`SLC_LOCAL_TTS_UPDATE_FEED` may also be an `http://` or `https://` URL. When
it is unset or empty, the update check reads GitHub.

### Releasing

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`, by building), then
   commit.
2. Push a tag `v<version>`, for example `v0.2.0`.

`.github/workflows/release.yml` runs on that tag. It fails unless the tag
matches the `Cargo.toml` version. It runs the tests, packs the release,
uploads it with `vpk upload github`, and publishes the GitHub release.

### Commands

```text
slc-local-tts              start the server (same as `serve`)
slc-local-tts serve        start the server
slc-local-tts voices       list the voices and whether each is installed
slc-local-tts download ID  download one voice's model now
```

## Licence

GPL-3.0-or-later (see `LICENSE`), because SLC Local TTS uses espeak-ng. The
source code is on [GitHub](https://github.com/mjlyman/slc-local-tts).
`THIRD_PARTY_NOTICES.txt`, installed in
`%LocalAppData%\SLC-Local-TTS\current\`, lists the licences of the
libraries SLC Local TTS uses and of the voice models. Each voice model has
its own licence.
