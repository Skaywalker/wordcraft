//! Read Aloud in the browser, through the Web Speech API (`speechSynthesis`).
//!
//! The browser speaks one sentence at a time, as the desktop speech programs do: each sentence is
//! an utterance in its language, at the reading speed, in the chosen voice when it suits the
//! sentence. Its end event moves on to the next sentence; every change to the player (pause, skip,
//! stop, a new speed) cancels the utterance and speaks the current sentence again if still playing.
//! The generation and the sentence index an utterance was started for keep a late event of a
//! cancelled one from moving the player.

use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex};

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{SpeechSynthesis, SpeechSynthesisUtterance, SpeechSynthesisVoice};

use super::{Shared, State, Voice, VoiceKind, lock, plain_tag, voice_for};

type Handler = Closure<dyn FnMut(web_sys::Event)>;

thread_local! {
    /// The utterance being spoken and its event handler.
    static CURRENT: RefCell<Option<(SpeechSynthesisUtterance, Handler)>> = const { RefCell::new(None) };
    /// The previous handler, kept until the next cancel replaces it: the handler that ends a
    /// sentence is still running when it starts the next one, and must not be dropped meanwhile.
    static RETIRED: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

/// What Read Aloud says when the browser has no speech synthesis.
const UNAVAILABLE: &str = "Read Aloud isn't available in this browser";

fn synth() -> Option<SpeechSynthesis> {
    web_sys::window()?.speech_synthesis().ok()
}

/// Stop the sentence being spoken and detach its handler.
pub(super) fn cancel() {
    if let Some((u, handler)) = CURRENT.with(|c| c.borrow_mut().take()) {
        u.set_onend(None);
        u.set_onerror(None);
        RETIRED.with(|r| *r.borrow_mut() = Some(handler));
    }
    if let Some(s) = synth() {
        s.cancel();
    }
}

/// The browser's voices.
fn browser_voices(synth: &SpeechSynthesis) -> Vec<SpeechSynthesisVoice> {
    synth.get_voices().iter().filter_map(|v| v.dyn_into::<SpeechSynthesisVoice>().ok()).collect()
}

/// The browser's voices as Read Aloud lists them (the id is the voice URI).
pub(super) fn voices() -> Vec<Voice> {
    synth()
        .map(|s| browser_voices(&s))
        .unwrap_or_default()
        .iter()
        .map(|v| Voice { id: v.voice_uri(), name: v.name(), lang: v.lang(), kind: VoiceKind::Web })
        .collect()
}

/// Stop with `error`.
fn fail(shared: &Arc<(Mutex<Shared>, Condvar)>, error: String) {
    let mut s = lock(&shared.0);
    s.state = State::Stopped;
    s.error = Some(error);
}

/// After a change: cancel what is being spoken, and speak the current sentence if playing.
pub(super) fn sync(shared: &Arc<(Mutex<Shared>, Condvar)>) {
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
    if let Some(tag) = lang.as_deref().and_then(plain_tag) {
        u.set_lang(tag);
    }
    u.set_rate(rate);
    // The chosen voice when it suits the sentence; else the browser picks one for the language.
    let listed = voices();
    let chosen = voice.as_deref().and_then(|id| listed.iter().find(|v| v.id == id));
    if let Some(v) = voice_for(chosen, lang.as_deref(), &text)
        && let Some(bv) = browser_voices(&synth).into_iter().find(|bv| bv.voice_uri() == v.id)
    {
        u.set_voice(Some(&bv));
    }
    let sh = Arc::clone(shared);
    let handler: Handler = Closure::new(move |e: web_sys::Event| ended(&sh, generation, index, &e));
    u.set_onend(Some(handler.as_ref().unchecked_ref()));
    u.set_onerror(Some(handler.as_ref().unchecked_ref()));
    synth.speak(&u);
    CURRENT.with(|c| *c.borrow_mut() = Some((u, handler)));
}

/// The utterance for sentence `index` of `generation` ended (`end`) or failed (`error`).
fn ended(shared: &Arc<(Mutex<Shared>, Condvar)>, generation: u64, index: usize, e: &web_sys::Event) {
    {
        let mut s = lock(&shared.0);
        if s.generation != generation || s.index != index || s.state != State::Playing {
            return;
        }
        if e.type_() == "error" {
            let code = js_sys::Reflect::get(e, &"error".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
            // A sentence cancelled by the player itself isn't an error.
            if code == "interrupted" || code == "canceled" {
                return;
            }
            s.state = State::Stopped;
            s.error = Some(format!("The browser couldn't read aloud ({code})"));
            return;
        }
        s.index += 1;
        if s.index >= s.queue.len() {
            s.state = State::Stopped;
            return;
        }
    }
    sync(shared);
}
