# Recordings for the tests of `stt/local`

Two recorded sentences, each followed by what follows the last sentence of a dictation: a second and a half of the room and the click of the key being let go. Vinput 0.3.2 sent that end to the engine on its own, and the base model answered "Okay." for the English one and "..." for the Slovenian one, which was typed. The tests in `local.rs` hold that the end is no longer sent, and, run by hand against the engine itself, that nothing is typed for it.

| File | Sentence | Source |
| --- | --- | --- |
| `en.wav` | "It is related to but usually not involving alpine style ski touring or mountaineering, the latter ones done in steep terrain and requiring much stiffer skis and boots." | `en_us/test/678174152690230753.wav` |
| `sl.wav` | "Najdba ponuja tudi vpogled v razvoj ptičjih peres." | `sl_si/test/986130118904874446.wav` |

The recordings are from [FLEURS](https://huggingface.co/datasets/google/fleurs) (Google, CC BY 4.0), 16 kHz mono. Each was changed in one way: the quietest tenth of the recording, its room, was put before it for 0.2 s and after it for 1.6 s, followed by a click of 60 ms and another 0.25 s of the room. In the English one, recorded in a louder room, that room is at six tenths of its level.
