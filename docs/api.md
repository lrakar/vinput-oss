# Vinput local API

Vinput types what you say into the focused window, searches the web for it and records voice memos. It does nothing else. Other programs build on it through this API: they can follow whether Vinput is listening, start and stop a session, ask for a dictation's text instead of having it typed, and learn where each voice memo was saved. That is how integrations (a chat app connector, a mailer for memos, a control deck, a script) are meant to be written: outside Vinput.

## Connecting

- Windows named pipe: `\\.\pipe\vinput`
- Open it for reading and writing. Any number of clients can be connected at once.
- Each message is one JSON object on one line, UTF-8, ended by `\n` (`\r\n` is accepted), in both directions.
- A line may be at most 65,536 bytes. A longer one is answered with a `line_too_long` error and skipped up to its line break; the connection stays open.
- Up to 16 programs can be connected at once. Another one is sent a `too_many_clients` error and disconnected.
- Only programs running as the same Windows user as Vinput can connect. Remote connections are refused.
- The pipe exists while Vinput runs and the API is switched on (Settings, Advanced). If opening it fails, try again later.
- A client that leaves what Vinput sends unread for 5 seconds is disconnected, so a stuck program cannot hold its place.

Unknown fields, unknown events and unknown error codes must be ignored by both sides, so that things can be added without breaking anyone. Everything marked *since 0.2.5* below is such an addition; `protocol` stays 1.

**Reading and writing at the same time.** A pipe opened the plain way (a blocking file handle) cannot be written to while another thread of yours is blocked reading it: the write waits for the read. Either take turns (write a command, then read until its result), or open the pipe for overlapped I/O, which is what async runtimes do.

## Events (Vinput to client)

The first line after connecting is always `hello`:

```json
{"event":"hello","app":"vinput","version":"0.2.5","protocol":1,"modes":["type","type-enter","capture","search","memo"],"commands":["hello","start","stop","toggle"],"max_line":65536,"state":"idle"}
```

`protocol` changes only when an existing client would break. `modes` and `commands` list what this version understands (`commands` and `max_line` since 0.2.5). `state` is `idle` or `listening`; while listening, `mode` says in which mode (since 0.2.5).

Listening started or stopped, however it was triggered (hotkey or API), for a dictation or a memo:

```json
{"event":"state","state":"listening","mode":"capture","id":"my-42"}
{"event":"state","state":"idle"}
```

When listening starts, `mode` is the mode of the session and `id` the id of the `start` or `toggle` that began it, if it carried one (both since 0.2.5). This is how a client knows its command took effect.

A dictation started in `capture` mode has ended. `text` is everything that was said, or `""` if nothing was recognized or the user cancelled the dictation with Escape. `id` is present if the `start` that began it carried one:

```json
{"event":"dictation","text":"What is the capital of Slovenia?","id":"my-42"}
```

The text of dictations that are typed into a window is never sent over the API.

A voice memo was saved, whether a hotkey or a client started it. `path` is the MP3 file, `seconds` its length:

```json
{"event":"memo","path":"C:\\Users\\you\\Documents\\Vinput memos\\Voice memo 2026-10-05 18.42.07.mp3","seconds":12.4}
```

A command could not be carried out, or a session failed:

```json
{"event":"error","code":"unknown_mode","message":"unknown mode \"x\"; use type, type-enter, capture, search, memo","id":"my-42"}
```

`code` is for programs, `message` for people (since 0.2.5 `code` is always present). `id` is the id of the command or session the error is about, when it had one. An error about a command goes only to the client that sent it; `session_failed` goes to everyone.

