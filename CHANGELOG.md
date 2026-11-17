# Changelog

What changed in each version of SLC Local TTS, newest first.

## 0.3.1

- **Louder, even voices.** Every announcement is now levelled to the same
  loudness, so all voices come out about equally loud and noticeably louder
  than in 0.3.0.
- **Speech volume slider** on the Settings page (50 to 200%). It applies to
  new announcements at once; press Play on the Voices page to hear it.
- **Lines SLC already saved keep their old volume** until you clear SLC's
  cache for those voices (Cache Manager, or see the README).
- Going back to 0.3.0 after moving the slider? Delete the `volume_percent`
  line from `config.toml` first, or 0.3.0 won't start.

## 0.3.0

Initial public release.
