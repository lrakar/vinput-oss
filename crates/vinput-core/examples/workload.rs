//! Times the processor-bound work a dictation leans on, to compare release
//! profiles: hashing a download, sealing audio for the wire, base64 of audio
//! packets, reading and writing JSON, and synthesizing the sounds.
#![allow(
    missing_docs,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::print_stdout
)]

use std::hint::black_box;
use std::time::Instant;

use base64::Engine;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::digest::{Context, SHA256};
use vinput_core::sound::{self, Cue};

fn best<T>(runs: usize, mut work: impl FnMut() -> T) -> f64 {
    let mut least = f64::MAX;
    for _ in 0..runs {
        let start = Instant::now();
        black_box(work());
        least = least.min(start.elapsed().as_secs_f64() * 1000.0);
    }
    least
}

fn main() {
    let data: Vec<u8> = (0..64 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();

    let sha = best(5, || {
        let mut context = Context::new(&SHA256);
        for chunk in data.chunks(64 * 1024) {
            context.update(chunk);
        }
        context.finish()
    });

    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &[7; 32]).unwrap());
    let mut wire = data.clone();
    let seal = best(5, || {
        for record in wire.chunks_mut(16 * 1024) {
            let nonce = Nonce::assume_unique_for_key([1; 12]);
            let tag = key
                .seal_in_place_separate_tag(nonce, Aad::empty(), record)
                .unwrap();
            let _ = black_box(tag);
        }
    });

    // Ten milliseconds of 16 kHz speech at a time, as a microphone hands it on.
    let b64 = best(5, || {
        let mut total = 0;
        for packet in data.chunks(320) {
            total += base64::engine::general_purpose::STANDARD
                .encode(packet)
                .len();
        }
        total
    });

    let rows: Vec<serde_json::Value> = (0..4000)
        .map(|i| {
            serde_json::json!({
                "id": i, "name": format!("model-{i}"), "size": i * 1000, "installed": i % 3 == 0,
                "languages": ["en", "sl", "de"], "note": "A row of the picture a window is sent, more or less",
                "progress": 0.25, "nested": {"a": [1, 2, 3], "b": null}
            })
        })
        .collect();
    let text = serde_json::to_string(&rows).unwrap();
    let json = best(20, || {
        let read: serde_json::Value = serde_json::from_str(&text).unwrap();
        serde_json::to_string(&read).unwrap().len()
    });

    let cue = best(20, || {
        let mut total = 0;
        for _ in 0..200 {
            total +=
                sound::samples(Cue::Start, 48_000).len() + sound::samples(Cue::Stop, 48_000).len();
        }
        total
    });

    println!("sha256 64 MB       {sha:9.2} ms");
    println!("aes-256-gcm 64 MB  {seal:9.2} ms");
    println!("base64 64 MB       {b64:9.2} ms");
    println!("json {:4} kB r+w    {json:9.2} ms", text.len() / 1024);
    println!("400 sounds         {cue:9.2} ms");
}
