//! Live-engine smoke test: exercises the real `AudioEngine` (device sink,
//! loader thread, seek worker, fades) the way the frontend drives it.
//!
//! `cargo test -- --ignored --nocapture`

use std::time::Duration;

use crate::audio::AudioEngine;

fn wait_for<F: Fn(&crate::audio::PlayerStatus) -> bool>(
    engine: &AudioEngine,
    label: &str,
    timeout: Duration,
    predicate: F,
) -> crate::audio::PlayerStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let status = engine.status();
        if predicate(&status) {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: state={} position={:.2} error={:?}",
                status.state, status.position, status.error
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test]
#[ignore = "opens the audio device and plays for real"]
async fn engine_plays_seeks_and_fades() {
    let engine = AudioEngine::global();

    println!("[engine] initial state = {}", engine.status().state);
    assert_eq!(engine.status().state, "idle");

    engine.set_normalize(true);
    engine.set_volume(0.2);
    engine
        .load("dQw4w9WgXcQ".to_string(), "low".to_string(), 0.0, true)
        .await;

    let playing = wait_for(engine, "playing", Duration::from_secs(60), |s| {
        s.state == "playing"
    });
    println!(
        "[engine] playing: duration={:.2} loadSeq={}",
        playing.duration, playing.load_seq
    );
    assert!(playing.duration > 30.0);

    std::thread::sleep(Duration::from_secs(2));
    let advanced = engine.status();
    println!("[engine] position after 2s = {:.2}", advanced.position);
    assert!(
        advanced.position > 0.5,
        "position did not advance: {}",
        advanced.position
    );

    // Seek + position bookkeeping.
    engine.seek(90.0);
    let seeked = wait_for(engine, "post-seek position", Duration::from_secs(10), |s| {
        s.position >= 88.0
    });
    println!("[engine] after seek: {:.2}", seeked.position);

    // Pause freezes the position.
    engine.pause();
    let paused = wait_for(engine, "paused", Duration::from_secs(5), |s| {
        s.state == "paused"
    });
    println!("[engine] paused at {:.2}", paused.position);
    std::thread::sleep(Duration::from_millis(700));
    let still = engine.status();
    assert!(
        (still.position - paused.position).abs() < 0.5,
        "position moved while paused: {} -> {}",
        paused.position,
        still.position
    );

    engine.play();
    wait_for(engine, "resumed", Duration::from_secs(5), |s| {
        s.state == "playing"
    });

    // Mute/unmute and a fade must not disturb transport state.
    engine.set_muted(true);
    std::thread::sleep(Duration::from_millis(120));
    engine.set_muted(false);
    engine.fade(0.5, 400);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(engine.status().state, "playing");
    println!("[engine] fade/mute left the transport playing");

    // Starting a *second* track while the first is playing must not leave the
    // engine wedged on the outgoing stream.
    engine
        .load("a5uQMwRMHcs".to_string(), "low".to_string(), 0.0, true)
        .await;
    let second = wait_for(engine, "second track", Duration::from_secs(60), |s| {
        s.state == "playing" && s.load_seq > playing.load_seq && s.position < 5.0
    });
    println!("[engine] second track loadSeq={}", second.load_seq);
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        engine.status().position > 0.5,
        "second track did not advance"
    );

    // Loading a bogus id must surface as an error status, not a panic.
    engine
        .load("aaaaaaaaaaX".to_string(), "low".to_string(), 0.0, true)
        .await;
    let errored = wait_for(engine, "error status", Duration::from_secs(60), |s| {
        s.state == "error"
    });
    println!(
        "[engine] error path: {:?} (errorSeq={})",
        errored.error, errored.error_seq
    );
    assert!(errored.error.is_some());

    // Auto-advance on track end: start near the end of a short preview.
    engine
        .load("dQw4w9WgXcQ".to_string(), "low".to_string(), 205.0, true)
        .await;
    wait_for(engine, "playing near end", Duration::from_secs(60), |s| {
        s.state == "playing"
    });
    let ended = wait_for(engine, "ended", Duration::from_secs(60), |s| {
        s.state == "ended"
    });
    println!("[engine] ended seq={}", ended.ended_seq);
    assert_eq!(ended.ended_seq, 1);
}
