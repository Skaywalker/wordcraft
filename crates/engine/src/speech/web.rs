//! Read Aloud in the browser, through the Web Speech API (`speechSynthesis`).
//!
//! The browser speaks one sentence at a time, as the desktop speech programs do: each sentence is
//! an utterance in its language, at the reading speed, in the chosen voice when it suits the
//! sentence. Its end event moves on to the next sentence; every change to the player (pause, skip,
//! stop, a new speed) cancels the utterance and speaks the current sentence again if still playing.
//! The generation and the sentence index an utterance was started for keep a late event of a
//! cancelled one from moving the player. An utterance cancelled from outside the player (another
//! script on the page) is spoken again, and one the browser can't speak in its language or voice
//! is spoken in the default voice, as on the desktop ([`super::web_retry`]).

use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex, Weak};

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{SpeechSynthesis, SpeechSynthesisUtterance, SpeechSynthesisVoice};

use super::{Shared, State, Voice, VoiceKind, WebAttempt, WebRetry, lock, plain_tag, same_player, voice_for, web_retry};

type Handler = Closure<dyn FnMut(web_sys::Event)>;
type Player = (Mutex<Shared>, Condvar);

/// The utterance being spoken, its event handler, and the player it speaks for.
struct Current {
    utterance: SpeechSynthesisUtterance,
    handler: Handler,
    player: Weak<Player>,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
    /// The previous handler, kept until the next cancel replaces it: the handler that ends a
    /// sentence is still running when it starts the next one, and must not be dropped meanwhile.
    static RETIRED: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

/// What Read Aloud says when the browser has no speech synthesis.
const UNAVAILABLE: &str = "Read Aloud isn't available in this browser";

fn synth() -> Option<SpeechSynthesis> {
    web_sys::window()?.speech_synthesis().ok()
}

/// Stop the sentence being spoken and detach its handler. The browser is told to cancel only when
/// it has something to cancel: Chrome and Safari can drop an utterance spoken right after a cancel,
/// which would otherwise happen at every sentence end.
pub(super) fn cancel() {
    if let Some(c) = CURRENT.with(|c| c.borrow_mut().take()) {
        c.utterance.set_onend(None);
        c.utterance.set_onerror(None);
        RETIRED.with(|r| *r.borrow_mut() = Some(c.handler));
    }
    if let Some(s) = synth()
        && (s.speaking() || s.pending())
    {
        s.cancel();
    }
}

/// [`cancel`], when the sentence being spoken is `player`'s: another player going away (the
/// session a template preview builds and drops) leaves the reading alone.
pub(super) fn cancel_for(player: &Arc<Player>) {
    if CURRENT.with(|c| c.borrow().as_ref().is_some_and(|c| same_player(&c.player, player))) {
        cancel();
    }
}

/// The browser's voices.
fn browser_voices(synth: &SpeechSynthesis) -> Vec<SpeechSynthesisVoice> {
    synth.get_voices().iter().filter_map(|v| v.dyn_into::<SpeechSynthesisVoice>().ok()).collect()
}

/// A browser voice as Read Aloud lists it (the id is the voice URI).
fn to_voice(v: &SpeechSynthesisVoice) -> Voice {
    Voice { id: v.voice_uri(), name: v.name(), lang: v.lang(), kind: VoiceKind::Web }
}

/// The browser's voices as Read Aloud lists them.
pub(super) fn voices() -> Vec<Voice> {
    synth().map(|s| browser_voices(&s)).unwrap_or_default().iter().map(to_voice).collect()
}

/// Stop with `error`.
fn fail(shared: &Arc<Player>, error: String) {
    let mut s = lock(&shared.0);
    s.state = State::Stopped;
    s.error = Some(error);
}

/// After a change: cancel what is being spoken, and speak the current sentence if playing.
pub(super) fn sync(shared: &Arc<Player>) {
    speak(shared, WebAttempt::default());
}

/// Cancel what is being spoken, and speak the current sentence if playing, as `attempt` says.
fn speak(shared: &Arc<Player>, attempt: WebAttempt) {
    cancel();
    let (text, lang, voice, rate, generation, index) = {
        let s = lock(&shared.0);
        if s.state != State::Playing {
            return;
        }
        let Some(u) = s.queue.get(s.index) else { return };
        (u.text.clone(), u.lang.clone(), s.voice.clone(), s.rate, s.generation, s.index)
    };
    let Some(synth) = synth() else { return fail(shared, UNAVAILABLE.into()) };
    let Ok(u) = SpeechSynthesisUtterance::new_with_text(&text) else { return fail(shared, UNAVAILABLE.into()) };
    // Whether the utterance was given a language or a voice the browser might not have.
    let mut tailored = false;
    if !attempt.plain {
        if let Some(tag) = lang.as_deref().and_then(plain_tag) {
            u.set_lang(tag);
            tailored = true;
        }
        // The chosen voice when it suits the sentence; else the browser picks one for the language.
        if let Some(bv) = voice.as_deref().and_then(|id| browser_voices(&synth).into_iter().find(|bv| bv.voice_uri() == id))
            && voice_for(Some(&to_voice(&bv)), lang.as_deref(), &text).is_some()
        {
            u.set_voice(Some(&bv));
            tailored = true;
        }
    }
    u.set_rate(rate);
    let sh = Arc::clone(shared);
    let handler: Handler = Closure::new(move |e: web_sys::Event| ended(&sh, generation, index, attempt, tailored, &e));
    u.set_onend(Some(handler.as_ref().unchecked_ref()));
    u.set_onerror(Some(handler.as_ref().unchecked_ref()));
    synth.speak(&u);
    CURRENT.with(|c| *c.borrow_mut() = Some(Current { utterance: u, handler, player: Arc::downgrade(shared) }));
}

/// The utterance for sentence `index` of `generation`, spoken as `attempt` says, ended (`end`)
/// or failed (`error`).
fn ended(shared: &Arc<Player>, generation: u64, index: usize, attempt: WebAttempt, tailored: bool, e: &web_sys::Event) {
    let next = {
        let mut s = lock(&shared.0);
        if s.generation != generation || s.index != index || s.state != State::Playing {
            return;
        }
        if e.type_() == "error" {
            let code = js_sys::Reflect::get(e, &"error".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
            match web_retry(&code, attempt, tailored) {
                WebRetry::Again(again) => again,
                WebRetry::Pause => {
                    s.state = State::Paused;
                    return;
                }
                WebRetry::Stop => {
                    s.state = State::Stopped;
                    s.error = Some(format!("The browser couldn't read aloud ({code})"));
                    return;
                }
            }
        } else {
            s.index += 1;
            if s.index >= s.queue.len() {
                s.state = State::Stopped;
                return;
            }
            WebAttempt::default()
        }
    };
    speak(shared, next);
}