| Code | Meaning |
| --- | --- |
| `invalid_json` | The line is not a JSON object, or not UTF-8. |
| `invalid_request` | `command` is missing, or a field has the wrong type: `id` must be a string of up to 128 characters, `mode` and `name` strings, `protocol` a whole number. |
| `unknown_command` | `command` names nothing Vinput knows. |
| `unknown_mode` | `mode` names nothing Vinput knows. |
| `line_too_long` | The line was over 65,536 bytes. It was skipped. |
| `unsupported_protocol` | `hello` asked for a newer `protocol` than this Vinput speaks. |
| `too_many_clients` | 16 programs are connected already. The pipe is closed after it. |
| `settings_open` | A hotkey is being set in Vinput's settings window, so no session can start right now. Try again in a moment. |
| `busy` | `start` was sent while a session is running. Nothing changed. |
| `no_model` | No speech model can be used as things stand, so only `memo` can start: none is chosen, the chosen one is not downloaded, a Google service of your own has no key or project, or VinPUT Pro is not signed in, has no subscription or has used up its hours with no model on this PC to carry on. The message says which. |
| `session_failed` | A session started but failed. `message` says why. `id` is that of the session that failed, also when another session has started or ended since. |

Events other than errors about a command go to every connected client, so use an `id` to recognize the results you asked for. A client that falls behind by more than 64 events misses the oldest ones but stays connected.

## Commands (client to Vinput)

```json
{"command":"hello","name":"My deck","protocol":1}
{"command":"start","mode":"capture","id":"my-42"}
{"command":"stop"}
{"command":"toggle","mode":"type"}
```

| Command | Effect |
| --- | --- |
| `hello` | Tells Vinput what your program is called, up to 48 characters. The name is shown under Settings, Advanced, Connected. `protocol` is the version your program speaks; a newer one than Vinput's is refused with `unsupported_protocol`. Both optional. |
| `start` | Starts a session. If one is running already, nothing changes and you get a `busy` error. |
| `stop` | Stops the running session. Does nothing if none is running. |
| `toggle` | Stops the running session, or starts one. |

| Mode | What happens to what you say |
| --- | --- |
| `type` (default) | Typed into the focused window, like the dictate hotkey. |
| `type-enter` | Typed, then Enter is pressed. |
| `capture` | Not typed. Sent to all clients as a `dictation` event when the session ends. |
| `search` | Not typed. Opened as a web search in the default browser when the session ends, with the search address set under Settings, Advanced. Nothing is sent to clients but the `state` events. Since 0.2.4; check `modes`. |
| `memo` | Not recognized. Saved as an MP3 and announced with a `memo` event. |

Every command but `hello` takes an optional `id`, a string of up to 128 characters. It comes back in the `state`, `dictation`, `memo` and `error` events of the session it starts, and in an `error` about the command itself.

A command that works has no reply of its own: the `state` event that follows, carrying your `id`, is the confirmation. A command that cannot be carried out is answered with an `error` event to you alone. A session started over the API runs until it is stopped, by `stop`, `toggle` or a press of a Vinput hotkey. For push-to-talk, send `start` on key down and `stop` on key up.

While a hotkey is being set in Vinput's settings window (the field that takes the new key has the keyboard), `start`, and a `toggle` that would start a session, are answered with a `settings_open` error; a dictation would otherwise be typed into that field. `stop` and `hello` are never refused. Up to 0.2.5 every command was answered this way for as long as the settings window was open; since 0.3.0 settings take effect as they are made, the hotkeys stay on while the window is open, and so does the API. A client that handles `settings_open` needs no change: it just sees it far less often.

## Example

A complete client in Python, with no dependencies: toggle a captured dictation and print its text.

```python
import json

with open(r"\\.\pipe\vinput", "r+b", buffering=0) as pipe:
    pipe.write(b'{"command":"toggle","mode":"capture","id":"demo"}\n')
    for line in iter(pipe.readline, b""):
        event = json.loads(line)
        if event.get("id") != "demo":
            continue
        if event["event"] == "dictation":
            print(event["text"])
            break
        if event["event"] == "error":
            print("Vinput refused:", event["message"])
            break
```

Run it to start listening, speak, and press the dictate hotkey to stop; the text is printed. Swap `capture` for `memo` and `dictation` for `memo` to record a voice memo and get its path, ready to attach to a mail or post to a messenger.

In Rust, `vinput::api::Client` does the connecting, reconnecting and parsing for you.

## The state file

`%APPDATA%\Vinput\state` still holds the word `listening` or `idle` for tools that only need to poll. The `state` event carries the same information without polling.
