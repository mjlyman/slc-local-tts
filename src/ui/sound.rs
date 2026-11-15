//! Plays a voice's preview through Windows' own `PlaySoundW`, so the window
//! needs no audio library.

use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

/// Plays one WAV at a time. A new one stops the one before.
#[derive(Default)]
pub struct Player {
    /// The WAV playing now, if any.
    ///
    /// `SND_ASYNC` makes `PlaySoundW` return at once and play on a thread
    /// of Windows' own, reading straight from this buffer as it goes. Rust
    /// can't see that Windows still holds a pointer into it, so the buffer
    /// is kept here, alive, until the sound is stopped: by the next `play`,
    /// or when the player is dropped. Freeing it any sooner would leave
    /// Windows reading freed memory.
    playing: Option<Vec<u8>>,
}

impl Player {
    pub fn new() -> Player {
        Player::default()
    }

    /// Stops any sound playing, then starts `wav` and returns without
    /// waiting for it to finish. False when Windows wouldn't play it (an
    /// empty or damaged buffer, or no sound device); a warning is logged.
    pub fn play(&mut self, wav: Vec<u8>) -> bool {
        // Stop first: the old buffer may only be freed once Windows has
        // let go of it.
        self.stop();
        if wav.is_empty() {
            tracing::warn!("the voice preview was empty, so there was nothing to play");
            return false;
        }
        // The buffer moves into `self` before Windows sees it. Moving a
        // `Vec` moves only its pointer, length and capacity, never the
        // bytes, so the pointer below stays valid for as long as `playing`
        // holds it.
        let wav = self.playing.insert(wav);
        // SAFETY: with `SND_MEMORY`, the first argument is the WAV's bytes
        // rather than a file name; the cast only satisfies the signature.
        // The bytes live in `self.playing` until `stop` has returned.
        let played = unsafe {
            PlaySoundW(
                wav.as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            )
        };
        if played == 0 {
            tracing::warn!(
                "Windows couldn't play the voice preview ({} bytes)",
                wav.len()
            );
            self.playing = None;
            return false;
        }
        true
    }

    /// Stops the sound playing, if any, and frees its buffer.
    fn stop(&mut self) {
        if self.playing.is_some() {
            // SAFETY: a null sound stops whatever this process is playing.
            // It returns once Windows has stopped reading the buffer.
            unsafe { PlaySoundW(std::ptr::null(), std::ptr::null_mut(), 0) };
            self.playing = None;
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unplayable_buffers_are_not_kept() {
        let mut player = Player::new();
        assert!(!player.play(Vec::new()));
        assert!(player.playing.is_none());
        // Not a WAV: Windows refuses it, and `SND_NODEFAULT` keeps it from
        // playing the default beep instead.
        assert!(!player.play(b"not a wav file".to_vec()));
        assert!(player.playing.is_none());
    }
}
